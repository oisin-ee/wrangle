// Typed process boundary over the `wrangle` binary. No decisions live here:
// every call is `wrangle <args> --json`, parsed and checked against a schema.

import { execFile } from "node:child_process";
import type { Static, TSchema } from "typebox";
import { Check } from "typebox/value";
import {
	AdmitOutput,
	Cancelled,
	Failure,
	Marked,
	Prepared,
	Released,
	Reported,
	Status,
} from "./types.ts";

export const BINARY = process.env["WRANGLE_BIN"] ?? "wrangle";

export const INSTALL_HINT =
	"wrangle binary not found on PATH. Install it with `mise use -g github:oisin-ee/wrangle@latest` " +
	"or `cargo install --git https://github.com/oisin-ee/wrangle`.";

export type EngineErrorCode = "missing" | "failed" | "invalid";

export class EngineError extends Error {
	readonly code: EngineErrorCode;
	readonly exitCode: number | undefined;

	constructor(message: string, code: EngineErrorCode, exitCode?: number) {
		super(message);
		this.name = "EngineError";
		this.code = code;
		this.exitCode = exitCode;
	}
}

export interface Raw {
	code: number;
	stdout: string;
	stderr: string;
}

export type Runner = (args: string[], signal?: AbortSignal) => Promise<Raw>;

/** Default runner: spawn the binary with `--json` appended. */
export const runBinary: Runner = (args, signal) =>
	new Promise((resolve, reject) => {
		execFile(
			BINARY,
			[...args, "--json"],
			{ signal, maxBuffer: 8 * 1024 * 1024, env: process.env },
			(error, stdout, stderr) => {
				if (error && !("code" in error && typeof error.code === "number")) {
					const err = error as NodeJS.ErrnoException;
					if (err.code === "ENOENT") {
						reject(new EngineError(INSTALL_HINT, "missing"));
						return;
					}
					reject(new EngineError(err.message, "failed"));
					return;
				}
				const code = error ? (error.code as number) : 0;
				resolve({ code, stdout: String(stdout), stderr: String(stderr) });
			},
		);
	});

export class Engine {
	private readonly run: Runner;

	constructor(run: Runner = runBinary) {
		this.run = run;
	}

	/** Run and parse. Exit 0 and exit 1 both carry a JSON body; exit 2 carries `{error}`. */
	async exec<T extends TSchema>(
		schema: T,
		args: string[],
		signal?: AbortSignal,
	): Promise<Static<T>> {
		const raw = await this.run(args, signal);
		const parsed = parseJson(raw.stdout);
		if (raw.code !== 0 && raw.code !== 1) {
			const detail =
				parsed !== undefined && Check(Failure, parsed)
					? parsed.error
					: raw.stderr.trim() || raw.stdout.trim() || `exit ${raw.code}`;
			throw new EngineError(`wrangle ${args[0] ?? ""}: ${detail}`, "failed", raw.code);
		}
		if (parsed === undefined || !Check(schema, parsed)) {
			throw new EngineError(
				`wrangle ${args[0] ?? ""}: unexpected output: ${raw.stdout.trim().slice(0, 400)}`,
				"invalid",
				raw.code,
			);
		}
		return parsed;
	}

	/** A ticket re-admits an existing queue entry; otherwise a lead opens a new one. */
	admit(options: { lead?: string; machine?: string; ticket?: string }, signal?: AbortSignal) {
		const args = ["admit"];
		if (options.ticket) args.push("--ticket", options.ticket);
		else if (options.lead) args.push("--lead", options.lead);
		else throw new EngineError("admit needs a lead or a ticket", "invalid");
		if (options.machine) args.push("--machine", options.machine);
		return this.exec(AdmitOutput, args, signal);
	}

	release(ticket: string, signal?: AbortSignal) {
		return this.exec(Released, ["release", "--ticket", ticket], signal);
	}

	cancel(ticket: string, signal?: AbortSignal) {
		return this.exec(Cancelled, ["cancel", "--ticket", ticket], signal);
	}

	status(signal?: AbortSignal) {
		return this.exec(Status, ["status"], signal);
	}

	prepare(
		options: { machine?: string; branch: string; base?: string; repo?: string },
		signal?: AbortSignal,
	) {
		const args = ["prepare", "--branch", options.branch];
		if (options.machine) args.push("--machine", options.machine);
		if (options.base) args.push("--base", options.base);
		if (options.repo) args.push("--repo", options.repo);
		return this.exec(Prepared, args, signal);
	}

	mark(
		options: { pane: string; machine?: string; lead: string; name: string; ticket?: string },
		signal?: AbortSignal,
	) {
		const args = ["mark", "--pane", options.pane, "--lead", options.lead, "--name", options.name];
		if (options.machine) args.push("--machine", options.machine);
		if (options.ticket) args.push("--ticket", options.ticket);
		return this.exec(Marked, args, signal);
	}

	queue(pane: string, count: number, signal?: AbortSignal) {
		return this.exec(Reported, ["queue", "--pane", pane, "--count", String(count)], signal);
	}

	async notify(title: string, body: string): Promise<void> {
		try {
			await this.run(["notify", title, "--body", body]);
		} catch {
			// A toast is best effort.
		}
	}
}

function parseJson(text: string): unknown {
	const trimmed = text.trim();
	if (!trimmed) return undefined;
	try {
		return JSON.parse(trimmed) as unknown;
	} catch {
		return undefined;
	}
}
