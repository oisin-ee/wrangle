import assert from "node:assert/strict";
import { test } from "node:test";
import { Engine, type Raw, type Runner } from "./engine.ts";
import { isLeadEntry, renderStatus, ticketFrom, withStored } from "./index.ts";
import {
	fleetLines,
	hostsLine,
	liveChildren,
	Queue,
	type QueuePort,
	type QueueState,
	type Ticket,
	wakeText,
	widgetLines,
} from "./queue.ts";

const HOST = { id: "local", label: "local" };
const ADMITTED = (ticket: string) => ({
	admitted: true,
	ticket,
	lead: "lead-1",
	host: HOST,
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
});
const QUEUED = (ticket: string) => ({ queued: true, ticket, hosts: [], reason: "all full" });

function scripted(script: Array<{ code: number; body: unknown }>) {
	const calls: string[][] = [];
	const run: Runner = (args) => {
		calls.push(args);
		const next = script.shift();
		if (!next) return Promise.reject(new Error(`unexpected call ${args.join(" ")}`));
		const raw: Raw = { code: next.code, stdout: JSON.stringify(next.body), stderr: "" };
		return Promise.resolve(raw);
	};
	return { engine: new Engine(run), calls };
}

function port() {
	const persisted: QueueState[] = [];
	const woken: Array<{ ticket: string; host: string }> = [];
	const rendered: number[] = [];
	const p: QueuePort = {
		persist: (state) => persisted.push(structuredClone(state)),
		wake: (ticket, host) => woken.push({ ticket: ticket.ticket, host }),
		render: (tickets) => rendered.push(tickets.length),
	};
	return { port: p, persisted, woken, rendered };
}

const ticket = (id: string, state: Ticket["state"] = "queued"): Ticket => ({
	ticket: id,
	params: { agent_type: "general", label: `unit-${id}`, message: "work" },
	createdAt: 1_000,
	state,
});

const FAST = { initialMs: 5, maxMs: 20 };

test("add persists, renders, and a poll that stays queued backs off", async () => {
	const fake = scripted([{ code: 1, body: QUEUED("t1") }]);
	const p = port();
	const q = new Queue(fake.engine, p.port, FAST);
	q.add(ticket("t1"));
	assert.equal(p.persisted.length, 1);
	assert.deepEqual(p.persisted[0]?.tickets.map((t) => t.ticket), ["t1"]);
	await q.poll();
	assert.deepEqual(fake.calls[0], ["admit", "--ticket", "t1"]);
	assert.equal(q.find("t1")?.state, "queued");
	assert.equal(q.find("t1")?.reason, "all full");
	assert.equal(p.woken.length, 0);
	q.stop();
});

test("a poll that is admitted keeps the reservation and wakes the lead once", async () => {
	const fake = scripted([{ code: 0, body: ADMITTED("t1") }]);
	const p = port();
	const q = new Queue(fake.engine, p.port, FAST);
	q.add(ticket("t1"));
	await q.poll();
	assert.deepEqual(p.woken, [{ ticket: "t1", host: "local" }]);
	assert.equal(q.find("t1")?.state, "admitted");
	assert.equal(q.find("t1")?.host, "local");
	// No release/cancel was issued: only the admit call happened.
	assert.deepEqual(fake.calls.map((c) => c[0]), ["admit"]);
	// Nothing left queued, so another poll is a no-op.
	await q.poll();
	assert.equal(fake.calls.length, 1);
	q.stop();
});

test("the timer polls on its own while a ticket is queued and stops when empty", async () => {
	const fake = scripted([
		{ code: 1, body: QUEUED("t1") },
		{ code: 0, body: ADMITTED("t1") },
	]);
	const p = port();
	const q = new Queue(fake.engine, p.port, FAST);
	q.add(ticket("t1"));
	await new Promise((resolve) => setTimeout(resolve, 60));
	assert.equal(p.woken.length, 1, `calls: ${fake.calls.length}`);
	const settled = fake.calls.length;
	await new Promise((resolve) => setTimeout(resolve, 40));
	assert.equal(fake.calls.length, settled, "no polling after admission");
	q.stop();
});

test("an unknown ticket is dropped instead of retried forever", async () => {
	const fake = scripted([{ code: 2, body: { error: "unknown ticket t9" } }]);
	const p = port();
	const q = new Queue(fake.engine, p.port, FAST);
	q.add(ticket("t9"));
	await q.poll();
	assert.equal(q.find("t9"), undefined);
	q.stop();
});

test("restore takes the persisted state and polls the oldest queued ticket first", async () => {
	const fake = scripted([{ code: 1, body: QUEUED("t1") }]);
	const p = port();
	const q = new Queue(fake.engine, p.port, FAST);
	q.restore({ version: 1, tickets: [ticket("t1"), ticket("t2", "admitted")] });
	assert.deepEqual(q.list().map((t) => t.ticket), ["t1", "t2"]);
	await q.poll();
	assert.deepEqual(fake.calls[0], ["admit", "--ticket", "t1"]);
	q.stop();
});

