//! Typed wrappers over the `herdr` commands the engine issues. `--machine`
//! forwards a command to a saved machine over ssh, so every call runs from
//! the lead's host.

use crate::error::Result;
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
