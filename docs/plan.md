# Plan: `herdr-dispatch` — a standalone package that admits, queues, and spawns children across hosts

Status: ready for review. All decisions taken (see "Decisions"). Revised: the package is named **`wrangle`**
(one word: repo, crate, binary, Pi tool, Herdr plugin id); the repo is public and GitHub Actions builds the release
binaries; Rust 1.99 (current stable); a `wrangle` skill teaches agents the orchestration method. The engine is a
Rust crate; the Pi face is a thin strict-TypeScript shim (Pi can only load TS/JS extensions); the agent harness
repo only registers it (settings, tools, bootstrap, skill, rule, docs). No scripts are added to the agent repo.

## Context

Three lead Pi sessions run in Herdr at once. Each one spawns children through Shepherdr's `agents` tool and picks a
host by hand (`uptime`, `df -h ~`, then `agents spawn machine=<id>`; `rules/topics/20-delegation.md:27-28`). Nothing
coordinates the three leads, so they race for the same host and over-commit it.

Wanted: one native tool call, shaped like `agents spawn`, that checks capacity, spawns when there is room, queues
when there is not, and keeps each lead's window showing only that lead's own work. No LLM dispatcher agent. No
`tool_call` interception. The engine must not be tied to Pi, so Claude Code or Codex leads can use the same
admission and queue from their shell tool.

One public repo (`oisin-ee/wrangle`), four faces, one engine:

1. **Rust CLI** `wrangle` (Cargo binary crate): `probe`, `admit`, `release`, `status`, `cancel`, `mark`,
   `queue`, `spawn`. All logic lives here. Any harness calls it from bash. On PATH on every host through mise
   (`[tools] "github:oisin-ee/wrangle"`, prebuilt release assets from GitHub Actions; no toolchain needed on hosts).
2. **Pi package** (`pi.extensions` in a `package.json` at the repo root): tool `wrangle`, native in Pi. A thin
   strict-TypeScript shim: validates arguments with the host's `typebox`, runs `wrangle … --json`, spawns
   through `ctx.executeTool("agents")` so Shepherdr keeps monitoring, and owns the Pi-only parts (widget, poll timer,
   wake-up message). No decision logic in TS.
3. **Herdr plugin** (`herdr-plugin.toml` at the repo root): `[[events]]` cleanup hooks, a `[[startup]]` sidebar
   view, a `[[panes]]` popup board, all argv calls into the same binary. Adds no model-callable tool; Herdr has no
   such surface.
4. **Skill** `skills/wrangle/SKILL.md`: the orchestration method (split units, spawn through `wrangle`, handle
   `queued`, follow up with `agents`, merge and prune). Canonical in this repo; vendored into the agent repo's
   `skills/` so Claude Code and Codex read it too.

## What exists today (evidence)

