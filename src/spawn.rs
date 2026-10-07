//! `wrangle spawn`: the whole flow for harnesses that are not Pi. Admit (or
//! resume a ticket), get a pane on the admitted host, start the agent there,
//! mark it, and submit the prompt.

use serde::{Deserialize, Serialize};

use crate::commands::{self, AdmitOutput};
use crate::error::{Error, Result};
use crate::herdr;
use crate::hook::{self, HookInputs};
use crate::hosts::{Fleet, Host};

#[derive(Debug, Clone)]
pub struct SpawnRequest<'a> {
    /// Resume this queued ticket instead of admitting a new one.
    pub ticket: Option<&'a str>,
    /// Who is spawning; required without a ticket.
    pub lead: Option<&'a str>,
    /// Optional host pin.
    pub machine: Option<&'a str>,
    pub kind: &'a str,
    pub name: &'a str,
    pub label: Option<&'a str>,
    pub message: &'a str,
    pub branch: Option<&'a str>,
    pub base: Option<&'a str>,
    pub repo: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub workspace: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spawned {
    pub spawned: bool,
    pub machine: String,
    pub host: Host,
    pub pane_id: String,
    pub workspace_id: Option<String>,
    pub name: String,
    pub ticket: String,
    /// `working` after the prompt went in; `blocked` when the agent sat on a
    /// startup dialog and the prompt was held back.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SpawnOutput {
    Spawned(Box<Spawned>),
    Queued(commands::Queued),
}

pub fn spawn(fleet: &Fleet, req: &SpawnRequest<'_>) -> Result<SpawnOutput> {
    let lead = match (req.ticket, req.lead) {
        (Some(_), _) => "",
        (None, Some(l)) => l,
        (None, None) => return Err(Error::Invalid("spawn needs --lead or --ticket".into())),
    };
    let admitted = match commands::admit(fleet, lead, req.machine, req.ticket)? {
        AdmitOutput::Queued(q) => return Ok(SpawnOutput::Queued(q)),
        AdmitOutput::Admitted(a) => *a,
    };
    let host = &admitted.host;
    let ticket = &admitted.ticket;
    let machine = host.machine();
    let lead = admitted.lead.as_str();

    // A pane at a shell prompt on the admitted host.
    let placed = match place(fleet, host, req) {
        Ok(p) => p,
        Err(e) => return Err(release_and(fleet, ticket, e)),
    };

    let started =
        match herdr::agent_start(fleet.shell, machine, req.name, req.kind, &placed.pane_id) {
            Ok(s) => s,
            Err(e) => {
                if let Some(location) = &placed.location {
                    let _ = herdr::close_location(fleet.shell, machine, location);
                }
                return Err(release_and(fleet, ticket, e));
            }
        };
    // Attach the pane to the reservation and mark it; a failed mark is not fatal.
    let _ = commands::mark(
        fleet,
        host.machine(),
        &placed.pane_id,
        lead,
        req.name,
        Some(ticket),
        false,
    );
    let (status, next) = match started {
        herdr::Started::Ready => {
            if let Err(e) = herdr::agent_prompt(fleet.shell, machine, req.name, req.message) {
                return Err(release_and(fleet, ticket, e));
            }
            ("working", None)
        }
        herdr::Started::Blocked => (
            "blocked",
            Some(format!(
                "{} is blocked on a startup dialog; read the pane, answer it, then \
                 `herdr{} agent prompt {} <message>`",
                req.name,
                machine
                    .map(|m| format!(" --machine {m}"))
                    .unwrap_or_default(),
                req.name
            )),
        ),
    };
    Ok(SpawnOutput::Spawned(Box::new(Spawned {
        spawned: true,
        machine: host.id.clone(),
        host: host.clone(),
        pane_id: placed.pane_id,
        workspace_id: placed.workspace_id,
        name: req.name.to_string(),
        ticket: ticket.clone(),
        status: status.to_string(),
        next,
    })))
}

struct Placed {
    pane_id: String,
    workspace_id: Option<String>,
    /// Set when we created the tab or workspace ourselves (closed on failure).
    location: Option<herdr::Location>,
}

/// The `prepare` hook when a branch is requested, else a new tab or workspace.
fn place(fleet: &Fleet, host: &Host, req: &SpawnRequest<'_>) -> Result<Placed> {
    let cwd = std::env::current_dir()?.display().to_string();
    if let Some(branch) = req.branch {
        let inputs = HookInputs {
            repo: req.repo.unwrap_or(&cwd),
            branch,
            base: req.base.unwrap_or(""),
        };
        let p = hook::run(fleet, host, inputs)?;
        return Ok(Placed {
            pane_id: p.pane_id,
            workspace_id: p.workspace_id,
            location: None,
        });
    }
    let label = req.label.unwrap_or(req.name);
    let l = herdr::create_location(
        fleet.shell,
        host.machine(),
        req.workspace,
        req.cwd.unwrap_or(&cwd),
        label,
    )?;
    Ok(Placed {
        pane_id: l.pane_id.clone(),
        workspace_id: Some(l.workspace_id.clone()),
        location: Some(l),
    })
}

fn release_and(fleet: &Fleet, ticket: &str, error: Error) -> Error {
    let _ = commands::release_ticket(fleet, ticket);
    error
}
