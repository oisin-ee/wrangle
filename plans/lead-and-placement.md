# Plan: one lead per repo, mechanical placement, Jev for the profile, visible fleet

Status: revised after review (Jev dropped). Two units, independent after Unit 1.

## Context

Observed on 2026-10-08 (`herdr agent list`): workspace `w42` (rondo) holds three unnamed `working` Pi sessions (`p1`, `p7`, `pP`) and two children, one marked `↳ jalgpall-data`, one not (`fix-1350-fixture`). `~/.config/herdr/config.toml` has no `[ui.sidebar.agents]` rows, so even that mark does not render. An agent called `wrangle` and nobody could tell which session was the orchestrator.

Causes, from the code:

1. **No lead identity.** `leadName()` is `WRANGLE_LEAD ?? HERDR_PANE_ID ?? pi-<pid>` (`pi/spawn.ts:38`). Every Pi session that calls `wrangle` becomes its own lead. Nothing ties a lead to a repo, and nothing stops a second lead in the same repo.
2. **Layout is an LLM choice.** `placement` (`new_workspace|new_tab|pane`) and `workspace` pass straight through to Shepherdr (`pi/spawn.ts:54-61`, `pi/types.ts:11,28-29`). Children land wherever the model says.
3. **Nothing shows while things work.** Pi shows a widget only while a ticket is queued (`pi/queue.ts widgetLines`) and toasts only on `queued` (`pi/index.ts`). Herdr marks need config the user does not have.

Out of scope for now (user decision): a decision model for `agent_type`. The lead keeps choosing the profile from the table in `~/.agents/rules/20-delegation.md`. For later: Jev is already reachable from an extension through `ctx.modelRegistry.classify()` (`docs/models.md:164`, `examples/extensions/jev-router.ts`), credentials present on `openrouter`.

## Approach

### A. Lead = repo. Children live in the lead's workspace. (mechanical)

- Lead id is `lead:<repo-basename>` (repo = `git rev-parse --show-toplevel` of the tool's cwd; fall back to today's `HERDR_PANE_ID`).
- On the first `spawn` in a session, the tool **claims** the lead: `herdr pane report-metadata <own pane> --source wrangle --display-agent "⌂ <repo>" --token role=lead --token repo=<repo>`. Herdr loses metadata on restart; the tool re-reports on every call, same as the `↳` marks today.
- Before claiming, the tool lists `herdr agent list` on the local host and looks for another pane with `role=lead` and the same `repo` whose `agent_status` is not `done`. If found and it is not me → return `isError` with `{lead_exists: true, pane, workspace, name}` and the text: "A lead for <repo> runs in <pane>. Send the unit to it with `agents send`, or pass `take_over=true`." `take_over=true` re-marks the old pane with `--clear-token role` and claims.
- Layout rule, no parameter:
  - `branch` set → the prepare hook's pane (unchanged).
  - no `branch` → `placement=new_tab`, `workspace=<lead's workspace_id>` (from `herdr agent list` row for `HERDR_PANE_ID`). Remove `placement` and `workspace` from `WrangleParameters`; the CLI drops `--workspace`.
- Children marks gain `--token lead=<lead id>` (today `owner`), so the sidebar can group by lead.

### B. Make it visible. (UI)