| Surface | Fact | Source |
| --- | --- | --- |
| Shepherdr 0.2.15 | `agents` is one tool, default `direct` exposure, `executionMode: "sequential"`. No fleet-wide scheduler, cap, or queue (grep of `src/` for `max\|limit\|concurren\|queue\|loadavg\|capacity\|slot`: no hits). | `~/.pi/agent/npm/node_modules/@howaboua/pi-shepherdr/src/agents-tool.ts:34-40` |
| Shepherdr | `spawn` returns JSON `{spawned, machine, target (pane id), name, status}` in `content` text and `details`. Async settlement reaches the controller that spawned the child through `pi.sendMessage` (`customType: "herdr-agent-message"`). | `src/agents-spawn.ts:160-188`, `src/delivery.ts:99-103` |
| Shepherdr | Remote machines = Herdr saved profiles (`herdr machine list --json`: `id`, `label`, `target`, `session`). Transport is `ssh -T -o BatchMode=yes -- <target> herdr --session <session>`. | `src/fleet.ts:66-68`, `src/remote-client.ts:104-105` |
| Shepherdr | Settlement comes from Herdr `events.subscribe` on `pane.agent_status_changed` and `pane.closed`. Watches persist per session (`appendEntry`, restored from `ctx.sessionManager.getBranch()`). | `src/monitor-events.ts:113-117`, `src/fleet.ts:73-74,683` |
| Shepherdr | Fleet widget: `ctx.ui.setWidget("herdr-agents", lines, { placement: "aboveEditor" })`, lists only agents this controller monitors. | `src/widget.ts:104` |
| Shepherdr | Ships as a Pi package: `package.json` with `"pi": { "extensions": ["./index.ts"] }`, keyword `pi-package`, host packages as `peerDependencies` only. Installed with `npm:@howaboua/pi-shepherdr` in `settings/pi/settings.json`. | `~/.pi/agent/npm/node_modules/@howaboua/pi-shepherdr/package.json` |
| Pi 1.0.4 | A package is "an ordinary directory or npm package"; sources `npm:`, `git:` ("Cloned and reconciled to the selected ref"; tags are pinned), URL, or a local path ("Loaded from the resolved path without copying"). `pi install ./local-package` for development. Host packages (`pi-coding-agent`, `pi-tui`, `typebox`, …) go in `peerDependencies` with `"*"`; Pi installs `dependencies` for npm and git sources. | `docs/packages.md` "Choose a source", "Create a package", "Declare dependencies" |
| Pi 1.0.4 | Extensions are TypeScript or JavaScript modules loaded through `jiti`; there is no other extension language. So the Pi face must be TS, and it stays a shim over the Rust binary. | `docs/extensions.md` lines 3, 38, 46 |
| Rust | Current stable is **1.99.0** (released 2026-10-01). Installed: local cargo 1.97.1 from Homebrew (formula stable is 1.99.0, so `brew upgrade rust` reaches it), netcup cargo 1.98.1 via rustup, momokaya-2 cargo 1.97.1. All resolve over `ssh -T -o BatchMode=yes`. The agent repo keeps rustup off the Mac (`agent-runtime.toml:55-56`). | `static.rust-lang.org/dist/channel-rust-stable.toml`; `brew info rust`; `command -v cargo` + `readlink` on each host |
| mise `github` backend | `"github:owner/repo" = "v0.1.0"` installs prebuilt assets from a GitHub release, autodetecting OS/arch from asset names; releases without assets are skipped and it never builds from source. `asset_pattern`/`matching` narrow selection. | mise docs `dev-tools/backends/github.md` lines 2-7, 24, 34-36, 70, 88-94 |
| mise `cargo` backend | Fallback: `cargo:https://github.com/owner/repo@tag:TAG` → `cargo install --git`, via cargo-binstall when present (binstall tries release binaries, quickinstall, then source). Needs a toolchain ≥ `rust-version` on the host. | mise docs `dev-tools/backends/cargo.md` lines 7-8, 40-57; cargo-binstall README lines 4-11 |
| cargo-dist | `dist init` writes `dist-workspace.toml` and `.github/workflows/release.yml`; pushing a tag `vX.Y.Z` builds per-target archives and attaches them to a GitHub release. Public repos get free Actions minutes. | cargo-dist book `quickstart/rust.md` lines 13-36, 109-116 |
| Rust conventions | The user's Rust workspaces use edition 2024, `clippy::pedantic` warn, `unwrap_used`/`expect_used`/`panic` deny, `unsafe_op_in_unsafe_fn` deny; CI runs `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo fmt --check`. | `/Users/oisin/dev/rondo/Cargo.toml:5-32`, `mise.toml:94-124` |
| Pi 1.0.4 skills | A skill is a directory with `SKILL.md` (frontmatter `name`, `description`, then instructions). Pi reads `~/.agents/skills/` and `.agents/skills/`; packages can ship skills (`pi.skills`); the settings object form `{ "source": …, "skills": [] }` loads none of a package's skills. "Name collisions keep the first discovered skill and produce a warning." | `docs/skills.md` lines 11-29, 59-63, 85, 91; `docs/packages.md` "Select package resources" |
| Agent repo skills | `skills/` is the master tree, linked into `~/.agents/skills` and `~/.claude/skills` (`symlink-each`, `agent-runtime.toml:628-630`), so one `SKILL.md` reaches Pi, Claude Code, and Codex. Third-party skills are vendored with a `metadata` block naming `upstream`, `source`, `revision` (`skills/herdr/SKILL.md:1-10`). | `agent-runtime.toml:628-630`; `skills/herdr/SKILL.md` |
| Names | `wrangle` is free on crates.io, npm, and `github.com/oisin-ee`; `muster`, `drover`, `corral`, `shepherd`, `marshal` are taken on crates.io; `paddock` and `roundup` are free on crates.io but taken on npm. | crates.io API, GitHub API, `npm view` (2026-10-08) |
| Pi 1.0.4 | A tool can run another tool: `ctx.executeTool(name, args)` is on `ExtensionToolContext` (inside `execute()`); event and command handlers get plain `ExtensionContext` without it. Nested calls go through `tool_call`/`tool_result` hooks and return `isError: true` instead of rejecting. | `docs/extensions.md` "Tools"; `types.ts` `ExtensionToolContext` |
| Pi 1.0.4 | `pi.sendMessage(message, { triggerTurn?, deliverAs?: "steer" \| "followUp" \| "nextTurn" })`. Timers start in `session_start` or inside a tool, stop in `session_shutdown`. `ctx.ui.setWidget(id, lines, { placement })`, `ctx.ui.notify`. State: `details` on tool results, `pi.appendEntry()` for durable data, rebuild from `ctx.sessionManager.getBranch()`. | `types.ts` `ExtensionAPI`; `docs/extensions.md` "Respect the runtime lifecycle", "State" |
| Pi 1.0.4 | An MCP server cannot call Pi tools; a Herdr plugin cannot call Pi tools. Only an extension tool can call `agents`. | `docs/mcp.md`, herdr.dev/docs/plugins |
| Herdr 0.9.3 | Plugins are a `herdr-plugin.toml` manifest with `[[build]]`, `[[startup]]`, `[[actions]]`, `[[events]]`, `[[panes]]`, `[[link_handlers]]` argv commands run from the plugin root. Installed from GitHub (`herdr plugin install owner/repo`) or linked (`herdr plugin link <path>`). "Startup hooks are one-shot initialization commands rather than supervised daemons" — but a started process may stay alive on its own (the installed `herdr.auto-title` plugin does exactly that). No plugin storage API. "Runtime action registration ... [is] not part of v1." | herdr.dev/docs/plugins; `~/.config/herdr/plugins/github/herdr.auto-title-*/herdr-plugin.toml` |
| Herdr 0.9.3 | A plugin cannot expose a tool to an agent: `herdr plugin action invoke <action_id> [--plugin ID]` takes no payload (verified locally); socket `plugin.action.invoke` `context` is filled from the active workspace, tab, focused pane, worktree provenance, and request id. | `herdr plugin action invoke --help`; socket-api "Plugin APIs" |
| Herdr 0.9.3 | `[[events]] on = "<event>"` runs an argv command when Herdr emits that event; emitted names include `pane.agent_status_changed`, `pane.closed`, `pane.exited`, `worktree.removed`. `herdr agent start --kind <pi\|claude\|codex\|…>`, `agent prompt`, `agent wait` are agent-kind agnostic. | socket-api "Event subscriptions"; `herdr api schema --json` |
| Herdr 0.9.3 | `herdr [--machine id] pane report-metadata <pane> --source ID --display-agent NAME --token NAME=VALUE [--ttl-ms N]`: display-only; `$name` tokens render in `ui.sidebar.agents.rows`; 80-char values, 32 keys, lost on server restart. `state_icon` accepts fixed styles only, not rules; text tokens accept `rules = [{ starts_with = …, fg = … }]`. | herdr.dev/docs/configuration "Sidebar row layouts"; socket-api "Agent state reporting" |
| Herdr 0.9.3 | `notification.show` / `herdr notification show "title" --body …`. `--machine <id>` forwards API commands over non-interactive SSH. | socket-api; cli-reference "Saved SSH machines" |
| Agent repo | npm CLIs reach every host through mise: `[tools] "npm:<name>" = "latest"` (`agent-runtime.toml:32,42,72-82`). Pi packages are listed in `settings/pi/settings.json` `packages` and updated by `pi update --extensions` in `[tasks.bootstrap]`. Herdr plugins are installed there with `herdr plugin install <owner>/<repo> --yes` (`agent-runtime.toml:737-738`). | `agent-runtime.toml`, `settings/pi/settings.json` |
| Agent repo | `agent:worktree` creates the writer's worktree and linked Herdr workspace on the host that owns the checkout and prints `{pane_id, workspace_id}`; `agent:worktree-remove` reverses it. | `agent-runtime.toml:353-470, 550` |
| Local | `@oisin-ee` scope maps to GitHub Packages in `~/.npmrc`; remote hosts have no such mapping in the agent repo. With git sources for Pi and mise, and GitHub for Herdr, no registry publish is needed for the first release. `wrangle` is free on npm; crates.io availability is unverified. | `npm config get @oisin-ee:registry`; `npm view wrangle --registry https://registry.npmjs.org` |

