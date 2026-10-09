//! The `prepare` hook: an argv template from the policy, run on the admitted
//! host after admission. It must print one JSON object with `pane_id`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hosts::{Fleet, Host};
use crate::lead;
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

/// `{repo}` as the admitted host sees it. A directory resolves to its main
/// checkout, because a linked worktree exists only on the lead's host. A remote
/// host gets the checkout relative to the lead's home: hosts keep repositories
/// at the same place under their own home, and ssh starts the hook there. Any
/// other value (a name, or a path outside home) passes through unchanged.
pub fn locate(fleet: &Fleet, host: &Host, repo: &str) -> Result<String> {
    if !Path::new(repo).is_dir() {
        return Ok(repo.to_string());
    }
    let checkout = lead::main_checkout(fleet.shell, repo)?;
    if host.is_local() {
        return Ok(checkout.display().to_string());
    }
    Ok(relative_to_home(&checkout, Path::new(&fleet.home))
        .unwrap_or_else(|| checkout.display().to_string()))
}

/// `checkout` relative to `home` (`.` for home itself). Git reports resolved
/// paths, so a symlinked home matches through its canonical form too.
fn relative_to_home(checkout: &Path, home: &Path) -> Option<String> {
    let canonical = home.canonicalize().ok();
    let relative = checkout
        .strip_prefix(home)
        .ok()
        .or_else(|| checkout.strip_prefix(canonical.as_deref()?).ok())?;
    if relative.as_os_str().is_empty() {
        Some(".".to_string())
    } else {
        Some(relative.display().to_string())
    }
}

/// Run the policy's `prepare` template on `host`.
pub fn run(fleet: &Fleet, host: &Host, inputs: HookInputs<'_>) -> Result<Prepared> {
    let repo = locate(fleet, host, inputs.repo)?;
    let inputs = HookInputs {
        repo: &repo,
        ..inputs
    };
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
    use crate::config::Policy;
    use crate::ledger::Ledger;
    use crate::shell::Fake;

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
    fn relative_to_home_strips_home_or_keeps_none() {
        let home = Path::new("/nonexistent-home/o");
        assert_eq!(
            relative_to_home(Path::new("/nonexistent-home/o/code/rondo"), home).as_deref(),
            Some("code/rondo")
        );
        assert_eq!(
            relative_to_home(Path::new("/nonexistent-home/o"), home).as_deref(),
            Some(".")
        );
        assert_eq!(relative_to_home(Path::new("/srv/rondo"), home), None);
    }

    #[test]
    fn remote_hook_enters_the_checkout_relative_to_home() {
        let shell = Fake::new();
        let ledger = Ledger::new(std::env::temp_dir().join("wrangle-hook-remote"));
        let policy = Policy::default();
        let dir = std::env::temp_dir().display().to_string();
        let fleet = Fleet {
            shell: &shell,
            ledger: &ledger,
            policy: &policy,
            home: "/nonexistent-home/o".to_string(),
            hosts: Vec::new(),
        };
        let host = Host {
            id: "8103".to_string(),
            label: "netcup".to_string(),
            target: Some("netcup-dev".to_string()),
        };
        shell.on(
            &format!("git -C {dir} rev-parse --path-format=absolute --git-common-dir"),
            "/nonexistent-home/o/code/rondo/.git\n",
        );
        shell.on(
            "ssh -T -o BatchMode=yes -- netcup-dev sh -c 'cd code/rondo && mise run -q agent:worktree -- feat-x main'",
            "{\"pane_id\":\"w9:p1\"}\n",
        );
        let p = run(&fleet, &host, inputs(&dir, "feat-x", "main")).unwrap();
        assert_eq!(p.pane_id, "w9:p1");

        // A name, not a directory, reaches the hook unchanged.
        assert_eq!(locate(&fleet, &host, "rondo").unwrap(), "rondo");
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
