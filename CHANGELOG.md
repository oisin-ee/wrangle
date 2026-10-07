# Changelog

All notable changes to this project are documented in this file.

## Unreleased

- Pi tool `wrangle`: `spawn` (default) admits on the host with the most headroom, runs the
  `prepare` hook when `branch` is set, calls Shepherdr `agents spawn` on that host, and marks
  the child pane; `status`, `cancel`, `help`. Queued calls return a ticket at once.
- Queue: tickets persist in the session (`wrangle-queue` entry), a poll timer (30 s → 60 s)
  re-admits the oldest ticket, admission sends a `wrangle` wake-up message, `wrangle ticket=…`
  reuses the stored arguments; queued tickets show in a widget above the editor, as a
  `queue` token on the lead's Herdr pane, and as a toast.
- Engine: `prepare` command runs the policy's `[hooks] prepare` template on a host.
- Engine: `probe`, `admit`, `release`, `status`, `cancel`, `mark`, `queue`, `notify`, and the
  per-host `host` primitives. Balanced admission by headroom, TTL reservations under a
  per-host lock, queue tickets, remote hosts over `ssh -T` (`remote_command`, default
  `mise x -- wrangle`).
- Scaffold: Cargo crate, Pi package manifest, CI, cargo-dist release workflow.