## Decisions

1. **Never hold a call.** When every host is full, `wrangle` returns `{ queued, ticket }` at once. A timer
   re-admits and wakes the lead with a message; the lead re-issues `wrangle ticket=T`. The `blocking` field
   is passed through to `agents spawn` unchanged; it only decides whether Shepherdr waits for the child's reply.
2. **Worktree creation is a hook, not hard-coded.** The config names a `prepare` argv template that runs on the
   admitted host and must print JSON with `pane_id`. The default is `agent:worktree`; a user can replace it. Hooks
   run only after admission, so a queued unit holds no pane or worktree.
3. **No per-host agent cap.** Admission keeps the existing rule only: load per core `< 1.5` and home disk `> 15 %`
   free. Each unexpired reservation counts as one extra unit of load, so three leads admitting within the same minute
   do not all see the same idle host. The lead decides how many children to run.
4. **Host choice is balanced.** With no explicit `machine`, the tool spreads spawns across the eligible hosts by
   headroom (`1.5 × cores − load1 − reservations`); a host at capacity drops out and the rest keep balancing.
   `machine` is an optional pin.
5. **Herdr sidebar marks children.** The tool reports metadata on every child pane it spawns; a small
   `[ui.sidebar.agents]` block in the user's `~/.config/herdr/config.toml` renders it. Herdr cannot recolor
   `state_icon` by rule, so the mark is a red token right after the icon plus a recolored `↳ name`. See "Sidebar
   layout". The user's `config.toml` is not managed by any repo; the block is documented in the package README.
