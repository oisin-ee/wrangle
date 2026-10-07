// Pi extension: the `wrangle` tool. Admits, queues, and spawns child agents
// across Herdr hosts through the `wrangle` binary and Shepherdr's `agents` tool.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Engine, EngineError } from "./engine.ts";
import { hostLine, leadName, message, runSpawn, toolError, toolResult } from "./spawn.ts";
import { type Status, WrangleParameters, type WrangleParams } from "./types.ts";

export const HELP = `wrangle: spawn child agents without picking a host.

Calls (action defaults to spawn):
  wrangle agent_type=… label=… message=… [branch=… base=… repo=…] [machine=…]
    Admits on the host with the most headroom, runs the prepare hook when
    branch is set (a worktree and pane on that host), then Shepherdr's
    agents spawn. Returns Shepherdr's spawn result plus host and ticket.
    When every host is full it returns {queued, ticket} at once; a wrangle
    message arrives when the ticket is admitted. Then call
    wrangle ticket=<ticket> with the same arguments. Never poll.
  wrangle action=status      hosts, headroom, your queue
  wrangle action=cancel ticket=…
  wrangle action=help

Use agents (send, read, answer, watch, assign) for everything after the spawn.`;

export default function wrangle(pi: ExtensionAPI) {
	const engine = new Engine();

	pi.registerTool({
		name: "wrangle",
		label: "Wrangle",
		description:
			"Spawn a child agent on whichever Herdr host has headroom; queues a ticket when all are full. Same arguments as agents spawn plus branch/base/repo.",
		promptSnippet: "wrangle: spawn or queue a child agent across hosts; agents for follow-up",
		parameters: WrangleParameters,
		executionMode: "sequential",
		async execute(_id, params: WrangleParams, signal, _onUpdate, ctx) {
			const action = params.action ?? "spawn";
			try {
				switch (action) {
					case "help":
						return toolResult({ help: HELP });
					case "status": {
						const status = await engine.status(signal);
						return {
							content: [{ type: "text", text: renderStatus(status) }],
							details: status as unknown as Record<string, unknown>,
						};
					}
					case "cancel": {
						if (!params.ticket) return toolError("cancel needs ticket");
						const out = await engine.cancel(params.ticket, signal);
						return toolResult(out as unknown as Record<string, unknown>);
					}
					case "spawn": {
						const outcome = await runSpawn(engine, ctx, params, leadName(), signal);
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

export function renderStatus(status: Status): string {
	const lines = status.hosts.map((host) => {
		const probe = host.probe;
		const load =
			probe === undefined
				? ""
				: ` load ${probe.load1.toFixed(2)}/${probe.cores} disk ${probe.disk_free_percent.toFixed(0)}% agents ${probe.agents.total} reserved ${probe.reservations.length}`;
		return `${hostLine(host)}${load}`;
	});
	if (status.queue.length === 0) lines.push("queue: empty");
	for (const entry of status.queue) {
		const age = Math.round(entry.age_ms / 1000);
		const where = entry.host ? ` on ${entry.host}` : "";
		lines.push(`queue: ${entry.ticket} ${entry.state}${where} (${entry.lead}, ${age}s)`);
	}
	return lines.join("\n");
}
