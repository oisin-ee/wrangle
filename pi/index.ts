// Pi extension: the `wrangle` tool. Admits, queues, and spawns child agents
// across Herdr hosts through the `wrangle` binary and Shepherdr's `agents` tool.

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Engine, EngineError } from "./engine.ts";
import {
	ENTRY_TYPE,
	type FleetView,
	fleetLines,
	isQueueState,
	liveChildren,
	MESSAGE_TYPE,
	Queue,
	type QueuePort,
	type Ticket,
	wakeText,
} from "./queue.ts";
import { hostLine, message, runSpawn, self, toolError, toolResult } from "./spawn.ts";
import { type Status, WrangleParameters, type WrangleParams } from "./types.ts";

export const WIDGET_ID = "wrangle";
/** Session entry that remembers this session's lead, so a resumed lead shows its widget. */
export const LEAD_ENTRY = "wrangle-lead";
/** How often the widget re-reads the fleet while children run or tickets wait. */
export const FLEET_REFRESH_MS = 60_000;

export const HELP = `wrangle: spawn child agents without picking a host.

Calls (action defaults to spawn):
  wrangle agent_type=… label=… message=… [branch=… base=… repo=…] [machine=…]
    Claims this repo's lead for your session (⌂ <repo> in the sidebar).
    One lead per repo: when another session holds it, the call returns
    lead_exists with that pane. Then send the unit to that lead with
    agents send; pass take_over=true only when the user asks.
    Admits on the host with the most headroom, runs the prepare hook when
    branch is set (a worktree and pane on that host), else opens a tab in
    your workspace. Returns Shepherdr's spawn result plus host and ticket.
    Never blocks: the call returns once the child has its task; its
    completion arrives later as a message. Keep working meanwhile.
    When every host is full it returns {queued, ticket} at once; a wrangle
    message arrives when the ticket is admitted. Then call
    wrangle ticket=<ticket>; the stored arguments are reused. Never poll.
  wrangle action=status      leads and children, hosts, headroom, your queue
  wrangle action=cancel ticket=…
  wrangle action=help

Use agents (send, read, answer, watch, assign) for everything after the spawn.`;

export default function wrangle(pi: ExtensionAPI) {
	const engine = new Engine();
	let context: ExtensionContext | undefined;
	let lastCount = -1;
	let leadId: string | undefined;
	let view: FleetView | undefined;
	let tickets: readonly Ticket[] = [];
	let fleetTimer: ReturnType<typeof setTimeout> | undefined;

	const paint = () => {
		if (!context?.hasUI) return;
		const lines = fleetLines(view, tickets);
		context.ui.setWidget(WIDGET_ID, lines.length ? lines : undefined, { placement: "aboveEditor" });
	};

	/** Take a fresh status: this session's lead and the hosts. */
	const absorb = (status: Status) => {
		const pane = process.env["HERDR_PANE_ID"];
		const leads = status.leads ?? [];
		const lead =
			leads.find((l) => pane !== undefined && l.pane === pane) ??
			(leadId ? leads.find((l) => l.lead === leadId) : undefined);
		view = lead ? { lead, hosts: status.hosts } : { hosts: status.hosts };
		paint();
	};

	const stopFleet = () => {
		if (fleetTimer) clearTimeout(fleetTimer);
		fleetTimer = undefined;
	};

	const scheduleFleet = () => {
		stopFleet();
		if (liveChildren(view) === 0 && tickets.length === 0) return;
		fleetTimer = setTimeout(() => void refresh(), FLEET_REFRESH_MS);
		fleetTimer.unref?.();
	};

	async function refresh(): Promise<void> {
		try {
			absorb(await engine.status());
		} catch {
			// Keep the last view; the next tick tries again.
		} finally {
			scheduleFleet();
		}
	}

	const port: QueuePort = {
		persist: (state) => pi.appendEntry(ENTRY_TYPE, state),
		wake: (ticket, host) => {
			pi.sendMessage(
				{
					customType: MESSAGE_TYPE,
					content: wakeText(ticket, host),
					display: true,
					details: { ticket: ticket.ticket, host },
				},
				{ triggerTurn: true, deliverAs: "followUp" },
			);
		},
		render: (current) => {
			tickets = current;
			paint();
			const pane = process.env["HERDR_PANE_ID"];
			if (pane && current.length !== lastCount) {
				lastCount = current.length;
				void engine.queue(pane, current.length).catch(() => undefined);
			}
		},
	};
	const queue = new Queue(engine, port);

	pi.on("session_start", (_event, ctx) => {
		context = ctx;
		let restored = false;
		for (const entry of [...ctx.sessionManager.getBranch()].reverse()) {
			if (entry.type !== "custom") continue;
			if (entry.customType === LEAD_ENTRY && leadId === undefined && isLeadEntry(entry.data)) {
				leadId = entry.data.lead;
			}
			if (entry.customType === ENTRY_TYPE && !restored && isQueueState(entry.data)) {
				queue.restore(entry.data);
				restored = true;
			}
		}
		// Only a session that has used wrangle pays for a fleet probe.
		if (leadId !== undefined || tickets.length > 0) void refresh();
	});
	pi.on("session_shutdown", () => {
		queue.stop();
		stopFleet();
	});

	pi.registerTool({
		name: "wrangle",
		label: "Wrangle",
		description:
			"Spawn a child agent on whichever Herdr host has headroom; queues a ticket when all are full. Same arguments as agents spawn plus branch/base/repo.",
		promptSnippet: "wrangle: spawn or queue a child agent across hosts; agents for follow-up",
		parameters: WrangleParameters,
		executionMode: "sequential",
		async execute(_id, input: WrangleParams, signal, _onUpdate, ctx) {
			context = ctx;
			const action = input.action ?? "spawn";
			try {
				switch (action) {
					case "help":
						return toolResult({ help: HELP });
					case "status": {
						const status = await engine.status(signal);
						port.render(queue.list());
						absorb(status);
						scheduleFleet();
						return {
							content: [{ type: "text", text: renderStatus(status, queue.list()) }],
							details: status as unknown as Record<string, unknown>,
						};
					}
					case "cancel": {
						if (!input.ticket) return toolError("cancel needs ticket");
						const out = await engine.cancel(input.ticket, signal);
						queue.remove(input.ticket);
						return toolResult(out as unknown as Record<string, unknown>);
					}
					case "spawn": {
						const params = withStored(input, input.ticket ? queue.find(input.ticket) : undefined);
						const outcome = await runSpawn(engine, ctx, params, self(), signal);
						const label = params.label ?? params.name ?? params.agent_type ?? "child";
						if (outcome.claimed && outcome.claimed.lead !== leadId) {
							leadId = outcome.claimed.lead;
							pi.appendEntry(LEAD_ENTRY, { lead: leadId, repo: outcome.claimed.repo });
						}
						if (outcome.leadExists) {
							void engine.notify(
								"wrangle: lead exists",
								`${label} not spawned: ${outcome.leadExists.repo} is led from ${outcome.leadExists.pane}`,
							);
						}
						if (outcome.queued) {
							const existing = queue.find(outcome.queued.ticket);
							if (existing) queue.requeue(existing.ticket, outcome.queued.reason);
							else {
								queue.add(ticketFrom(params, outcome.queued.ticket, outcome.queued.reason));
								void engine.notify(
									"wrangle: queued",
									`${params.label ?? params.agent_type ?? "child"} waits for headroom (${outcome.queued.ticket})`,
								);
							}
						} else if (outcome.admitted) {
							queue.remove(outcome.admitted.ticket);
							if (!outcome.result.isError) {
								const a = outcome.admitted;
								void engine.notify(
									"wrangle: spawned",
									`${label} → ${a.host.label} (headroom ${a.headroom.headroom.toFixed(1)})`,
								);
							}
						}
						// Show the new child (or the queued ticket) without waiting for the timer.
						void refresh();
						return outcome.result;
					}
				}
			} catch (error) {
				if (error instanceof EngineError && error.code === "missing") {
					return toolError(error.message);
				}
				return toolError(`wrangle ${action}: ${message(error)}`);
			}
		},
	});
}