6. **No work-to-host mapping.** `rules/topics/20-delegation.md:31-33` (Rust → netcup, TypeScript → momokaya-2,
   browser → local) is removed; the tool spreads children across every enabled machine.
7. **Separate package, separate public repo.** New public repo `oisin-ee/wrangle` (`gh repo create --public`),
   so GitHub Actions minutes are free. One git tag `vX.Y.Z` drives every face: cargo-dist's `release.yml` builds
   `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu` archives (the three hosts: verified `uname -sm`) and
   attaches them to the GitHub release; mise installs the binary with `"github:oisin-ee/wrangle" = "vX.Y.Z"`
   (no toolchain needed on any host); Pi installs the extension and skill with `git:github.com/oisin-ee/wrangle@vX.Y.Z`;
   Herdr installs the plugin with `herdr plugin install oisin-ee/wrangle --ref vX.Y.Z`. Fallback if an asset is
   missing for a platform: `"cargo:https://github.com/oisin-ee/wrangle" = "tag:vX.Y.Z"`. No npm or crates.io
   publish for the first release. The agent repo gets no scripts: it registers the package in
   `settings/pi/settings.json`, adds the mise tool, adds the `herdr plugin install` line, ships the config file
   through `[dotfiles]`, vendors the skill, and edits the rule and docs.
8. **Rust engine, TypeScript shim.** The engine is a Cargo binary crate on current stable: edition 2024,
   `rust-version = "1.99"`, `rust-toolchain.toml` pinning `channel = "1.99.0"` for CI, the rondo lint set
   (pedantic warn, `unwrap_used`/`expect_used`/`panic` deny). Dependencies: `clap` (derive), `serde` +
   `serde_json`, `toml`, `thiserror`; no async runtime (probes are parallel `std::thread::scope` over
   `std::process::Command`). The dev Mac needs `brew upgrade rust` (1.97.1 → 1.99.0) before Unit 1; hosts need
   nothing because they install prebuilt assets. The Pi shim is strict TypeScript (`"strict": true`,
   `noUncheckedIndexedAccess`), typechecked with `tsc --noEmit`, zero npm `dependencies`, host packages as
   `peerDependencies`. Node is not required anywhere except inside Pi itself.
9. **One word: `wrangle`.** Repo, crate, binary, Pi tool, Herdr plugin id, config dir (`~/.config/wrangle/`),
   state dir (`~/.local/state/wrangle/`), metadata source, and skill all share it. The Pi tool's `action` defaults
   to `spawn`, so the common call is `wrangle agent_type=… label=… message=…`; `status`, `cancel`, and `help`
   are explicit actions. `wrangle` is free on crates.io, npm, and GitHub (checked 2026-10-08). Alternates if the
   user prefers: `paddock`, `roundup` (both free on crates.io, taken on npm).
10. **Skill `wrangle` ships with the package and is vendored into the agent repo.** Canonical file
    `skills/wrangle/SKILL.md` in the package repo, declared in `package.json` `pi.skills` for Pi-only users. The
    agent repo vendors it as `skills/wrangle/SKILL.md` (markdown only, with the `metadata.upstream/source/revision`
    block used by `skills/herdr`), so `~/.agents/skills` and `~/.claude/skills` carry it to every harness. The agent
    repo's package entry uses the object form with `"skills": []` so Pi does not load the same skill twice.

## Approach

### Package layout (`/Users/oisin/dev/wrangle`, GitHub `oisin-ee/wrangle`)

