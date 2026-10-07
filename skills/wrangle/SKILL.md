---
name: wrangle
description: Orchestrate parallel coding agents across Herdr hosts with the wrangle tool. Use when splitting work into units, spawning or queueing children, or following up on their results.
---

# wrangle

`wrangle` picks the host for each child, reserves a slot there, and spawns through Shepherdr. When every host is full, it gives you a ticket and wakes you when the ticket is admitted. You never pick a host, read load, or poll.

## Procedure

1. Split the task into units. One unit is one change that merges on its own. Commit your own changes before you spawn, because a worktree starts from the committed HEAD.

2. For each unit, call the tool once. Give the child a writer branch when it edits files:

   ```
   wrangle agent_type=general label="unit a" message="<task>" branch=feat/unit-a base=main
   ```

   Omit `machine`. Set `machine` only when the unit needs one host, for example a browser on `local`. Omit `branch` for read-only work. Then the child gets a new workspace on the admitted host.

3. Read the result:
   - `spawned`: note `name`, `target`, and `host`. The child runs. Continue with the next unit.
   - `queued`: note `ticket`. Continue with other work or end your turn. A `wrangle` message arrives when the ticket is admitted. Then call `wrangle ticket=<ticket>`. The stored arguments are reused.
   - `isError`: read the text. Correct the call, or report the blocker.

4. Do not call `wrangle` again for the same ticket before the wake-up message. Do not call `wrangle action=status` in a loop. One `status` call is fine when you report.

5. After the spawn, use `agents` for the child: `send`, `read`, `answer`, `watch`, `assign`. The child's completion arrives as a message. Do not wait on it with `sleep`.

6. After you merge or drop a unit, remove its worktree with the repository's remove task (`agent:worktree-remove` in a repository that uses the agent harness). The reservation ends when the pane closes or when its TTL ends.

7. Leave panes marked `↳` alone unless the `owner` token names you. They belong to another lead.

## Without the Pi tool

In a shell (Claude Code, Codex, a script), the CLI does the same work:

```
wrangle spawn --lead <your-name> --kind claude --name unit-a --message "<task>" --branch feat/unit-a --base main --json
```

Exit 0 prints `{"spawned": true, "pane_id": …, "name": …}`. Exit 1 prints `{"queued": true, "ticket": …}`. If the exit code is 1, retry later with `wrangle spawn --ticket <ticket>` and the same arguments. `wrangle status` shows every host and your queue. Then follow the child with `herdr [--machine <id>] agent read <name>` and `herdr [--machine <id>] agent prompt <name> "<text>"`.

If `status` is `blocked`, the child sits on a startup dialog. Read its pane, answer the dialog with `herdr agent send-keys`, then prompt it.

## Rules

- `machine` is a resource pin, not a work-to-host mapping. `wrangle` balances the hosts.
- Every writer gets its own branch. The `prepare` hook makes the worktree on the admitted host.
- Give the child one unit, its allowed files, its verification command, and commit authority. Do not give it the transcript.
