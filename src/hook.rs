//! The `prepare` hook: an argv template from the policy, run on the admitted
//! host after admission. It must print one JSON object with `pane_id`.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hosts::{Fleet, Host};
use crate::shell::quote;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepared {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub host: Host,
}

#[derive(Debug, Clone, Copy)]
pub struct HookInputs<'a> {
    pub repo: &'a str,
    pub branch: &'a str,
    pub base: &'a str,
}

/// Substitute `{repo}`, `{branch}`, `{base}`; drop argv words that become
/// empty (an omitted `base` in an argv-style template).
#[must_use]
pub fn render(template: &[String], inputs: HookInputs<'_>) -> Vec<String> {
    template
        .iter()
        .map(|word| {
            word.replace("{repo}", inputs.repo)
                .replace("{branch}", inputs.branch)
                .replace("{base}", inputs.base)
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Parse the hook's stdout: the whole output, else its last non-empty line.
pub fn parse_output(stdout: &str, host: &Host) -> Result<Prepared> {
    #[derive(Deserialize)]
    struct Raw {
        pane_id: String,
        #[serde(default)]
        workspace_id: Option<String>,
    }
    let candidates = std::iter::once(stdout.trim()).chain(stdout.lines().rev().map(str::trim));
    for text in candidates.filter(|t| !t.is_empty()) {
        if let Ok(raw) = serde_json::from_str::<Raw>(text) {
            return Ok(Prepared {
                pane_id: raw.pane_id,
                workspace_id: raw.workspace_id,
                host: host.clone(),
            });
        }
    }
    Err(Error::Hook {
        detail: stdout
            .trim()
            .lines()
            .last()
            .unwrap_or("no output")
            .to_string(),
    })
}

/// Run the policy's `prepare` template on `host`.
pub fn run(fleet: &Fleet, host: &Host, inputs: HookInputs<'_>) -> Result<Prepared> {
    let argv = render(&fleet.policy.hooks.prepare, inputs);
    let Some((program, rest)) = argv.split_first() else {
        return Err(Error::Invalid("hooks.prepare is empty".into()));
    };
    let out = match host.target.as_deref() {
        None => fleet.shell.run_ok(program, rest)?,
        Some(target) => {
            let command = argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
            fleet.shell.run_ok(
                "ssh",
                &[
                    "-T".to_string(),
                    "-o".to_string(),
                    "BatchMode=yes".to_string(),
                    "--".to_string(),
                    target.to_string(),
                    command,
                ],
            )?
        }
    };
    parse_output(&out.stdout, host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(repo: &'a str, branch: &'a str, base: &'a str) -> HookInputs<'a> {
        HookInputs { repo, branch, base }
    }

    #[test]
    fn render_substitutes_and_drops_empty_words() {
        let argv_template: Vec<String> =
            ["mise", "run", "agent:worktree", "--", "{branch}", "{base}"]
                .iter()
                .map(ToString::to_string)
                .collect();
        assert_eq!(
            render(&argv_template, inputs("/r", "feat-x", "")),
            ["mise", "run", "agent:worktree", "--", "feat-x"]
        );
        let sh: Vec<String> = crate::config::DEFAULT_PREPARE
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            render(&sh, inputs("/r", "feat-x", "main"))[2],
            "cd /r && mise run -q agent:worktree -- feat-x main"
        );
    }

    #[test]
    fn parse_output_accepts_trailing_json_line() {
        let host = Host::local();
        let p = parse_output(
            "creating worktree…\n{\"pane_id\":\"w7:p2\",\"workspace_id\":\"w7\"}\n",
            &host,
        )
        .unwrap();
        assert_eq!(p.pane_id, "w7:p2");
        assert_eq!(p.workspace_id.as_deref(), Some("w7"));
        assert!(matches!(
            parse_output("nothing here", &host),
            Err(Error::Hook { .. })
        ));
    }
}
