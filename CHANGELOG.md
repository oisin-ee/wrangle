# Changelog

All notable changes to this project are documented in this file.

## Unreleased

- The `prepare` hook's `{repo}` is now valid on the admitted host. A directory resolves to its
  main checkout, so a lead inside a linked worktree works. On a remote host the checkout is
  relative to home, where ssh starts the hook. Hooks no longer need to rebuild the path from a
  fixed folder such as `~/dev/<name>`.

## 0.3.0 - 2026-10-09

- Lifecycle log: the ledger appends `queued`, `admitted`, `spawned`, `released`, and `cancelled`
  events to `events.jsonl` under its lock. It stores no prompts or messages, keeps 30 days, and
  skips a torn line instead of failing.
- New command `wrangle report [--since <duration>] [--json]`: per-ticket queue wait and run time,
  plus host refusal counts with reasons.
- `wrangle status` shows the refusal reason for every ineligible host.
- The Pi tool passes `base` only to the `prepare` hook, never to Shepherdr's spawn.

## 0.2.1 - 2026-10-09

- The Pi tool never blocks: every spawn goes to `agents spawn` with `blocking: false`, so the call
  returns once the child has its task and the lead keeps working. The child's completion arrives
  as a Shepherdr message. The `blocking` parameter is removed.

## 0.2.0 - 2026-10-08

- One lead per repository. The first spawn from a Herdr pane claims the lead for the pane's
  repository (`⌂ <repo>`, tokens `role=lead`, `repo=`). A second live lead is refused with
  `lead_exists` before anything is admitted; `take_over=true` / `--take-over` moves it.
  New command `wrangle lead`.
- Children carry `lead=lead:<repo>` instead of `owner=`. `wrangle status` lists every lead with
  its children before the hosts; older `owner=` children are grouped too.
- Layout is no longer a choice: the Pi tool drops `placement`, `workspace`, and `pane`, and the
  CLI drops `--workspace`. A child with a branch opens in its worktree; any other child opens as
  a tab in the lead's workspace (a new workspace on a remote host). `spawn --lead` is optional
  inside Herdr.
- Pi widget shows the session's lead, its children by status, the queue, and host headroom;
  refreshes on every call and every 60 s while work runs. Toasts on spawn and on `lead_exists`.
- `wrangle status --sidebar` prints the Herdr sidebar rows (`herdr-sidebar.toml`).

## 0.1.1 - 2026-10-08

- Herdr plugin: `pane.closed` and `pane.exited` release the pane's reservation; a `board`
  popup runs `wrangle status --watch`.

## 0.1.0 - 2026-10-08

- Skill `wrangle`: the orchestration procedure for Pi and for shell harnesses.
- README: install, configuration, CLI, Pi tool, sidebar rows, skill.

- Pi tool `wrangle`: `spawn` (default) admits on the host with the most headroom, runs the
  `prepare` hook when `branch` is set, calls Shepherdr `agents spawn` on that host, and marks
  the child pane; `status`, `cancel`, `help`. Queued calls return a ticket at once.
- Queue: tickets persist in the session (`wrangle-queue` entry), a poll timer (30 s → 60 s)
  re-admits the oldest ticket, admission sends a `wrangle` wake-up message, `wrangle ticket=…`
  reuses the stored arguments; queued tickets show in a widget above the editor, as a
  `queue` token on the lead's Herdr pane, and as a toast.
- CLI `spawn` for harnesses that are not Pi (Claude Code, Codex, …): admit or resume a ticket,
  place a pane (the `prepare` hook with `--branch`, else a new tab or workspace), `herdr agent
  start`, mark, `herdr agent prompt`. An agent blocked on a startup dialog keeps its pane and
  returns `status: blocked` with the next step.
- Engine: `prepare` command runs the policy's `[hooks] prepare` template on a host.
- Engine: `probe`, `admit`, `release`, `status`, `cancel`, `mark`, `queue`, `notify`, and the
  per-host `host` primitives. Balanced admission by headroom, TTL reservations under a
  per-host lock, queue tickets, remote hosts over `ssh -T` (`remote_command`, default
  `mise x -- wrangle`).
- Scaffold: Cargo crate, Pi package manifest, CI, cargo-dist release workflow.
