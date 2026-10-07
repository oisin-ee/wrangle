//! Typed wrappers over the `herdr` commands the engine issues. `--machine`
//! forwards a command to a saved machine over ssh, so every call runs from
//! the lead's host.

use crate::error::{Error, Result};
use crate::shell::Shell;

pub const SOURCE: &str = "wrangle";
/// The mark on a child pane. Rendered red by the sidebar block in the README.
pub const SUB_TOKEN: &str = "●";

fn base(machine: Option<&str>, words: &[&str]) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(m) = machine {
        args.push("--machine".to_string());
        args.push(m.to_string());
    }
    args.extend(words.iter().map(ToString::to_string));
    args
}

/// `pane report-metadata <pane> --source wrangle --display-agent "↳ name"
/// --token sub=● --token owner=<lead>`.
pub fn mark(
    shell: &dyn Shell,
    machine: Option<&str>,
    pane: &str,
    lead: &str,
    name: &str,
) -> Result<()> {
    let display = format!("↳ {name}");
    let sub = format!("sub={SUB_TOKEN}");
    let owner = format!("owner={lead}");
    let args = base(
        machine,
        &[
            "pane",
            "report-metadata",
            pane,
            "--source",
            SOURCE,
            "--display-agent",
            &display,
            "--token",
            &sub,
            "--token",
            &owner,
        ],
    );
    shell.run_ok("herdr", &args).map(|_| ())
}

/// Remove everything `mark` reported.
pub fn unmark(shell: &dyn Shell, machine: Option<&str>, pane: &str) -> Result<()> {
    let args = base(
        machine,
        &[
            "pane",
            "report-metadata",
            pane,
            "--source",
            SOURCE,
            "--clear-display-agent",
            "--clear-token",
            "sub",
            "--clear-token",
            "owner",
        ],
    );
    shell.run_ok("herdr", &args).map(|_| ())
}

/// `--token queue="N queued"` on the lead's own pane; cleared at zero.
pub fn queue_count(shell: &dyn Shell, pane: &str, count: usize) -> Result<()> {
    let value = format!("queue={count} queued");
    let words: Vec<&str> = if count == 0 {
        vec![
            "pane",
            "report-metadata",
            pane,
            "--source",
            SOURCE,
            "--clear-token",
            "queue",
        ]
    } else {
        vec![
            "pane",
            "report-metadata",
            pane,
            "--source",
            SOURCE,
            "--token",
            &value,
        ]
    };
    shell.run_ok("herdr", &base(None, &words)).map(|_| ())
}

/// Where a created pane lives, so a failed start can close it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    /// `workspace` when we created a workspace, `tab` when we added a tab.
    pub created: Created,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    Workspace,
    Tab,
}

/// `tab create --workspace W …` when a workspace is given, else
/// `workspace create …`; both return the root pane at a shell prompt.
pub fn create_location(
    shell: &dyn Shell,
    machine: Option<&str>,
    workspace: Option<&str>,
    cwd: &str,
    label: &str,
) -> Result<Location> {
    let (words, created): (Vec<&str>, Created) = match workspace {
        Some(w) => (
            vec![
                "tab",
                "create",
                "--workspace",
                w,
                "--cwd",
                cwd,
                "--label",
                label,
                "--no-focus",
            ],
            Created::Tab,
        ),
        None => (
            vec![
                "workspace",
                "create",
                "--cwd",
                cwd,
                "--label",
                label,
                "--no-focus",
            ],
            Created::Workspace,
        ),
    };
    let origin = format!("herdr {} {}", words[0], words[1]);
    let out = shell.run_ok("herdr", &base(machine, &words))?;
    let value: serde_json::Value = serde_json::from_str(&out.stdout).map_err(|e| Error::Parse {
        origin: origin.clone(),
        detail: format!("{e}: {}", out.stdout.trim()),
    })?;
    let field = |path: &[&str]| -> Result<String> {
        let mut cur = &value["result"];
        for key in path {
            cur = &cur[key];
        }
        cur.as_str()
            .map(ToString::to_string)
            .ok_or_else(|| Error::Parse {
                origin: origin.clone(),
                detail: format!("missing result.{}", path.join(".")),
            })
    };
    Ok(Location {
        pane_id: field(&["root_pane", "pane_id"])?,
        workspace_id: field(&["root_pane", "workspace_id"])?,
        tab_id: field(&["root_pane", "tab_id"])?,
        created,
    })
}