```text
wrangle/
├── Cargo.toml            [package] name = "wrangle" · edition 2024 · rust-version 1.99 · [[bin]] wrangle
│                         [lints] = rondo set · deps clap, serde, serde_json, toml, thiserror
├── Cargo.lock
├── rust-toolchain.toml   channel = "1.99.0"
├── dist-workspace.toml   cargo-dist: targets aarch64-apple-darwin, x86_64-unknown-linux-gnu; installers none
├── src/
│   ├── main.rs           clap `Cli { command, json }` → dispatch to the modules; exit codes 0 ok / 1 full / 2 error
│   ├── config.rs         `Policy` (serde, defaults) from ~/.config/wrangle/config.toml or $WRANGLE_CONFIG
│   ├── plan.rs           pure `fn plan(probes: &[Probe], policy: &Policy) -> Plan` (`Admit { host } | Full { hosts }`)
│   ├── probe.rs          `Probe`: load1 (`/proc/loadavg` or `sysctl vm.loadavg`), cores, `df` of $HOME, `herdr agent list`, reservations
│   ├── ledger.rs         ~/.local/state/wrangle/{lock/,reservations.json}: `create_dir` lock, TTL reservations
│   ├── hosts.rs          `herdr machine list --json` → `Host { id, label, target, local: bool }`; run self locally or
│   │                     `ssh -T -o BatchMode=yes -- <target> wrangle probe --json` (binary is on every host)
│   ├── herdr.rs          typed wrappers over `herdr [--machine id] pane report-metadata | notification show | agent start | agent prompt`
│   └── spawn.rs          `prepare` hook template + Shepherdr-free spawn for non-Pi harnesses
├── tests/                integration: run the binary with a fake `herdr` shim dir prepended to PATH
├── pi/
│   ├── index.ts          Pi extension: registerTool("wrangle"), widget, poll timer, wake-up message
│   ├── engine.ts         typed `exec(args) -> Result<T>` over `wrangle --json` (child_process, JSON parse, zod-free: typebox `Value.Check`)
│   └── types.ts          Typebox schemas for CLI output and tool params (mirrors the Rust serde types)
├── package.json          "pi": { "extensions": ["./pi/index.ts"] } · peerDependencies pi-coding-agent, pi-tui, typebox "*"
│                         · no dependencies · scripts typecheck = tsc --noEmit
├── tsconfig.json         strict, noUncheckedIndexedAccess, module NodeNext, types node
├── skills/wrangle/SKILL.md   the orchestration skill (see "Skill"); listed in package.json "pi": { "skills": ["./skills"] }
├── herdr-plugin.toml     id "wrangle" · commands ["wrangle", …] (binary from PATH; no [[build]])
├── mise.toml             tasks: check = cargo fmt --check + cargo clippy --all-targets -- -D warnings + cargo test + tsc --noEmit
├── lefthook.yml          pre-commit runs `mise run check`
├── README.md             install (mise, pi, herdr), CLI, Pi tool, sidebar block
├── LICENSE, CHANGELOG.md
└── .github/workflows/
    ├── ci.yml            fmt + clippy + test + tsc on push and PR (ubuntu + macos matrix)
    └── release.yml       generated by `dist init`; on tag vX.Y.Z builds both targets and attaches archives to the release
```

### Layer 1: CLI `wrangle`

| Command | Does | Prints (`--json`) |
| --- | --- | --- |
| `probe [--machine id]` | per host: load1, cores, disk free %, `herdr agent list` counts, unexpired reservations, headroom | `[{host, headroom, …}]` |
| `admit --lead <name> [--machine id]` | probe, pick the host with most headroom (ties → fewer live agents), lock, re-check, write reservation | `{admitted, ticket, host}` or `{full, hosts}` |
| `release --ticket T` / `release --pane P` | drop a reservation | `{released}` |
| `status [--watch]` | hosts plus every lead's reservations and tickets on each host | JSON or a live table (popup board) |
| `cancel --ticket T` | drop a queued ticket and its reservation | `{cancelled}` |
| `mark --machine id --pane P --lead <name> --name <agent> [--clear]` | `pane report-metadata --source wrangle --display-agent "↳ <name>" --token sub=● --token owner=<lead>` | `{marked}` |
| `queue --pane P --count N` | `--token queue="N queued"` on the lead pane, or `--clear-token queue` when 0 | `{reported}` |
| `spawn --ticket T --kind <pi\|claude\|codex> --label … --message … [--branch … --base … --repo …]` | `prepare` hook, `herdr agent start`, `herdr agent prompt`; for harnesses without Shepherdr | `{spawned, machine, pane_id, name}` |

Host access: local when the host is this machine; otherwise `ssh -T -o BatchMode=yes -- <target> wrangle
<probe|reserve|release> --json` (the same SSH form Shepherdr uses for `herdr`). The binary is installed on every
host by mise, and cargo paths already resolve over non-interactive SSH (verified). Targets come from `herdr machine
list --json`. Reservations live on the target host so all three leads see the same ledger. `plan()` is pure; I/O is
thin and behind traits (`trait Shell`, `trait HerdrApi`) so unit tests use fakes.

Config (`~/.config/wrangle/config.toml`, every key optional):

```toml
load_per_core_max = 1.5
disk_free_min_percent = 15
reservation_load = 1.0
reservation_ttl_ms = 180000
poll_ms = 30000

[hooks]
prepare = ["sh", "-c", "cd {repo} && mise run -q agent:worktree -- {branch} {base}"]
```

Headroom = `load_per_core_max × cores − load1 − reservation_load × unexpired reservations`; the host must also pass
the disk rule. `prepare` is an argv template with `{repo}`, `{branch}`, `{base}` placeholders; it runs on the
admitted host and must print one JSON object with `pane_id`.

### Layer 2: Pi tool `wrangle` (`pi/index.ts`, strict TypeScript shim)

