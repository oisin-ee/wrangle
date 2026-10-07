import assert from "node:assert/strict";
import { test } from "node:test";
import { Engine, type Raw, type Runner } from "./engine.ts";
import { ticketFrom, withStored } from "./index.ts";
import { Queue, type QueuePort, type QueueState, type Ticket, wakeText, widgetLines } from "./queue.ts";

const HOST = { id: "local", label: "local" };
const ADMITTED = (ticket: string) => ({
	admitted: true,
	ticket,
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
