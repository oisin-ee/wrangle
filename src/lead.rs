//! One lead per repository. A lead is the Herdr pane that orchestrates the
//! children for one repo; it carries `role=lead` and `repo=<name>` and shows
//! `⌂ <repo>` in the sidebar. Children carry `lead=lead:<repo>`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::herdr;
use crate::hosts::HostReport;
use crate::probe::{self, Pane};
use crate::shell::Shell;

/// The ledger and sidebar identity of the lead for `repo`.
#[must_use]
pub fn lead_id(repo: &str) -> String {
    format!("lead:{repo}")
}

/// A repo name from `--repo`: a directory resolves through git (a linked
/// worktree resolves to its main repository); anything else is the name.
pub fn repo_name(shell: &dyn Shell, repo: Option<&str>) -> Result<String> {
    let dir = match repo {
        Some(r) if !Path::new(r).is_dir() => return Ok(r.to_string()),
        Some(r) => r.to_string(),
        None => std::env::current_dir()?.display().to_string(),
    };
    let checkout = main_checkout(shell, &dir)?;
    checkout
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| Error::Parse {
            origin: format!("git -C {dir} rev-parse --git-common-dir"),
            detail: checkout.display().to_string(),
        })
}

/// The main checkout of the repository that holds `dir`; a linked worktree
/// resolves to its main repository.
pub fn main_checkout(shell: &dyn Shell, dir: &str) -> Result<PathBuf> {
    let out = shell.run_ok(
        "git",
        &[
            "-C".to_string(),
            dir.to_string(),
            "rev-parse".to_string(),
            "--path-format=absolute".to_string(),
            "--git-common-dir".to_string(),
        ],
    )?;
    checkout_from_common_dir(&out.stdout).ok_or_else(|| Error::Parse {
        origin: format!("git -C {dir} rev-parse --git-common-dir"),
        detail: out.stdout.trim().to_string(),
    })
}

