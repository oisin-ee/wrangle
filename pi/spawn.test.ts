import assert from "node:assert/strict";
import { test } from "node:test";
import { Engine, type Raw, type Runner } from "./engine.ts";
import { leadName, runSpawn, type SpawnContext, spawnArguments } from "./spawn.ts";

const HOST = { id: "8103d2b65147790e4ef79e26e31c6911", label: "netcup", target: "netcup-dev" };
const PROBE = {
	host: HOST.id,
	label: "netcup",
	load1: 2,
	cores: 16,
	disk_free_percent: 30,
	agents: { total: 3, by_status: { working: 3 } },
	reservations: [],
};
const ADMITTED = {
	admitted: true,
	ticket: "w-1-aaaa",
	host: HOST,
	probe: PROBE,
	headroom: { host: HOST.id, headroom: 22, eligible: true, live_agents: 3 },
};
const QUEUED = {
	queued: true,
	ticket: "w-2-bbbb",
	hosts: [
		{
			...HOST,
			ok: true,
			probe: PROBE,
			headroom: { host: HOST.id, headroom: -1, eligible: false, reason: "load", live_agents: 9 },
		},
		{ id: "local", label: "local", ok: false, error: "boom" },
	],
	reason: "no host has headroom",
};

function fakeEngine(bodies: Record<string, { code?: number; body: unknown }>) {
	const calls: string[][] = [];
	const run: Runner = (args) => {
		calls.push(args);
		const entry = bodies[args[0] ?? ""];
		if (!entry) return Promise.reject(new Error(`no fake for ${args.join(" ")}`));
		const raw: Raw = { code: entry.code ?? 0, stdout: JSON.stringify(entry.body), stderr: "" };
		return Promise.resolve(raw);
	};
	return { engine: new Engine(run), calls };
}

function fakeCtx(result: { details: unknown; isError?: boolean }) {
	const calls: Array<{ name: string; args: unknown }> = [];
	const ctx: SpawnContext = {
		executeTool(name, args) {
			calls.push({ name, args });
			return Promise.resolve({
				isError: result.isError ?? false,
				result: {
					content: [{ type: "text" as const, text: JSON.stringify(result.details) }],
					details: result.details,
				},
			});
		},
	};
	return { ctx, calls };
}

const PARAMS = { agent_type: "general", label: "unit-a", message: "do the thing" };

test("spawnArguments maps the call onto agents spawn and pins the prepared pane", () => {
	const args = spawnArguments(
		{ ...PARAMS, blocking: false, placement: "new_tab", cwd: "/x" },
		"local",
		"w1:p2",
	);
	assert.deepEqual(args, {
		action: "spawn",
		machine: "local",
		agent_type: "general",
		message: "do the thing",
		label: "unit-a",
		cwd: "/x",
		blocking: false,
		placement: "pane",
		pane: "w1:p2",
	});
	assert.equal(spawnArguments({ ...PARAMS, placement: "new_tab" }, "local")["placement"], "new_tab");
	assert.throws(() => spawnArguments({ label: "x" }, "local"), /agent_type is required/);
});

test("admitted path: admit → agents spawn on the host → mark with the ticket", async () => {
	const fake = fakeEngine({
		admit: { body: ADMITTED },
		mark: { body: { marked: true, pane: "w7:p1", ticket: "w-1-aaaa" } },
	});
	const spawned = { spawned: true, machine: HOST.id, target: "w7:p1", name: "unit-a", status: "working" };
	const ctx = fakeCtx({ details: spawned });
	const out = await runSpawn(fake.engine, ctx.ctx, PARAMS, "lead-1");
	assert.equal(out.admitted?.ticket, "w-1-aaaa");
	assert.equal(ctx.calls.length, 1);
	assert.equal((ctx.calls[0]?.args as { machine: string }).machine, HOST.id);
	assert.deepEqual(fake.calls.map((c) => c[0]), ["admit", "mark"]);
	assert.deepEqual(fake.calls[1], [
		"mark", "--pane", "w7:p1", "--lead", "lead-1", "--name", "unit-a",
		"--machine", HOST.id, "--ticket", "w-1-aaaa",
	]);
	assert.deepEqual(out.result.details, { ...spawned, host: "netcup", ticket: "w-1-aaaa" });
	assert.equal(out.result.isError, undefined);
});

