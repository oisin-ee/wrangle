// Pi extension: the `wrangle` tool. Admits, queues, and spawns child agents
// across Herdr hosts through the `wrangle` binary and Shepherdr's `agents` tool.

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Engine, EngineError } from "./engine.ts";
import {
	ENTRY_TYPE,
	isQueueState,
	MESSAGE_TYPE,
	Queue,
	type QueuePort,
	type Ticket,
	wakeText,
	widgetLines,
} from "./queue.ts";
import { hostLine, leadName, message, runSpawn, toolError, toolResult } from "./spawn.ts";
import { type Status, WrangleParameters, type WrangleParams } from "./types.ts";

export const WIDGET_ID = "wrangle";

export const HELP = `wrangle: spawn child agents without picking a host.

Calls (action defaults to spawn):
  wrangle agent_type=… label=… message=… [branch=… base=… repo=…] [machine=…]
    Admits on the host with the most headroom, runs the prepare hook when
    branch is set (a worktree and pane on that host), then Shepherdr's
    agents spawn. Returns Shepherdr's spawn result plus host and ticket.
    When every host is full it returns {queued, ticket} at once; a wrangle
    message arrives when the ticket is admitted. Then call
    wrangle ticket=<ticket>; the stored arguments are reused. Never poll.
  wrangle action=status      hosts, headroom, your queue
  wrangle action=cancel ticket=…
  wrangle action=help

Use agents (send, read, answer, watch, assign) for everything after the spawn.`;

export default function wrangle(pi: ExtensionAPI) {
	const engine = new Engine();
	let context: ExtensionContext | undefined;
	let lastCount = -1;

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
		render: (tickets) => {
			if (context?.hasUI) {
				const lines = widgetLines(tickets);
				context.ui.setWidget(WIDGET_ID, lines.length ? lines : undefined, {
					placement: "aboveEditor",
				});
			}
			const pane = process.env["HERDR_PANE_ID"];
			if (pane && tickets.length !== lastCount) {
				lastCount = tickets.length;
				void engine.queue(pane, tickets.length).catch(() => undefined);
			}
		},
	};
	const queue = new Queue(engine, port);

	pi.on("session_start", (_event, ctx) => {
		context = ctx;
		for (const entry of [...ctx.sessionManager.getBranch()].reverse()) {
			if (entry.type !== "custom" || entry.customType !== ENTRY_TYPE) continue;
			if (isQueueState(entry.data)) queue.restore(entry.data);
			break;
		}
	});
	pi.on("session_shutdown", () => queue.stop());

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
						const outcome = await runSpawn(engine, ctx, params, leadName(), signal);
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
						}
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

export function renderStatus(status: Status, mine: readonly Ticket[] = []): string {
	const lines = status.hosts.map((host) => {
		const probe = host.probe;
		const load =
			probe === undefined
				? ""
				: ` load ${probe.load1.toFixed(2)}/${probe.cores} disk ${probe.disk_free_percent.toFixed(0)}% agents ${probe.agents.total} reserved ${probe.reservations.length}`;
		return `${hostLine(host)}${load}`;
	});
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
