// The spawn flow: admit → prepare → agents spawn → mark. The engine decides;
// this file only sequences calls and shapes the tool result.

import type { AgentToolResult } from "@earendil-works/pi-coding-agent";
import { Check } from "typebox/value";
import { Engine } from "./engine.ts";
import { type Admitted, type Queued, SpawnDetails, type WrangleParams } from "./types.ts";

/** The slice of `ExtensionToolContext` the flow needs; narrow so tests can fake it. */
export interface SpawnContext {
	executeTool(
		name: string,
		args: unknown,
	): Promise<{ result: AgentToolResult<unknown>; isError: boolean }>;
}

export type Details = Record<string, unknown>;

export function toolResult(value: Details, warning?: string): AgentToolResult<Details> {
	if (warning) value = { ...value, next: warning };
	return { content: [{ type: "text", text: JSON.stringify(value) }], details: value };
}

export function toolError(message: string, details: Details = {}): AgentToolResult<Details> {
	return {
		content: [{ type: "text", text: message }],
		details: { ...details, error: message },
		isError: true,
	};
}

export function required(value: string | undefined, field: string): string {
	if (!value?.trim()) throw new Error(`${field} is required for spawn`);
	return value.trim();
}

/** The lead's identity for the ledger and the sidebar `owner` token. */
export function leadName(env: NodeJS.ProcessEnv = process.env): string {
	return env["WRANGLE_LEAD"] ?? env["HERDR_PANE_ID"] ?? `pi-${process.pid}`;
}

/** Shepherdr `agents spawn` arguments, built from the tool call and the admitted host. */
export function spawnArguments(
	params: WrangleParams,
	host: string,
	pane?: string,
): Record<string, unknown> {
	const args: Record<string, unknown> = {
		action: "spawn",
		machine: host,
		agent_type: required(params.agent_type, "agent_type"),
		message: required(params.message, "message"),
	};
	for (const key of ["name", "label", "workspace", "cwd", "base", "blocking"] as const) {
		if (params[key] !== undefined) args[key] = params[key];
	}
	if (pane) {
		args["placement"] = "pane";
		args["pane"] = pane;
	} else {
		if (params.placement !== undefined) args["placement"] = params.placement;
		if (params.pane !== undefined) args["pane"] = params.pane;
	}
	return args;
}

export interface SpawnOutcome {
	result: AgentToolResult<Details>;
	admitted?: Admitted;
	queued?: Queued;
}

/**
 * Run one spawn. Returns the tool result plus which branch was taken so the
 * caller can update its queue state without re-parsing the result.
 */
export async function runSpawn(
	engine: Engine,
	ctx: SpawnContext,
	params: WrangleParams,
	lead: string,
	signal?: AbortSignal,
): Promise<SpawnOutcome> {
	required(params.agent_type, "agent_type");
	required(params.message, "message");
	const admit = await engine.admit(
		lead,
		params.ticket
			? { ticket: params.ticket }
			: params.machine
				? { machine: params.machine }
				: {},
		signal,
	);
	if ("queued" in admit) {
		return {
			queued: admit,
			result: toolResult({
				queued: true,
				ticket: admit.ticket,
				reason: admit.reason,
				hosts: admit.hosts.map(hostLine),
				next: `No host has headroom. Keep working; a wrangle message will tell you when ticket ${admit.ticket} is admitted. Then call wrangle again with ticket=${admit.ticket} and the same arguments. Do not poll.`,
			}),
		};
	}

	const host = admit.host;
	const warnings: string[] = [];
	let pane: string | undefined;
	if (params.branch) {
		try {
			const prepared = await engine.prepare(
				{
					machine: host.id,
					branch: params.branch,
					...(params.base ? { base: params.base } : {}),
					...(params.repo ? { repo: params.repo } : {}),
				},
				signal,
			);
			pane = prepared.pane_id;
		} catch (error) {
			await engine.release(admit.ticket).catch(() => undefined);
			return { admitted: admit, result: toolError(`prepare hook failed: ${message(error)}`) };
		}
	}

	// Mark early when the pane is already known, so the sidebar shows the child
	// as soon as it starts, even for blocking spawns.
	if (pane) {
		await engine
			.mark({ pane, machine: host.id, lead, name: params.name ?? params.label ?? "child" })
			.catch((error: unknown) => warnings.push(`mark: ${message(error)}`));
	}

	const outcome = await ctx.executeTool("agents", spawnArguments(params, host.id, pane));
	if (outcome.isError) {
		await engine.release(admit.ticket).catch(() => undefined);
		return {
			admitted: admit,
			result: toolError(`agents spawn failed on ${host.label}: ${textOf(outcome.result)}`, {
				host: host.label,
			}),
		};
	}

	const details: Details = Check(SpawnDetails, outcome.result.details)
		? outcome.result.details
		: { spawned: true, raw: textOf(outcome.result) };
	const target = typeof details["target"] === "string" ? details["target"] : pane;
	if (target) {
		await engine
			.mark({
				pane: target,
				machine: host.id,
				lead,
				name: typeof details["name"] === "string" ? details["name"] : (params.label ?? "child"),
				ticket: admit.ticket,
			})
			.catch((error: unknown) => warnings.push(`mark: ${message(error)}`));
	} else {
		await engine.release(admit.ticket).catch(() => undefined);
	}
	return {
		admitted: admit,
		result: toolResult(
			{ ...details, host: host.label, ticket: admit.ticket },
			warnings.length ? warnings.join("; ") : undefined,
		),
	};
}

export function hostLine(report: {
	label: string;
	ok: boolean;
	error?: string;
	headroom?: { headroom: number; eligible: boolean; reason?: string };
}): string {
	if (!report.ok) return `${report.label}: unreachable (${report.error ?? "unknown"})`;
	const h = report.headroom;
	if (!h) return `${report.label}: no headroom data`;
	return h.eligible
		? `${report.label}: headroom ${h.headroom.toFixed(2)}`
		: `${report.label}: ${h.reason ?? "full"}`;
}

export function textOf(result: AgentToolResult<unknown>): string {
	return result.content
		.map((part) => (part.type === "text" ? part.text : "[image]"))
		.join("\n")
		.trim();
}

export function message(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}