test("branch set: prepare runs on the admitted host and the pane is pinned", async () => {
	const fake = fakeEngine({
		admit: { body: ADMITTED },
		prepare: { body: { pane_id: "w9:p3", workspace_id: "w9", host: HOST } },
		mark: { body: { marked: true, pane: "w9:p3" } },
	});
	const ctx = fakeCtx({ details: { spawned: true, machine: HOST.id, target: "w9:p3", name: "unit-a" } });
	const out = await runSpawn(
		fake.engine,
		ctx.ctx,
		{ ...PARAMS, branch: "feat/a", base: "main", repo: "/repo" },
		"lead-1",
	);
	assert.deepEqual(fake.calls[1], [
		"prepare", "--branch", "feat/a", "--machine", HOST.id, "--base", "main", "--repo", "/repo",
	]);
	const spawnArgs = ctx.calls[0]?.args as Record<string, unknown>;
	assert.equal(spawnArgs["placement"], "pane");
	assert.equal(spawnArgs["pane"], "w9:p3");
	// Marked once before the spawn (pane known) and once after (ticket attached).
	assert.deepEqual(fake.calls.map((c) => c[0]), ["admit", "prepare", "mark", "mark"]);
	assert.equal(out.result.details?.["ticket"], "w-1-aaaa");
});

test("queued: returns the ticket at once and never calls agents", async () => {
	const fake = fakeEngine({ admit: { code: 1, body: QUEUED } });
	const ctx = fakeCtx({ details: {} });
	const out = await runSpawn(fake.engine, ctx.ctx, PARAMS, "lead-1");
	assert.equal(out.queued?.ticket, "w-2-bbbb");
	assert.equal(ctx.calls.length, 0);
	const details = out.result.details ?? {};
	assert.equal(details["queued"], true);
	assert.deepEqual(details["hosts"], ["netcup: load", "local: unreachable (boom)"]);
	assert.match(String(details["next"]), /ticket=w-2-bbbb/);
});

test("agents spawn failure releases the reservation and reports isError", async () => {
	const fake = fakeEngine({
		admit: { body: ADMITTED },
		release: { body: { released: 1, dequeued: false } },
	});
	const ctx = fakeCtx({ details: { error: "profile not found" }, isError: true });
	const out = await runSpawn(fake.engine, ctx.ctx, PARAMS, "lead-1");
	assert.equal(out.result.isError, true);
	assert.deepEqual(fake.calls.map((c) => c[0]), ["admit", "release"]);
	assert.match(String(out.result.details?.["error"]), /netcup/);
});

test("prepare failure releases the reservation", async () => {
	const fake = fakeEngine({
		admit: { body: ADMITTED },
		prepare: { code: 2, body: { error: "prepare hook printed no pane_id" } },
		release: { body: { released: 1, dequeued: false } },
	});
	const ctx = fakeCtx({ details: {} });
	const out = await runSpawn(fake.engine, ctx.ctx, { ...PARAMS, branch: "b" }, "lead-1");
	assert.equal(out.result.isError, true);
	assert.deepEqual(fake.calls.map((c) => c[0]), ["admit", "prepare", "release"]);
	assert.equal(ctx.calls.length, 0);
});

test("leadName prefers WRANGLE_LEAD, then the Herdr pane", () => {
	assert.equal(leadName({ WRANGLE_LEAD: "me", HERDR_PANE_ID: "w1:p1" }), "me");
	assert.equal(leadName({ HERDR_PANE_ID: "w1:p1" }), "w1:p1");
	assert.match(leadName({}), /^pi-\d+$/);
});