Registered with `pi.registerTool({ name: "wrangle", … })`, default `direct` exposure, so it is declared to the
model like `agents` and callable from codemode. Every decision is made by the binary: the shim runs
`wrangle <cmd> --json`, validates the output against Typebox schemas (`pi/types.ts`), and maps it to tool
results. If the binary is missing it returns `isError` with the mise install line. Actions:

| Action | Fields | Result |
| --- | --- | --- |
| `help` | — | actions, policy, hosts |
| `spawn` (default when `action` is omitted) | all `agents spawn` fields (`agent_type`, `label`, `message`, `machine?`, `placement?`, `pane?`, `cwd?`, `blocking?`, `name?`) plus `ticket?` and hook inputs `branch?`/`base?`/`repo?` | the `agents spawn` result plus `{ host }`, or `{ queued, ticket, hosts, reason }` |
| `status` | `machine?` | per host: load/core, disk free, live agents by status, reservations; this lead's queue |
| `cancel` | `ticket` | drops a queued ticket and its reservation |

Flow for `wrangle` (action `spawn`):

1. Eligible hosts: `machine` → that one; omitted → local plus every enabled profile from `herdr machine list --json`.
2. `wrangle admit --lead <this session's agent name> [--machine id] --json`. Exit 1 (`full`) → step 5.
3. Run the `prepare` hook on the admitted host when the call carries hook inputs; its `pane_id` becomes
   `placement: "pane"`, `pane`. Otherwise the call's own `placement`/`pane`/`cwd` pass through.
4. `ctx.executeTool("agents", { action: "spawn", machine, … })`. Success → `wrangle release --ticket T`,
   `wrangle mark …` on the child pane, return Shepherdr's result plus `{ host }`. Failure → `release`,
   return the error with `isError`.
5. Full: `pi.appendEntry("wrangle-queue", …)`, update the widget and `queue` token, start the poll timer if
   needed, return `{ queued, ticket, hosts }`.
6. Poll timer (only while this lead has tickets; 30 s backing off to 60 s): `wrangle admit --ticket T` for
   the oldest ticket.
   On admission keep the reservation (its TTL covers the gap) and send
   `pi.sendMessage({ customType: "wrangle", content: "ticket T admitted on netcup; call wrangle
   ticket=T" }, { triggerTurn: true, deliverAs: "followUp" })`. A `spawn` carrying `ticket` skips step 2 and
   continues at step 3 with the reserved host. Stop the timer when the queue is empty and in `session_shutdown`.
   Restore tickets from `ctx.sessionManager.getBranch()` on `session_start`.

Visibility in each lead's window: `ctx.ui.setWidget("wrangle", lines, { placement: "aboveEditor" })`, one
line per queued ticket of this lead, next to Shepherdr's fleet widget; removed when empty. Spawned children appear in
Shepherdr's widget as today because this lead's Shepherdr spawned them. Toast on a queued start via
`herdr notification show`.

Flow from a non-Pi harness (Claude Code, Codex): `wrangle admit --lead <name>` → `wrangle spawn
--ticket T --kind …` → `herdr [--machine id] agent wait <name>` when the result is needed. No wake-up; the harness
polls. Documented in the README and the delegation rule as the fallback path.

### Layer 3: Herdr plugin (`herdr-plugin.toml`, later unit)

- `[[events]] on = "pane.closed"` and `on = "pane.exited"` → `["wrangle", "release", "--pane",
  "$HERDR_PANE_ID"]` (drops a stale reservation if a child dies between `admit` and `spawn`).