/// `/x/rondo/.git` → `/x/rondo`; a bare `/x/rondo.git` stays `/x/rondo.git`.
#[must_use]
pub fn checkout_from_common_dir(text: &str) -> Option<PathBuf> {
    let path = Path::new(text.trim());
    let checkout = if path.file_name()? == ".git" {
        path.parent()?
    } else {
        path
    };
    Some(checkout.to_path_buf())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claimed {
    pub claimed: bool,
    /// `lead:<repo>`: pass it as `--lead` to `admit`, `mark`, and `spawn`.
    pub lead: String,
    pub repo: String,
    pub pane: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The pane that held the lead before `--take-over`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub took_over: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadExists {
    pub lead_exists: bool,
    pub lead: String,
    pub repo: String,
    pub pane: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub next: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LeadOutput {
    Claimed(Claimed),
    Exists(LeadExists),
}

/// Claim the lead for `repo` on `pane`, or report the pane that holds it.
/// A lead pane whose agent is `done` does not count. Re-claiming your own
/// pane is a no-op that re-reports the marks (Herdr drops them on restart).
pub fn claim(shell: &dyn Shell, repo: &str, pane: &str, take_over: bool) -> Result<LeadOutput> {
    let list = probe::agent_list(shell)?;
    let other = list
        .panes
        .iter()
        .find(|p| p.pane_id != pane && p.lead_repo() == Some(repo) && !p.is_done());
    let mut took_over = None;
    if let Some(o) = other {
        if !take_over {
            return Ok(LeadOutput::Exists(LeadExists {
                lead_exists: true,
                lead: lead_id(repo),
                repo: repo.to_string(),
                pane: o.pane_id.clone(),
                workspace_id: o.workspace_id.clone(),
                name: o.name.clone(),
                next: format!(
                    "A lead for {repo} runs in {}. Send the unit to it with `agents send`, \
                     or pass take_over=true (CLI: --take-over) to move the lead here.",
                    o.pane_id
                ),
            }));
        }
        herdr::lead_clear(shell, &o.pane_id)?;
        took_over = Some(o.pane_id.clone());
    }
    herdr::lead_mark(shell, pane, repo)?;
    let workspace_id = list
        .panes
        .iter()
        .find(|p| p.pane_id == pane)
        .and_then(|p| p.workspace_id.clone())
        .or_else(|| workspace_of(pane));
    Ok(LeadOutput::Claimed(Claimed {
        claimed: true,
        lead: lead_id(repo),
        repo: repo.to_string(),
        pane: pane.to_string(),
        workspace_id,
        took_over,
    }))
}

/// Herdr pane ids are `<workspace>:<pane>`.
#[must_use]
pub fn workspace_of(pane: &str) -> Option<String> {
    pane.split_once(':').map(|(w, _)| w.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildView {
    pub pane: String,
    pub host: String,
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadView {
    /// `lead:<repo>`, or the raw owner token of children whose lead pane is gone.
    pub lead: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The lead's own pane; absent when no live pane claims this lead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub children: Vec<ChildView>,
}

fn status_of(p: &Pane) -> String {
    p.agent_status.clone().unwrap_or_else(|| "unknown".into())
}

/// Every lead on every probed host, with its children. A child names its
/// lead by `lead:<repo>` or, before this version, by the lead's pane id.
#[must_use]
pub fn leads(reports: &[HostReport]) -> Vec<LeadView> {
    let panes: Vec<(&str, &Pane)> = reports
        .iter()
        .filter_map(|r| r.probe.as_ref().map(|p| (r.host.label.as_str(), p)))
        .flat_map(|(host, p)| p.panes.iter().map(move |pane| (host, pane)))
        .collect();
    let mut views: BTreeMap<String, LeadView> = BTreeMap::new();
    let mut alias: BTreeMap<String, String> = BTreeMap::new();
    for (host, p) in &panes {
        let Some(repo) = p.lead_repo() else { continue };
        let id = lead_id(repo);
        alias.insert(p.pane_id.clone(), id.clone());
        views.insert(
            id.clone(),
            LeadView {
                lead: id,
                repo: Some(repo.to_string()),
                pane: Some(p.pane_id.clone()),
                host: Some((*host).to_string()),
                status: Some(status_of(p)),
                children: Vec::new(),
            },
        );
    }
    for (host, p) in &panes {
        let Some(owner) = p.owner() else { continue };
        let key = alias
            .get(owner)
            .cloned()
            .unwrap_or_else(|| owner.to_string());
        let view = views.entry(key.clone()).or_insert_with(|| LeadView {
            lead: key,
            repo: None,
            pane: None,
            host: None,
            status: None,
            children: Vec::new(),
        });
        view.children.push(ChildView {
            pane: p.pane_id.clone(),
            host: (*host).to_string(),
            name: p.label().to_string(),
            status: status_of(p),
        });
    }
    views.into_values().collect()
}

/// `working 2, idle 1`.
#[must_use]
pub fn tally(children: &[ChildView]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for c in children {
        *counts.entry(c.status.as_str()).or_insert(0) += 1;
    }
    counts
        .iter()
        .map(|(s, n)| format!("{n} {s}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::Host;
    use crate::probe::{AgentCounts, Probe};
    use crate::shell::Fake;

    const LIST: &str = r#"{"result":{"agents":[
        {"agent_status":"working","pane_id":"w42:p1","workspace_id":"w42","name":"main",
         "tokens":{"role":"lead","repo":"rondo"}},
        {"agent_status":"done","pane_id":"w50:p1","workspace_id":"w50",
         "tokens":{"role":"lead","repo":"infra"}},
        {"agent_status":"working","pane_id":"w42:p7","workspace_id":"w42"}
    ]}}"#;

    #[test]
    fn common_dir_checkouts() {
        assert_eq!(
            checkout_from_common_dir("/Users/o/dev/rondo/.git\n"),
            Some(PathBuf::from("/Users/o/dev/rondo"))
        );
        assert_eq!(
            checkout_from_common_dir("/srv/rondo.git"),
            Some(PathBuf::from("/srv/rondo.git"))
        );
    }

    #[test]
    fn a_second_lead_is_refused_then_takes_over() {
        let shell = Fake::new();
        shell.on("herdr agent list", LIST);
        let LeadOutput::Exists(e) = claim(&shell, "rondo", "w42:p7", false).unwrap() else {
            unreachable!("expected lead_exists")
        };
        assert_eq!(e.pane, "w42:p1");
        assert_eq!(e.name.as_deref(), Some("main"));
        assert!(e.next.contains("agents send"));
        assert_eq!(shell.calls(), vec!["herdr agent list".to_string()]);

        shell
            .on(
                "herdr pane report-metadata w42:p1 --source wrangle --clear-display-agent \
                 --clear-token role --clear-token repo",
                "",
            )
            .on(
                "herdr pane report-metadata w42:p7 --source wrangle --display-agent ⌂ rondo \
                 --token role=lead --token repo=rondo",
                "",
            );
        let LeadOutput::Claimed(c) = claim(&shell, "rondo", "w42:p7", true).unwrap() else {
            unreachable!("expected claimed")
        };
        assert_eq!(c.lead, "lead:rondo");
        assert_eq!(c.took_over.as_deref(), Some("w42:p1"));
        assert_eq!(c.workspace_id.as_deref(), Some("w42"));
    }

    #[test]
    fn a_done_lead_and_my_own_pane_do_not_block() {
        let shell = Fake::new();
        shell.on("herdr agent list", LIST).on(
            "herdr pane report-metadata w60:p2 --source wrangle --display-agent ⌂ infra \
             --token role=lead --token repo=infra",
            "",
        );
        let out = claim(&shell, "infra", "w60:p2", false).unwrap();
        assert!(
            matches!(out, LeadOutput::Claimed(ref c) if c.took_over.is_none()
            && c.workspace_id.as_deref() == Some("w60"))
        );
        shell.on(
            "herdr pane report-metadata w42:p1 --source wrangle --display-agent ⌂ rondo \
             --token role=lead --token repo=rondo",
            "",
        );
        assert!(matches!(
            claim(&shell, "rondo", "w42:p1", false).unwrap(),
            LeadOutput::Claimed(_)
        ));
    }

    fn report(label: &str, panes: Vec<Pane>) -> HostReport {
        HostReport {
            host: Host {
                id: label.into(),
                label: label.into(),
                target: None,
            },
            ok: true,
            probe: Some(Probe {
                host: label.into(),
                label: label.into(),
                load1: 0.0,
                cores: 1,
                disk_free_percent: 50.0,
                agents: AgentCounts::default(),
                reservations: Vec::new(),
                panes,
            }),
            headroom: None,
            error: None,
        }
    }

    fn pane(id: &str, status: &str, name: Option<&str>, tokens: &[(&str, &str)]) -> Pane {
        Pane {
            pane_id: id.into(),
            workspace_id: workspace_of(id),
            name: name.map(Into::into),
            display_agent: None,
            agent_status: Some(status.into()),
            tokens: tokens
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    #[test]
    fn leads_group_children_across_hosts_and_legacy_owners() {
        let reports = vec![
            report(
                "local",
                vec![
                    pane(
                        "w42:p1",
                        "working",
                        None,
                        &[("role", "lead"), ("repo", "rondo")],
                    ),
                    pane("w42:pX", "idle", Some("old"), &[("owner", "w42:p1")]),
                    pane("w42:pZ", "idle", Some("orphan"), &[("owner", "w42:pP")]),
                ],
            ),
            report(
                "netcup",
                vec![pane(
                    "w3:p2",
                    "working",
                    Some("unit-a"),
                    &[("lead", "lead:rondo")],
                )],
            ),
        ];
        let views = leads(&reports);
        assert_eq!(views.len(), 2);
        let rondo = views.iter().find(|v| v.lead == "lead:rondo").unwrap();
        assert_eq!(rondo.pane.as_deref(), Some("w42:p1"));
        let names: Vec<&str> = rondo.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["old", "unit-a"]);
        assert_eq!(rondo.children[1].host, "netcup");
        assert_eq!(tally(&rondo.children), "1 idle 1 working");
        let orphan = views.iter().find(|v| v.lead == "w42:pP").unwrap();
        assert!(orphan.pane.is_none());
    }
}