/** Explicit arguments win; a stored ticket fills in the rest. */
export function withStored(input: WrangleParams, stored: Ticket | undefined): WrangleParams {
	if (!stored) return input;
	const merged: WrangleParams = { ...stored.params, ...input };
	for (const key of Object.keys(input) as (keyof WrangleParams)[]) {
		if (input[key] === undefined) delete merged[key];
	}
	return merged;
}

export function ticketFrom(params: WrangleParams, ticket: string, reason: string): Ticket {
	const { action: _action, ticket: _ticket, ...rest } = params;
	return { ticket, params: rest, createdAt: Date.now(), state: "queued", reason };
}

export function isLeadEntry(value: unknown): value is { lead: string; repo?: string } {
	return (
		typeof value === "object" &&
		value !== null &&
		typeof (value as { lead?: unknown }).lead === "string"
	);
}

export function renderStatus(status: Status, mine: readonly Ticket[] = []): string {
	const leadLines = (status.leads ?? []).flatMap((l) => {
		const head = l.pane
			? `⌂ ${l.repo ?? l.lead} ${l.pane} on ${l.host ?? "?"} (${l.status ?? "unknown"})`
			: `? ${l.lead} (no lead pane)`;
		return [
			`${head} · ${l.children.length} children`,
			...l.children.map((c) => `  ↳ ${c.name} ${c.status} ${c.pane} on ${c.host}`),
		];
	});
	const lines = status.hosts.map((host) => {
		const probe = host.probe;
		const load =
			probe === undefined
				? ""
				: ` load ${probe.load1.toFixed(2)}/${probe.cores} disk ${probe.disk_free_percent.toFixed(0)}% agents ${probe.agents.total} reserved ${probe.reservations.length}`;
		return `${hostLine(host)}${load}`;
	});
	lines.unshift(...(leadLines.length ? leadLines : ["leads: none"]));
	if (status.queue.length === 0) lines.push("queue: empty");
	const own = new Set(mine.map((t) => t.ticket));
	for (const entry of status.queue) {
		const age = Math.round(entry.age_ms / 1000);
		const where = entry.host ? ` on ${entry.host}` : "";
		const who = own.has(entry.ticket) ? "yours" : entry.lead;
		lines.push(`queue: ${entry.ticket} ${entry.state}${where} (${who}, ${age}s)`);
	}
	return lines.join("\n");
}
