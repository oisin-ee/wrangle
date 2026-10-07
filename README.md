# wrangle

Admit, queue, and spawn coding agents across [Herdr](https://herdr.dev) hosts.

Three lead agents that each spawn children on the same machines will race and
over-commit a host. `wrangle` is the shared gate: it probes every host, reserves
a slot on the one with the most headroom, and either spawns there or hands back a
queue ticket. One engine, four faces:

- a Rust CLI, `wrangle`, for any harness that has a shell;
- a native [Pi](https://pi.dev) tool, also called `wrangle`, that spawns through
  Shepherdr so the child stays monitored;
- a Herdr plugin for cleanup hooks and a fleet-wide board;
- a skill that teaches an agent the orchestration method.

Status: scaffold. See `CHANGELOG.md`.