/// Undo `create_location`.
pub fn close_location(shell: &dyn Shell, machine: Option<&str>, location: &Location) -> Result<()> {
    let words: Vec<&str> = match location.created {
        Created::Workspace => vec!["workspace", "close", &location.workspace_id],
        Created::Tab => vec!["tab", "close", &location.tab_id],
    };
    shell.run_ok("herdr", &base(machine, &words)).map(|_| ())
}

/// What `agent start` observed once the agent appeared in the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    /// Ready for a prompt.
    Ready,
    /// Herdr returned `agent_not_ready`: the agent is up but blocked on a
    /// startup dialog. The name stays valid; answer the dialog, then prompt.
    Blocked,
}

/// `agent start <name> --kind <kind> --pane <pane>`: returns when Herdr sees
/// the agent in the pane.
pub fn agent_start(
    shell: &dyn Shell,
    machine: Option<&str>,
    name: &str,
    kind: &str,
    pane: &str,
) -> Result<Started> {
    let words = ["agent", "start", name, "--kind", kind, "--pane", pane];
    let out = shell.run("herdr", &base(machine, &words))?;
    if out.ok() {
        return Ok(Started::Ready);
    }
    let text = format!("{}{}", out.stdout, out.stderr);
    if text.contains("agent_not_ready") {
        return Ok(Started::Blocked);
    }
    Err(Error::Command {
        program: "herdr".to_string(),
        args: base(machine, &words).join(" "),
        status: out.status.to_string(),
        stderr: text.trim().to_string(),
    })
}

/// `agent prompt <name> <text>`: submits and returns without waiting.
pub fn agent_prompt(
    shell: &dyn Shell,
    machine: Option<&str>,
    name: &str,
    text: &str,
) -> Result<()> {
    let words = ["agent", "prompt", name, text];
    shell.run_ok("herdr", &base(machine, &words)).map(|_| ())
}

/// A toast in the lead's Herdr window. Failure is not fatal to the caller.
pub fn notify(shell: &dyn Shell, title: &str, body: &str) -> Result<()> {
    shell
        .run_ok(
            "herdr",
            &base(None, &["notification", "show", title, "--body", body]),
        )
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Fake;

    #[test]
    fn mark_builds_the_documented_command() {
        let shell = Fake::new();
        let cmd = "herdr --machine 8103 pane report-metadata w3:p2 --source wrangle \
                   --display-agent ↳ reviewer --token sub=● --token owner=main-verify";
        shell.on(cmd, "");
        mark(&shell, Some("8103"), "w3:p2", "main-verify", "reviewer").unwrap();
        assert_eq!(shell.calls(), vec![cmd.to_string()]);
    }

    #[test]
    fn queue_count_sets_then_clears() {
        let shell = Fake::new();
        shell
            .on(
                "herdr pane report-metadata w1:p1 --source wrangle --token queue=2 queued",
                "",
            )
            .on(
                "herdr pane report-metadata w1:p1 --source wrangle --clear-token queue",
                "",
            );
        queue_count(&shell, "w1:p1", 2).unwrap();
        queue_count(&shell, "w1:p1", 0).unwrap();
        assert_eq!(shell.calls().len(), 2);
    }

    #[test]
    fn unmark_clears_every_field() {
        let shell = Fake::new();
        shell.on(
            "herdr pane report-metadata w1:p5 --source wrangle --clear-display-agent \
             --clear-token sub --clear-token owner",
            "",
        );
        unmark(&shell, None, "w1:p5").unwrap();
    }
}
