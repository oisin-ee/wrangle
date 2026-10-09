# wrangle

Admit, queue, and spawn coding agents across [Herdr](https://herdr.dev) hosts.

Lead agents that spawn children on the same machines race each other and
over-commit a host. `wrangle` is the shared gate. It probes every host, reserves
a slot on the host with the most headroom, and either spawns there or hands back
a queue ticket. The lead never picks a host and never polls.

One engine, four faces:

- A Rust CLI, `wrangle`, for every harness that has a shell.
- A native [Pi](https://github.com/earendil-works/pi) tool, also `wrangle`, that spawns through
  [Shepherdr](https://www.npmjs.com/package/@howaboua/pi-shepherdr) so the child stays monitored.
- A skill, `wrangle`, that teaches an agent the orchestration procedure.
- A Herdr plugin that releases a reservation when its pane closes and opens a
  fleet board.

## How it decides

For each host the engine reads the one-minute load, the core count, the free
space of the home disk, the Herdr agent list, and its own reservations. A host
is eligible when the home disk has more than 15 % free. Its headroom is
`load_per_core_max × cores − load1 − reservation_load × reservations`. The
engine picks the eligible host with the largest headroom. Ties go to the host
with fewer live agents.

An admitted call holds a reservation on that host for `reservation_ttl_ms`
(3 minutes). Each reservation counts as one extra unit of load, so a burst of
admits from several leads spreads out instead of landing on one host. The
reservation is attached to the child's pane after the spawn. It ends at the TTL
or when the pane closes.

When no host is eligible, the call returns at once with `queued: true` and a
ticket. The ticket lives in the caller's queue. A later `admit --ticket` tries
again from the same ticket, so the oldest request wins.

## One lead per repository

Each repository has one lead: the session that orchestrates its children.
The first spawn from a Herdr pane claims the lead for the pane's repository
(a linked worktree counts as its main repository). The pane gets
`display_agent = "⌂ <repo>"` and the tokens `role=lead` and `repo=<repo>`.
Children get `↳ <name>` and `lead=lead:<repo>`.

When another live pane already leads the repository, the spawn stops before
it admits anything and names that pane. Send the unit to that lead with
`agents send`, or move the lead with `take_over=true` (CLI: `--take-over`).
A lead pane whose agent is `done` does not count.

The layout is not a choice. A child with a branch opens in the pane that the
`prepare` hook makes. Any other child opens as a new tab in the lead's
workspace on the local host, or in a new workspace on a remote host.

Reservations live on the host they reserve, in
`~/.local/state/wrangle/reservations.json` under a directory lock. Remote hosts
run the same primitives over `ssh -T -o BatchMode=yes`. There is no daemon.

## Install

The binary is a prebuilt release asset for `aarch64-apple-darwin` and
`x86_64-unknown-linux-gnu`. Install it on every host, including the remote
ones, because a lead probes a remote host by running `wrangle` there.

```sh
# mise (no Rust toolchain needed)
mise use -g github:oisin-ee/wrangle@v0.3.0

# or cargo
cargo install --git https://github.com/oisin-ee/wrangle --tag v0.3.0
```

The Pi tool and the skill come from the same repository:

```sh
pi install git:github.com/oisin-ee/wrangle@v0.3.0
```

Add `wrangle` to your Pi settings `packages` to keep it on `pi update`. The
Pi tool calls Shepherdr's `agents` tool, so Shepherdr must be installed too.

## Configuration

Two optional files. Every key has a default. Unknown keys are an error.

- `~/.config/wrangle/config.toml` (or `$WRANGLE_CONFIG`): the host's policy.
- `wrangle.toml` at the root of a repository: how that repository prepares a
  child. See "Repository config" below.

The host policy:

```toml
load_per_core_max = 1.5        # host is full when load1 ≥ this × cores
disk_free_min_percent = 15     # home disk must have more than this free
reservation_load = 1.0         # load units one reservation adds
reservation_ttl_ms = 180000    # a reservation without a pane ends after this
poll_ms = 30000                # not used by the engine; kept for the Pi shim
# How a lead runs wrangle on a remote host. mise shims are not on the PATH of
# a non-interactive ssh session, but `mise` itself is.
remote_command = ["mise", "x", "--", "wrangle"]

[hooks]
# Runs on the admitted host when a spawn names a branch. It must print one JSON
# object with `pane_id` (and optionally `workspace_id`): a pane at a shell
# prompt inside the new worktree. {repo}, {branch}, {base} are substituted.
# {repo} is the main checkout of the lead's repository (a linked worktree
# resolves to it). On a remote host it is relative to home, where ssh starts the
# hook, so each host may keep the checkout under its own home. A name that is not
# a directory passes through unchanged. A repository's wrangle.toml replaces it.
prepare = ["sh", "-c", "cd {repo} && mise run -q agent:worktree -- {branch} {base}"]
```

### Repository config

A repository can carry `wrangle.toml` at its root. It holds only `[hooks]`;
admission thresholds stay host policy.

```toml
[hooks]
# Replaces the host's hooks.prepare for spawns from this repository.
prepare = ["sh", "-c", "cd {repo} && ./scripts/prepare-child {branch} {base}"]
```

The lead reads the file from the main checkout of its repository, also when it
runs in a linked worktree. The rendered command then runs on the admitted host.
A missing file, or a file without `prepare`, uses the host policy. An invalid
file is an error.

## CLI

Every command accepts `--json`. Exit codes: 0 ok, 1 every host is full (the
body carries the ticket), 2 error.

```sh
wrangle status [--sidebar]          # leads and children, hosts, headroom, queue
wrangle lead [--repo .] [--take-over]  # claim ⌂ <repo> on $HERDR_PANE_ID; exit 1 when held
wrangle probe [--machine netcup]    # the raw per-host numbers
wrangle admit --lead main-verify    # reserve a slot; exit 1 with a ticket when full
wrangle admit --ticket w-…          # try a queued ticket again
wrangle release --ticket w-…        # drop a reservation and its queue entry
wrangle release --pane w3:p2        # drop the reservation attached to a pane
wrangle cancel --ticket w-…         # release and dequeue; error when unknown
wrangle prepare --branch feat/x --base main [--machine netcup] [--repo /path]
wrangle mark --pane w3:p2 --lead lead:rondo --name reviewer [--ticket w-…]
wrangle spawn --kind claude --name unit-a --message "…" [--lead name | --take-over] \
  [--branch feat/x --base main] [--repo /path] [--cwd /path] [--machine netcup]
```

`spawn` is the whole flow for a harness that is not Pi. It admits or resumes a
ticket and places a pane: the `prepare` hook with `--branch`, else a new tab
in the lead's workspace. Without `--lead` it first claims the lead on
`$HERDR_PANE_ID`. Then it runs `herdr agent start`, marks the pane, and runs
`herdr agent prompt`. It prints
`{"spawned": true, "pane_id": …, "name": …, "status": "working"}`. When the
agent stops on a startup dialog, `status` is `blocked` and `next` says what to
do; the pane and the reservation stay.

`admit` and `spawn` take `--machine <id or label>` as a pin. Without it the
engine considers the local host and every enabled machine from
`herdr machine list`.

`wrangle report [--since 24h] [--json]` shows per-ticket durations in milliseconds, plus host refusal counts and reasons from local history.
The duration filter accepts `ms`, `s`, `m`, `h`, or `d` and selects recent tickets and refusal attempts.
Missing endpoints produce `-` (`null` in JSON), while immediate admission has zero queue wait.
The ledger appends lifecycle metadata to `events.jsonl` under its lock, without prompts or messages, and removes records older than 30 days.
`status` shows current refusal reasons in text and JSON.

## Pi tool

The package registers one tool, `wrangle`. `action` defaults to `spawn`, so
the usual call is:

```
wrangle agent_type=general label="unit a" message="…" branch=feat/unit-a base=main
```

The tool claims the repository's lead (see "One lead per repository"), then
admits. When `branch` is set, it runs the `prepare` hook on the
admitted host. Then it calls `agents spawn` on that host with the same
arguments. The result
is Shepherdr's spawn result plus `host` and `ticket`. Pass-through fields:
`agent_type`, `name`, `label`, `message`, `machine`, `cwd`.
The `base` field goes only to the `prepare` hook, not to Shepherdr.
The spawn never blocks: the call returns once the child has its task, and the
child's completion arrives later as a Shepherdr message. There is no `blocking`
field.
There is no `placement`, `workspace`, or `pane`: the layout follows from
`branch`. A `lead_exists` result is an error that names the other lead.

When every host is full the tool returns `{queued, ticket, reason, hosts}` at
once. The ticket is persisted in the session, listed in a widget above the
editor, and shown as a `queue` token on the lead's Herdr pane. A timer tries
the oldest ticket again every 30 s, then every 60 s. On admission the
reservation is kept and the lead receives a `wrangle` message. The lead then
calls `wrangle ticket=<ticket>`; the stored arguments are reused.

Other actions: `wrangle action=status`, `wrangle action=cancel ticket=…`,
`wrangle action=help`.

## What you see

In the lead's Pi session:

- A widget above the editor while the session leads a repository or has
  tickets: `⌂ rondo · 3 children: 1 idle 2 working · 0 queued · local 5.7
  netcup 19.3 momokaya-2 10.2`, then one line per queued ticket. It refreshes
  after every `wrangle` call and every 60 s while children run or tickets wait.
- A Herdr toast for each spawn (`unit a → netcup (headroom 19.3)`), each queued
  ticket, and each spawn refused with `lead_exists`.

In the Herdr sidebar, once you add the rows below:

- The lead's pane shows `⌂ <repo>` in blue, with `N queued` while it waits.
- Each child shows `↳ <name>` in red, a red `●` after its state icon, and the
  lead that owns it.

Herdr keeps this metadata until it restarts; the tool reports it again on
every call. `wrangle status` (and the `board` popup) lists every lead with its
children before the hosts.

To show the sidebar marks, print the rows and add them to your Herdr
`config.toml` once:

```sh
wrangle status --sidebar >> ~/.config/herdr/config.toml   # skip if you already have [ui.sidebar.agents]
herdr config check && herdr server reload-config
```

The rows are in [`herdr-sidebar.toml`](herdr-sidebar.toml). If your config
already has `[ui.sidebar.agents]`, merge the `rows` by hand.

## Herdr plugin

`herdr-plugin.toml` at the repository root declares two event hooks and one
popup pane. Install it on every host:

```sh
herdr plugin install oisin-ee/wrangle --ref v0.3.0 --yes
```

- `pane.closed` and `pane.exited` run `wrangle release --pane <id>`. The
  reservation attached to the pane ends at once instead of at its TTL.
- The `board` pane runs `wrangle status --watch` in a popup. Open it with
  `herdr plugin pane open --plugin wrangle --entrypoint board` or bind it to a key.

The plugin does not build anything. It finds `wrangle` on the Herdr server's
PATH, then through `mise x`, so install the binary first.

## Skill

`skills/wrangle/SKILL.md` is the procedure a lead follows. It splits units and
calls `wrangle` once per unit without a host. It sends units to the existing
lead when `wrangle` reports `lead_exists`. It handles `spawned` and
`queued`, uses `agents` for everything after the spawn, and cleans up after the
merge. Pi loads it
from the package. For other harnesses, symlink the directory into
`~/.agents/skills/` or `~/.claude/skills/`.

## Development

```sh
mise run check          # fmt --check, clippy -D warnings, cargo test, tsc, shim tests
cargo install --path .  # the binary on PATH
pi install /path/to/wrangle
```

A tag `vX.Y.Z` runs the cargo-dist workflow and attaches the two archives to
the GitHub release.

## License

MIT.
