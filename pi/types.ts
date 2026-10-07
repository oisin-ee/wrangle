// Typebox schemas for the tool's parameters and for the JSON the `wrangle`
// binary prints. The output schemas mirror the Rust serde types in `src/`.

import { StringEnum } from "@earendil-works/pi-ai";
import { type Static, Type } from "typebox";

export const ACTIONS = ["spawn", "status", "cancel", "help"] as const;
export type Action = (typeof ACTIONS)[number];

/** Shepherdr's `agents spawn` placements (`src/launch.ts` START_PLACEMENTS). */
export const PLACEMENTS = ["new_workspace", "new_tab", "pane"] as const;

export const WrangleParameters = Type.Object(
	{
		action: Type.Optional(
			StringEnum(ACTIONS, {
				description: "Defaults to spawn. status: hosts and your queue. cancel: drop a ticket.",
			}),
		),
		// Pass-through to Shepherdr `agents spawn`.
		agent_type: Type.Optional(Type.String({ description: "Shepherdr profile (explorer, general, astra, …)" })),
		name: Type.Optional(Type.String({ description: "Agent name; derived from label when omitted" })),
		label: Type.Optional(Type.String()),
		message: Type.Optional(Type.String({ description: "The child's task" })),
		machine: Type.Optional(
			Type.String({ description: "Pin to one host (id or label). Omit to balance across hosts." }),
		),
		placement: Type.Optional(StringEnum(PLACEMENTS)),
		workspace: Type.Optional(Type.String()),
		pane: Type.Optional(Type.String()),
		cwd: Type.Optional(Type.String()),
		blocking: Type.Optional(Type.Boolean({ description: "Passed to agents spawn unchanged" })),
		base: Type.Optional(Type.String({ description: "Base ref for the worktree and review" })),
		// wrangle's own fields.
		ticket: Type.Optional(
			Type.String({ description: "A queued ticket to spawn once admitted (from the wake-up message)" }),
		),
		branch: Type.Optional(
			Type.String({
				description: "Writer branch: runs the prepare hook (agent:worktree) on the admitted host",
			}),
		),
		repo: Type.Optional(
			Type.String({ description: "Repository for the prepare hook; default: this directory" }),
		),
	},
	{ additionalProperties: false },
);
export type WrangleParams = Static<typeof WrangleParameters>;

// ---- binary output ---------------------------------------------------------

export const Host = Type.Object({
	id: Type.String(),
	label: Type.String(),
	target: Type.Optional(Type.String()),
});
export type Host = Static<typeof Host>;

export const Reservation = Type.Object({
	ticket: Type.String(),
	lead: Type.String(),
	created_ms: Type.Number(),
	pane: Type.Optional(Type.String()),
});

export const Probe = Type.Object({
	host: Type.String(),
	label: Type.String(),
	load1: Type.Number(),
	cores: Type.Number(),
	disk_free_percent: Type.Number(),
	agents: Type.Object({
		total: Type.Number(),
		by_status: Type.Record(Type.String(), Type.Number()),
	}),
	reservations: Type.Array(Reservation),
});
export type Probe = Static<typeof Probe>;

export const Headroom = Type.Object({
	host: Type.String(),
	headroom: Type.Number(),
	eligible: Type.Boolean(),
	reason: Type.Optional(Type.String()),
	live_agents: Type.Number(),
});

export const HostReport = Type.Object({
	id: Type.String(),
	label: Type.String(),
	target: Type.Optional(Type.String()),
	ok: Type.Boolean(),
	probe: Type.Optional(Probe),
	headroom: Type.Optional(Headroom),
	error: Type.Optional(Type.String()),
});
export type HostReport = Static<typeof HostReport>;

export const Admitted = Type.Object({
	admitted: Type.Literal(true),
	ticket: Type.String(),
	lead: Type.String(),
	host: Host,
	probe: Probe,
	headroom: Headroom,
});
export type Admitted = Static<typeof Admitted>;

export const Queued = Type.Object({
	queued: Type.Literal(true),
	ticket: Type.String(),
	hosts: Type.Array(HostReport),
	reason: Type.String(),
});
export type Queued = Static<typeof Queued>;

export const AdmitOutput = Type.Union([Admitted, Queued]);
export type AdmitOutput = Static<typeof AdmitOutput>;

export const QueueStatus = Type.Object({
	ticket: Type.String(),
	lead: Type.String(),
	created_ms: Type.Number(),
	machine: Type.Optional(Type.String()),
	state: Type.String(),
	host: Type.Optional(Type.String()),
	age_ms: Type.Number(),
});

export const Status = Type.Object({
	hosts: Type.Array(HostReport),
	queue: Type.Array(QueueStatus),
});
export type Status = Static<typeof Status>;

export const Released = Type.Object({
	released: Type.Number(),
	dequeued: Type.Boolean(),
});

export const Cancelled = Type.Object({
	cancelled: Type.Literal(true),
	ticket: Type.String(),
	released: Type.Number(),
	dequeued: Type.Boolean(),
});

export const Marked = Type.Object({
	marked: Type.Boolean(),
	pane: Type.String(),
	ticket: Type.Optional(Type.String()),
});

export const Prepared = Type.Object({
	pane_id: Type.String(),
	workspace_id: Type.Optional(Type.String()),
	host: Host,
});
export type Prepared = Static<typeof Prepared>;

export const Reported = Type.Object({
	reported: Type.Boolean(),
	pane: Type.String(),
	count: Type.Number(),
});

export const Failure = Type.Object({ error: Type.String() });

/** What Shepherdr's `agents spawn` puts in `details` on success. */
export const SpawnDetails = Type.Object({
	spawned: Type.Literal(true),
	machine: Type.Optional(Type.String()),
	target: Type.Optional(Type.String()),
	name: Type.Optional(Type.String()),
	status: Type.Optional(Type.String()),
});
export type SpawnDetails = Static<typeof SpawnDetails>;