test("remove and requeue update the persisted state", () => {
	const fake = scripted([]);
	const p = port();
	const q = new Queue(fake.engine, p.port, FAST);
	q.add(ticket("t1", "admitted"));
	q.requeue("t1", "full again");
	assert.equal(q.find("t1")?.state, "queued");
	assert.equal(q.remove("t1"), true);
	assert.equal(q.remove("t1"), false);
	assert.deepEqual(p.persisted.at(-1)?.tickets, []);
	q.stop();
});

test("wake text and widget lines name the ticket and the label", () => {
	const t = ticket("w-1-aaaa");
	assert.match(wakeText(t, "netcup"), /ticket w-1-aaaa \(unit-w-1-aaaa\) is admitted on netcup/);
	assert.match(wakeText(t, "netcup"), /wrangle ticket=w-1-aaaa/);
	assert.deepEqual(widgetLines([], 5_000), []);
	assert.deepEqual(widgetLines([t], 11_000), ["wrangle w-1-aaaa unit-w-1-aaaa: queued 10s"]);
	assert.deepEqual(
		widgetLines(
			[{ ...t, reason: "local: load 5 + 0 reserved ≥ 1.5 × 12 cores; netcup: disk 13.4% free < 15.0%; momokaya-2: ssh momokaya-2-dev: exit 127" }],
			11_000,
		),
		["wrangle w-1-aaaa unit-w-1-aaaa: queued 10s · local full, netcup disk, momokaya-2 unreachable"],
	);
	assert.deepEqual(widgetLines([{ ...t, state: "admitted", host: "netcup" }], 11_000), [
		"wrangle w-1-aaaa unit-w-1-aaaa: admitted on netcup",
	]);
});

test("withStored fills missing arguments from the ticket; explicit ones win", () => {
	const stored = ticketFrom(
		{ action: "spawn", agent_type: "general", label: "a", message: "m", branch: "b" },
		"t1",
		"full",
	);
	assert.deepEqual(stored.params, { agent_type: "general", label: "a", message: "m", branch: "b" });
	assert.deepEqual(withStored({ ticket: "t1" }, stored), {
		ticket: "t1",
		agent_type: "general",
		label: "a",
		message: "m",
		branch: "b",
	});
	assert.equal(withStored({ ticket: "t1", message: "new" }, stored).message, "new");
	assert.deepEqual(withStored({ ticket: "t1" }, undefined), { ticket: "t1" });
});

test("fleetLines: lead header with children, queue, and host headroom; empty without lead or tickets", () => {
	const hosts = [
		{ id: "local", label: "local", ok: true, headroom: { host: "local", headroom: 5.67, eligible: true, live_agents: 6 } },
		{ id: "8103", label: "netcup", ok: true, headroom: { host: "8103", headroom: -1, eligible: false, live_agents: 9 } },
		{ id: "08c2", label: "momokaya-2", ok: false, error: "ssh" },
	];
	assert.equal(hostsLine(hosts), "local 5.7 netcup full momokaya-2 down");
	const lead = {
		lead: "lead:rondo",
		repo: "rondo",
		pane: "w42:p1",
		children: [
			{ pane: "w42:pX", host: "local", name: "a", status: "working" },
			{ pane: "w3:p2", host: "netcup", name: "b", status: "idle" },
			{ pane: "w42:pY", host: "local", name: "c", status: "working" },
		],
	};
	assert.deepEqual(fleetLines({ lead, hosts }, []), [
		"⌂ rondo · 3 children: 1 idle 2 working · 0 queued · local 5.7 netcup full momokaya-2 down",
	]);
	assert.equal(liveChildren({ lead, hosts }), 3);
	assert.equal(
		fleetLines({ lead: { ...lead, children: [] }, hosts: [] }, [])[0],
		"⌂ rondo · no children · 0 queued",
	);
	assert.deepEqual(fleetLines({ hosts }, []), []);
	assert.deepEqual(fleetLines(undefined, []), []);
	const t: Ticket = {
		ticket: "w-1",
		params: { label: "unit" },
		createdAt: 1_000,
		state: "queued",
	};
	assert.deepEqual(fleetLines(undefined, [t], 11_000), ["⌂ wrangle · 1 queued", "wrangle w-1 unit: queued 10s"]);
});

test("renderStatus lists leads with children before the hosts", () => {
	const text = renderStatus({
		leads: [
			{
				lead: "lead:rondo",
				repo: "rondo",
				pane: "w42:p1",
				host: "local",
				status: "working",
				children: [{ pane: "w42:pX", host: "local", name: "kid", status: "idle" }],
			},
			{ lead: "w42:pP", children: [] },
		],
		hosts: [{ id: "local", label: "local", ok: false, error: "x" }],
		queue: [],
	});
	const lines = text.split("\n");
	assert.equal(lines[0], "⌂ rondo w42:p1 on local (working) · 1 children");
	assert.equal(lines[1], "  ↳ kid idle w42:pX on local");
	assert.equal(lines[2], "? w42:pP (no lead pane) · 0 children");
	assert.match(lines[3] ?? "", /^local: unreachable/);
	assert.equal(renderStatus({ hosts: [], queue: [] }).split("\n")[0], "leads: none");
	assert.equal(isLeadEntry({ lead: "lead:rondo" }), true);
	assert.equal(isLeadEntry({}), false);
});