- `[[startup]]` → re-apply an `agent.view.set` that groups children under their `$owner`.
- `[[panes]] id = "board" placement = "popup"` → `["wrangle", "status", "--watch"]`: fleet-wide queue.
- Commands call the binary from PATH (installed by the mise tools phase, which runs before `[tasks.bootstrap]`), so
  no `[[build]]`. Fallback if Herdr's plugin environment lacks the mise PATH: `[[build]] command = ["cargo",
  "build", "--release", "--locked"]` and `./target/release/wrangle`, the `herdr.auto-title` pattern. Verify
  at Unit 6. `min_herdr_version = "0.9.1"`.
- Possible later: a `[[startup]]` process that stays alive as a per-host scheduler (the `herdr.auto-title` pattern)
  if the file lock ever proves racy. Not in scope now.

### Skill: `skills/wrangle/SKILL.md`

Frontmatter: `name: wrangle`; `description: Orchestrate parallel coding agents across Herdr hosts with the wrangle
tool. Use when splitting work into units, spawning or queueing children, or following up on their results.`
Body (≤ 80 lines, STE style, lead with the procedure):

1. Split the task into independent units first; one unit per child; keep one unit for yourself.
2. For each unit call `wrangle agent_type=… label=… message=… branch=… [base=…] [repo=…]`. Do not pick a host; pass
   `machine` only when the unit must run where a resource lives (for example a browser).
3. Read the result: `spawned` → note `name` and `target`; `queued` → note `ticket`, continue with other work, and
   when the wake-up message `ticket T admitted on <host>` arrives, call `wrangle ticket=T`. Never poll by hand.
4. Use Shepherdr's `agents` for everything after spawn: `send`, `read`, `answer`, `watch`, `assign`.
5. After a merge, run `agent:worktree-remove`; `wrangle action=status` shows load, reservations, and your queue.
6. Non-Pi harness: `wrangle admit --lead <name> --json` → `wrangle spawn --ticket T --kind <claude|codex|pi> …`
   → `herdr [--machine id] agent wait <name>`.
7. Do not touch panes marked `↳` in the Herdr sidebar; they belong to another lead.

The delegation rule (`rules/topics/20-delegation.md`) keeps the unit-splitting and review requirements and points
to this skill for the spawn procedure instead of repeating it.

### Sidebar layout (user's `~/.config/herdr/config.toml`, documented in README)

```toml
[ui.sidebar.agents]
rows = [
  ["state_icon", { token = "$sub", fg = "#f38ba8", bold = true }, "machine", "workspace"],
  [{ token = "agent", rules = [{ starts_with = "↳", fg = "#f38ba8" }] }, { token = "$owner", dim = true }, { token = "$queue", fg = "#f9e2af", bold = true }],
]
```

Before (default rows):

```
● local      main
  main-verify
● netcup     agent-wt1
  reviewer
● momokaya-2 agent-wt3
  explorer
```

After (red `●` and `↳ name` on children; yellow queue count on the lead):

```
● local        main
  main-verify · 2 queued
● ● netcup     agent-wt1
  ↳ reviewer · main-verify
● ● momokaya-2 agent-wt3
  ↳ explorer · main-verify
```

The first dot is Herdr's state icon (unchanged); the second, always red, is the "sub-agent, do not touch" mark.
`$sub` is unreported on lead panes, so it disappears there. Metadata is lost on a Herdr server restart; the tool
re-reports on every `status`, `spawn`, and poll. Children spawned by hand with `agents spawn` get no mark.

## Files to modify

New repo `oisin-ee/wrangle` (everything under "Package layout" above).

Agent repo `oisin-ee/agent` (configuration only, no code):

- `settings/pi/settings.json` — add `{ "source": "git:github.com/oisin-ee/wrangle@v0.1.0", "skills": [] }` to
  `packages` (object form; the skill comes from `~/.agents/skills` instead; bump the tag on release,
  `pi update --extensions` reconciles the checkout).
- `agent-runtime.toml` — `[tools] "github:oisin-ee/wrangle" = "v0.1.0"` (prebuilt binary on PATH for every
  harness on every host; cargo git source as the documented fallback); `[tasks.bootstrap]` add
  `"herdr plugin install oisin-ee/wrangle --ref v0.1.0 --yes"`; `[dotfiles]`
  `"~/.config/wrangle/config.toml" = { source = "settings/wrangle/config.toml" }`.
- `settings/wrangle/config.toml` — the policy above (data, not code).
- `skills/wrangle/SKILL.md` — vendored copy of the package skill with the `metadata` block (`upstream:
  "oisin-ee/wrangle"`, `source`, `revision`), like `skills/herdr`. Reaches `~/.agents/skills` and
  `~/.claude/skills` through the existing `symlink-each` entries.
- `rules/topics/20-delegation.md` — replace the manual `uptime`/`df`, `agent:worktree`, `agents spawn` steps with
  "spawn through the `wrangle` skill"; delete the work-to-host list at lines 31-33; keep unit splitting, review,
  and prune-after-merge; keep `agents` for `send`, `assign`, `read`, `answer`, `watch`.
- `docs/tools.md` and `docs/skills.md` — one paragraph each on `wrangle` with a link to the package README.

Nothing in Shepherdr or Herdr is modified.

## Reuse

- Shepherdr `agents spawn` via `ctx.executeTool` — child, watch, widget line, and settlement unchanged.
- Shepherdr `src/fleet.ts:66-68` SSH form; `src/widget.ts` widget conventions; `src/agents-tool.ts` Typebox
  parameter schema style; `package.json` manifest shape (`pi.extensions` pointing at a `.ts` file,
  `peerDependencies`, `pi-package` keyword).
- `rondo/Cargo.toml` lint set and `mise.toml` check tasks; `startrail/Cargo.toml` single-crate layout.
- `agent:worktree` / `agent:worktree-remove` (agent repo) — unchanged; `agent:worktree` is the default `prepare`
  hook, `agent:worktree-remove` stays a lead step after merge.
- `runtime/agent-prune.mjs` (agent repo) — pattern only: pure `plan()` + thin I/O; the engine can call
  `mise run -q agent:prune -- --apply` on a failing host before giving up.
- `herdr.auto-title` plugin manifest — pattern for `herdr-plugin.toml` fields and relative argv commands.
- `momokaya-agent-auth` — repo scaffolding pattern (release-please, lefthook, mise.toml, renovate).

## Steps

- [x] Unit 0: `brew upgrade rust` (→ 1.99.0); `gh repo create oisin-ee/wrangle --public`; scaffold
      `/Users/oisin/dev/wrangle` (`cargo init --bin`, `rust-toolchain.toml`, lints, `package.json`,
      `tsconfig.json`, LICENSE, README stub, mise.toml, lefthook, `ci.yml`); `dist init --yes` for
      `dist-workspace.toml` + `release.yml`. Dev loop: `cargo install --path .`, `pi install
      /Users/oisin/dev/wrangle`, `herdr plugin link /Users/oisin/dev/wrangle`.
- [x] Unit 1: engine: `config.rs`, `plan.rs` + unit tests, `probe.rs`, `ledger.rs` + tests (lock contention with
      threads), `hosts.rs`, `herdr.rs`; `main.rs` with `probe`, `admit`, `release`, `status`, `cancel`, `mark`,
      `queue`; integration tests with a fake `herdr` on PATH.
- [x] Unit 2: Pi shim `pi/types.ts`, `pi/engine.ts`, `pi/index.ts` (`help`, `status`, `spawn` admitted path:
      `admit` → `prepare` hook → `executeTool("agents")` → `mark`); `tsc --noEmit` clean; shim tests with a fake
      `ctx` and a fake binary (`node --test` on the `.ts` files, Node 24 strips types).
- [x] Unit 3: queue path (ticket, `appendEntry` restore, poll timer, wake-up message, `spawn ticket=`), `cancel`,
      widget, `queue` token, toast.
- [x] Unit 4: `spawn.rs` for non-Pi harnesses (`prepare` hook, `herdr agent start` + `agent prompt`);
      `skills/wrangle/SKILL.md`; README (install, CLI, Pi, skill, sidebar block); tag `v0.1.0` and confirm
      `release.yml` attaches both archives.
- [ ] Unit 5: agent repo registration (settings object entry with `"skills": []`, mise `github:` tool, bootstrap
      line, dotfile + config, vendored skill, rule, docs); `cza`.
- [ ] Unit 6 (later): `herdr-plugin.toml` (`[[events]]` release, `[[startup]]` view, popup board); crates.io
      publish + release binaries for cargo-binstall if compile-on-install proves slow.

## Verification

- Package: `mise run check` green (`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
  `tsc --noEmit`).
