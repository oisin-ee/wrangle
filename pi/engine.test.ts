import assert from "node:assert/strict";
import { test } from "node:test";
import { Engine, EngineError, type Raw, type Runner } from "./engine.ts";

function runner(responses: Record<string, Raw>): { run: Runner; calls: string[][] } {
	const calls: string[][] = [];
	const run: Runner = (args) => {
		calls.push(args);
		const key = args[0] ?? "";
		const raw = responses[key];
		if (!raw) return Promise.reject(new Error(`no fake for ${key}`));
		return Promise.resolve(raw);
	};
	return { run, calls };
}

const ADMITTED = {
	admitted: true,
	ticket: "w-1-aaaa",
	host: { id: "local", label: "local" },
	probe: {
		host: "local",
		label: "local",
		load1: 1,
		cores: 8,
		disk_free_percent: 40,
		agents: { total: 0, by_status: {} },
		reservations: [],
	},
	headroom: { host: "local", headroom: 11, eligible: true, live_agents: 0 },
};

test("admit parses exit 0 as Admitted and passes --lead/--machine", async () => {
	const fake = runner({ admit: { code: 0, stdout: JSON.stringify(ADMITTED), stderr: "" } });
	const out = await new Engine(fake.run).admit("lead-1", { machine: "netcup" });
	assert.equal("admitted" in out, true);
	assert.deepEqual(fake.calls[0], ["admit", "--lead", "lead-1", "--machine", "netcup"]);
});

test("admit with a ticket does not resend --lead", async () => {
	const fake = runner({ admit: { code: 0, stdout: JSON.stringify(ADMITTED), stderr: "" } });
	await new Engine(fake.run).admit("lead-1", { ticket: "w-1-aaaa" });
	assert.deepEqual(fake.calls[0], ["admit", "--ticket", "w-1-aaaa"]);
});

test("admit parses exit 1 as Queued", async () => {
	const queued = { queued: true, ticket: "w-2-bbbb", hosts: [], reason: "all full" };
	const fake = runner({ admit: { code: 1, stdout: JSON.stringify(queued), stderr: "" } });
	const out = await new Engine(fake.run).admit("lead-1", {});
	assert.equal("queued" in out && out.ticket, "w-2-bbbb");
});

test("exit 2 with {error} becomes an EngineError", async () => {
	const fake = runner({
		cancel: { code: 2, stdout: JSON.stringify({ error: "unknown ticket w-9" }), stderr: "" },
	});
	await assert.rejects(
		() => new Engine(fake.run).cancel("w-9"),
		(error: unknown) =>
			error instanceof EngineError &&
			error.code === "failed" &&
			error.message === "wrangle cancel: unknown ticket w-9",
	);
});

test("output that fails the schema is reported as invalid", async () => {
	const fake = runner({ status: { code: 0, stdout: '{"hosts": "nope"}', stderr: "" } });
	await assert.rejects(
		() => new Engine(fake.run).status(),
		(error: unknown) => error instanceof EngineError && error.code === "invalid",
	);
});

test("a missing binary gives the install hint", async () => {
	process.env["WRANGLE_BIN"] = "/nonexistent/wrangle-missing";
	const { runBinary, INSTALL_HINT } = await import(`./engine.ts?missing=${Date.now()}`);
	await assert.rejects(
		() => runBinary(["status"]),
		(error: unknown) =>
			error instanceof Error &&
			error.message === INSTALL_HINT &&
			(error as { code?: string }).code === "missing",
	);
	delete process.env["WRANGLE_BIN"];
});
