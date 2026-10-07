// This lead's queued tickets: persisted in the session as a custom entry,
// polled while any remain, and turned into a wake-up message on admission.

import type { Engine } from "./engine.ts";
import { message } from "./spawn.ts";
import type { WrangleParams } from "./types.ts";

export const ENTRY_TYPE = "wrangle-queue";
export const MESSAGE_TYPE = "wrangle";

export interface Ticket {
	ticket: string;
	params: Omit<WrangleParams, "action" | "ticket">;
	createdAt: number;
	state: "queued" | "admitted";
	host?: string;
	reason?: string;
}

export interface QueueState {
	version: 1;
	tickets: Ticket[];
}

/** The slice of Pi the queue needs; narrow so tests can fake it. */
export interface QueuePort {
	persist(state: QueueState): void;
	wake(ticket: Ticket, host: string): void;
	render(tickets: readonly Ticket[]): void;
	now?(): number;
}

export interface PollPolicy {
	/** First interval after a ticket is queued. */
	initialMs: number;
	/** Interval ceiling while tickets stay queued. */
	maxMs: number;
}

export const DEFAULT_POLL: PollPolicy = { initialMs: 30_000, maxMs: 60_000 };

export function isQueueState(value: unknown): value is QueueState {
	return (
		typeof value === "object" &&
		value !== null &&
		(value as { version?: unknown }).version === 1 &&
		Array.isArray((value as { tickets?: unknown }).tickets)
	);
}

export class Queue {
	private tickets: Ticket[] = [];
	private timer: ReturnType<typeof setTimeout> | undefined;
	private intervalMs: number;
	private polling = false;
	private readonly engine: Engine;
	private readonly port: QueuePort;
	private readonly policy: PollPolicy;

	constructor(engine: Engine, port: QueuePort, policy: PollPolicy = DEFAULT_POLL) {
		this.engine = engine;
		this.port = port;
		this.policy = policy;
		this.intervalMs = policy.initialMs;
	}

	list(): readonly Ticket[] {
		return this.tickets;
	}

	find(ticket: string): Ticket | undefined {
		return this.tickets.find((t) => t.ticket === ticket);
	}

	/** Replace the state from a persisted entry (session start). */
	restore(state: QueueState): void {
		this.tickets = state.tickets.map((t) => ({ ...t }));
		this.port.render(this.tickets);
		this.schedule();
	}

	add(ticket: Ticket): void {
		this.tickets = [...this.tickets.filter((t) => t.ticket !== ticket.ticket), ticket];
		this.intervalMs = this.policy.initialMs;
		this.commit();
		this.schedule();
	}

	remove(ticket: string): boolean {
		const before = this.tickets.length;
		this.tickets = this.tickets.filter((t) => t.ticket !== ticket);
		if (this.tickets.length === before) return false;
		this.commit();
		this.schedule();
		return true;
	}

	/** Mark a ticket back to queued (its reservation lapsed and admit queued it again). */
	requeue(ticket: string, reason?: string): void {
		const found = this.find(ticket);
		if (!found) return;
		found.state = "queued";
		delete found.host;
		if (reason) found.reason = reason;
		this.commit();
		this.schedule();
	}

	/**
	 * One poll: try the oldest queued ticket. On admission the reservation is
	 * kept (its TTL covers the gap) and the lead is woken up.
	 */
	async poll(): Promise<void> {
		if (this.polling) return;
		this.polling = true;
		try {
			const oldest = this.tickets.find((t) => t.state === "queued");
			if (!oldest) return;
			const out = await this.engine.admit({ ticket: oldest.ticket });
			if ("admitted" in out) {
				oldest.state = "admitted";
				oldest.host = out.host.label;
				delete oldest.reason;
				this.intervalMs = this.policy.initialMs;
				this.commit();
				this.port.wake(oldest, out.host.label);
			} else {
				oldest.reason = out.reason;
				this.intervalMs = Math.min(this.intervalMs * 2, this.policy.maxMs);
				this.port.render(this.tickets);
			}
		} catch (error) {
			// The engine is unreachable or the ticket is gone; keep the ticket and
			// try again on the next tick. An unknown ticket means another path
			// (cancel, release) already dropped it.
			if (/unknown ticket/i.test(message(error))) {
				const oldest = this.tickets.find((t) => t.state === "queued");
				if (oldest) this.remove(oldest.ticket);
			}
		} finally {
			this.polling = false;
			this.schedule();
		}
	}

	stop(): void {
		if (this.timer) clearTimeout(this.timer);
		this.timer = undefined;
	}

	private schedule(): void {
		this.stop();
		if (!this.tickets.some((t) => t.state === "queued")) return;
		this.timer = setTimeout(() => void this.poll(), this.intervalMs);
		this.timer.unref?.();
	}

	private commit(): void {
		this.port.persist({ version: 1, tickets: this.tickets });
		this.port.render(this.tickets);
	}
}

/** The wake-up text the lead receives when a ticket is admitted. */
export function wakeText(ticket: Ticket, host: string): string {
	const label = ticket.params.label ?? ticket.params.agent_type ?? "child";
	return (
		`wrangle: ticket ${ticket.ticket} (${label}) is admitted on ${host}. ` +
		`Call wrangle ticket=${ticket.ticket} now; the stored arguments are reused unless you pass new ones.`
	);
}

/** "local: load 53.65 + 0 reserved ≥ 1.5 × 12 cores; netcup: …" → "local full, netcup full". */
export function shortReason(reason: string): string {
	return reason
		.split(";")
		.map((part) => {
			const [host, detail = ""] = part.split(":", 2).map((s) => s.trim());
			if (!host) return "";
			const kind = /^load/.test(detail) ? "full" : /^disk/.test(detail) ? "disk" : "unreachable";
			return `${host} ${kind}`;
		})
		.filter(Boolean)
		.join(", ");
}

/** Widget lines, one per ticket; empty when nothing is queued. */
export function widgetLines(tickets: readonly Ticket[], now = Date.now()): string[] {
	if (tickets.length === 0) return [];
	return tickets.map((t) => {
		const label = t.params.label ?? t.params.agent_type ?? "child";
		const age = Math.max(0, Math.round((now - t.createdAt) / 1000));
		const where =
			t.state === "admitted"
				? `admitted on ${t.host ?? "?"}`
				: `queued ${age}s${t.reason ? ` · ${shortReason(t.reason)}` : ""}`;
		return `wrangle ${t.ticket} ${label}: ${where}`;
	});
}