- CLI from bash, no Pi: `wrangle status --json`; `admit --lead probe` then `release --ticket <T>`; a second
  `admit` within the TTL sees one reservation of load on that host; `ssh -T -o BatchMode=yes -- netcup-dev
  wrangle probe --json` works after `cza` on netcup.
- Pi: in a Herdr pane, `pi` → `wrangle action=help`; `wrangle action=status`; `wrangle agent_type=explorer label="probe
  host" message="say hi" blocking=false` three times with `load_per_core_max` lowered in the config so the third is
  `queued`; confirm the lead is woken, re-issues `spawn ticket=`, and the child starts; confirm Shepherdr's widget
  shows only this lead's children and the sidebar shows the red mark.
- Balance: with two eligible hosts, four spawns land two and two (or follow headroom) rather than all on one.
- Race: two Pi sessions call `wrangle` within a second with headroom for one; exactly one is admitted.
- Release: after `git tag v0.1.0 && git push --tags`, the GitHub release carries `wrangle-aarch64-apple-darwin.*`
  and `wrangle-x86_64-unknown-linux-gnu.*`; `mise install github:oisin-ee/wrangle@v0.1.0` succeeds on all three
  hosts without cargo.
- Agent repo: `cza` installs the extension through `pi update --extensions` (git source), the binary through the
  mise `github` backend, the plugin through `herdr plugin install`, the skill through `~/.agents/skills`;
  `pi list`, `which wrangle`, `herdr plugin list`, and `ls ~/.agents/skills/wrangle` show it on local, netcup,
  momokaya-2; `/skill:wrangle` loads in Pi with no collision warning.

## Out of scope

- Recoloring Herdr's `state_icon` itself: not supported by Herdr 0.9.3 config (fixed styles only, no rules).
- A per-host daemon: not needed for the chosen design (possible later via a Herdr plugin `[[startup]]` process).
- Changing Shepherdr or Herdr code.
- Any script in the agent repo.
- Any logic in TypeScript beyond argument validation, process invocation, and Pi UI; the TS shim exists only
  because Pi loads TS/JS extensions and nothing else.