1. **Pi widget, always on while the lead has live children or tickets** (`aboveEditor`, id `wrangle`):
   `⌂ rondo · 3 children: 2 working 1 idle · 0 queued · local 5.7 netcup 19.3 momokaya-2 10.2`
   plus one line per queued ticket (today's `widgetLines`). Refresh: on every tool call, on the poll tick, and on Shepherdr's `herdr-agent-message` custom messages (child state changes) via `pi.on("message")`-equivalent hook if present, else a 60 s timer while children > 0.
2. **Toast on `spawned`**: `unit a → netcup (headroom 19.3)` — mirror of the `queued` toast in `pi/index.ts`.
3. **Lead mark** `⌂ <repo>` on the lead's pane (from A), so the sidebar shows owner and children together.
4. **Sidebar rows**: ship a `herdr-sidebar.toml` snippet in the repo and print it from `wrangle status --sidebar`; the README already documents the block but nobody has pasted it. (The user's `config.toml` is unmanaged; this stays a copy-paste, done once.)
5. **`wrangle status` groups by lead**: `lead ⌂ rondo w42:p1 · children: fix-1350-fixture idle, jalgpall-data idle · queue 0`, then hosts.

## Files to modify

| Unit | Files |
|---|---|
| 1 (A) | `pi/spawn.ts` (`leadName`→`leadId`, `spawnArguments` layout rule, lead claim), `pi/types.ts` (drop `placement`/`workspace`, add `take_over`), `pi/index.ts` (claim before spawn, error path), `src/herdr.rs` (lead mark helpers, `lead=` token), `src/commands.rs` + `src/cli.rs` (`wrangle lead [--take-over]`, `status` lead grouping, drop `--workspace` from `spawn`), `src/spawn.rs`, `skills/wrangle/SKILL.md`, `README.md`, `~/.agents/rules/20-delegation.md` (agent repo, separate commit) |
| 2 (B) | `pi/queue.ts` (`widgetLines` → `fleetLines(lead, children, hosts, tickets)`), `pi/index.ts` (spawned toast, refresh hooks), `src/commands.rs` (`status --sidebar`), new `herdr-sidebar.toml`, `README.md` |

## Reuse

- Host choice: `src/plan.rs` `plan/choose/evaluate` — unchanged.
- Pane marks: `src/herdr.rs` `mark`/`unmark`/`queue` build `report-metadata` argv; add `lead` next to them with the same shape and tests (`src/herdr.rs:253-281`).
- Agent list parsing: `src/probe.rs parse_agent_list` — extend `AgentRow` with `name`, `pane_id`, `workspace_id`, `display_agent`, `tokens` (check the JSON key for reported tokens with `herdr agent list`).
- Widget/toast plumbing: `pi/index.ts` `port.render`, `engine.notify`.
- Poll timer: `pi/queue.ts Queue.schedule` — reuse for the fleet refresh.
- Shepherdr placement contract: `pi-shepherdr/src/launch.ts:15-23` (`new_tab` + `workspace`).

## Steps

Unit 1 — lead and layout (branch `feat/lead`)
- [ ] `src/probe.rs`: parse `name`, `pane_id`, `workspace_id`, `display_agent`, tokens from `herdr agent list`; test with a fixture that has a `role=lead` row.
- [ ] `src/herdr.rs`: `lead_mark(pane, repo)`, `lead_clear(pane)`; `mark` adds `--token lead=<id>`; tests.
- [ ] `src/commands.rs`/`src/cli.rs`: `wrangle lead --repo <name> [--take-over] --json` → `{claimed}` or exit 1 `{lead_exists, pane, workspace}`; `status` lists leads with their children.
- [ ] `pi/spawn.ts`: `leadId(cwd)`; `spawnArguments` applies the layout rule; `runSpawn` calls `engine.lead()` first and maps exit 1 to `toolError`.
- [ ] `pi/types.ts`: remove `placement`, `workspace`; add `take_over?: boolean`; update `pi/spawn.test.ts`.
- [ ] `skills/wrangle/SKILL.md` + `README.md`: "one lead per repo; if `lead_exists`, `agents send` the unit to that lead".
- [ ] Agent repo `20-delegation.md`: same sentence; remove the placement guidance.

Unit 2 — visibility (branch `feat/fleet-widget`)
- [ ] `pi/queue.ts`: `fleetLines(...)`; tests.
- [ ] `pi/index.ts`: spawned toast; refresh on tool call, poll tick, and 60 s timer while children > 0.
- [ ] `src/commands.rs`: `status --sidebar` prints the TOML block; `herdr-sidebar.toml` at repo root.
- [ ] README: replace the "Sidebar marks" block with `wrangle status --sidebar`.

## Verification

- `mise run check` (fmt, clippy `-D warnings`, cargo test, tsc, shim tests) in each worktree.
- Unit 1 manual: in a second Pi session on rondo call `wrangle agent_type=explorer label=x message=y` → `lead_exists` names `w42:p?`; in the first session the same call spawns a tab in `w42`; `herdr agent list` shows `⌂ rondo` on the lead and `lead=lead:rondo` on the child.
- Unit 2 manual: after one spawn the widget line appears above the editor and the toast names the host; `wrangle status --sidebar | pbcopy`, paste into `config.toml`, `herdr` sidebar shows `⌂`/`↳`.

## Decisions taken (change if wrong)

1. Second lead in the same repo is **refused**, not auto-redirected. Auto-forwarding a unit to another session would move work out of the conversation that asked for it.
2. Lead scope is per repo, not per host: `lead:<repo>`; the lead's own pane stays where the user opened it. Remote children still open through the prepare hook or a new tab in the lead's workspace on their host.
3. No decision model in this round. `agent_type` stays the lead's choice; host and layout are arithmetic.
4. Sidebar rows remain a one-time paste; wrangle does not edit `~/.config/herdr/config.toml`.
