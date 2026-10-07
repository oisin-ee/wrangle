//! The lead-side commands: each one orchestrates the fleet primitives and
//! returns a serialisable result that `main` prints as JSON or text.

use std::collections::HashSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::herdr;
use crate::hosts::{Fleet, Host, HostReport};
use crate::ledger::{QueueEntry, new_ticket, now_ms};
use crate::plan::{Headroom, Plan, plan};
use crate::probe::Probe;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Admitted {
    pub admitted: bool,
    pub ticket: String,
    pub lead: String,
    pub host: Host,
    pub probe: Probe,
    pub headroom: Headroom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    pub queued: bool,
    pub ticket: String,
    pub hosts: Vec<HostReport>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AdmitOutput {
    Admitted(Box<Admitted>),
    Queued(Queued),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Released {
    pub released: usize,
    pub dequeued: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancelled {
    pub cancelled: bool,
    pub ticket: String,
    pub released: usize,
    pub dequeued: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueStatus {
    #[serde(flatten)]
    pub entry: QueueEntry,
    /// `queued` while no host holds a reservation for it; `reserved` after.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub age_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub hosts: Vec<HostReport>,
    pub queue: Vec<QueueStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marked {
    pub marked: bool,
    pub pane: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reported {
    pub reported: bool,
    pub pane: String,
    pub count: usize,
}

pub fn probe(fleet: &Fleet, machine: Option<&str>) -> Result<Vec<HostReport>> {
    let hosts = fleet.eligible(machine)?;
    Ok(fleet.probe_all(&hosts))
}

/// Admit on the host with the most headroom, or queue a ticket. With
/// `ticket`, retry a queued ticket (idempotent once a reservation exists).
pub fn admit(
    fleet: &Fleet,
    lead: &str,
    machine: Option<&str>,
    ticket: Option<&str>,
) -> Result<AdmitOutput> {
    let (ticket, lead, pin) = match ticket {
        Some(t) => {
            let entry = fleet
                .ledger
                .queue()?
                .into_iter()
                .find(|e| e.ticket == t)
                .ok_or_else(|| Error::UnknownTicket(t.to_string()))?;
            let pin = machine.map(ToString::to_string).or(entry.machine);
            (entry.ticket, entry.lead, pin)
        }
        None => (
            new_ticket(),
            lead.to_string(),
            machine.map(ToString::to_string),
        ),
    };
    let candidates = fleet.eligible(pin.as_deref())?;
    let mut reports = fleet.probe_all(&candidates);

    // A reservation already made for this ticket (an earlier poll) wins.
    if let Some(r) = reports.iter().find(|r| {
        r.probe
            .as_ref()
            .is_some_and(|p| p.reservations.iter().any(|res| res.ticket == ticket))
    }) {
        return Ok(AdmitOutput::Admitted(Box::new(Admitted {
            admitted: true,
            ticket,
            lead,
            host: r.host.clone(),
            probe: r.probe.clone().unwrap_or_else(|| empty_probe(&r.host)),
            headroom: r.headroom.clone().unwrap_or_else(|| Headroom {
                host: r.host.id.clone(),
                headroom: 0.0,
                eligible: true,
                reason: None,
                live_agents: 0,
            }),
        })));
    }

    let thresholds = fleet.thresholds();
    let mut lost: HashSet<String> = HashSet::new();
    loop {
        let probes: Vec<Probe> = reports
            .iter()
            .filter(|r| !lost.contains(&r.host.id))
            .filter_map(|r| r.probe.clone())
            .collect();
        match plan(&probes, &thresholds) {
            Plan::Admit { host } => {
                let host = candidates
                    .iter()
                    .find(|h| h.id == host)
                    .copied()
                    .ok_or_else(|| Error::UnknownMachine(host.clone()))?;
                match fleet.reserve(host, &lead, &ticket) {
                    Ok(outcome) if outcome.reserved => {
                        return Ok(AdmitOutput::Admitted(Box::new(Admitted {
                            admitted: true,
                            ticket,
                            lead,
                            host: host.clone(),
                            probe: outcome.probe,
                            headroom: outcome.headroom,
                        })));
                    }
                    Ok(outcome) => {
                        // Lost the race under the host lock; show the fresh probe.
                        if let Some(r) = reports.iter_mut().find(|r| r.host.id == host.id) {
                            r.headroom = Some(outcome.headroom);
                            r.probe = Some(outcome.probe);
                        }
                        lost.insert(host.id.clone());
                    }
                    Err(e) => {
                        if let Some(r) = reports.iter_mut().find(|r| r.host.id == host.id) {
                            r.ok = false;
                            r.error = Some(e.to_string());
                        }
                        lost.insert(host.id.clone());
                    }
                }
            }
            Plan::Full => break,
        }
    }

    let reason = full_reason(&reports);
    fleet.ledger.with_lock(|l| {
        l.enqueue(QueueEntry {
            ticket: ticket.clone(),
            lead: lead.clone(),
            created_ms: now_ms(),
            machine: pin.clone(),
        })
    })?;
    Ok(AdmitOutput::Queued(Queued {
        queued: true,
        ticket,
        hosts: reports,
        reason,
    }))
}

fn empty_probe(host: &Host) -> Probe {
    Probe {
        host: host.id.clone(),
        label: host.label.clone(),
        load1: 0.0,
        cores: 0,
        disk_free_percent: 0.0,
        agents: crate::probe::AgentCounts::default(),
        reservations: Vec::new(),
    }
}

fn full_reason(reports: &[HostReport]) -> String {
    let mut parts: Vec<String> = reports
        .iter()
        .map(|r| {
            let why = r
                .error
                .clone()
                .or_else(|| r.headroom.as_ref().and_then(|h| h.reason.clone()))
                .unwrap_or_else(|| "lost the reservation race".to_string());
            format!("{}: {why}", r.host.label)
        })
        .collect();
    if parts.is_empty() {
        parts.push("no eligible host".to_string());
    }
    parts.join("; ")
}

/// Drop a ticket's reservation on every host and its queue entry here.
pub fn release_ticket(fleet: &Fleet, ticket: &str) -> Result<Released> {
    let released = fleet.release_ticket_everywhere(ticket);
    let dequeued = fleet.ledger.with_lock(|l| l.dequeue(ticket))?.is_some();
    Ok(Released { released, dequeued })
}

/// Drop the reservation attached to a pane on one host (default: this one).
pub fn release_pane(fleet: &Fleet, pane: &str, machine: Option<&str>) -> Result<Released> {
    let host = match machine {
        Some(m) => fleet.find(m)?,
        None => fleet.find(crate::hosts::LOCAL)?,
    };
    let out = fleet.release_pane(host, pane)?;
    Ok(Released {
        released: out.released,
        dequeued: false,
    })
}

pub fn cancel(fleet: &Fleet, ticket: &str) -> Result<Cancelled> {
    let r = release_ticket(fleet, ticket)?;
    if r.released == 0 && !r.dequeued {
        return Err(Error::UnknownTicket(ticket.to_string()));
    }
    Ok(Cancelled {
        cancelled: true,
        ticket: ticket.to_string(),
        released: r.released,
        dequeued: r.dequeued,
    })
}

pub fn status(fleet: &Fleet) -> Result<Status> {
    let hosts: Vec<&Host> = fleet.hosts.iter().collect();
    let reports = fleet.probe_all(&hosts);
    let now = now_ms();
    let queue = fleet
        .ledger
        .queue()?
        .into_iter()
        .map(|entry| {
            let host = reports.iter().find_map(|r| {
                r.probe
                    .as_ref()
                    .filter(|p| p.reservations.iter().any(|res| res.ticket == entry.ticket))
                    .map(|_| r.host.id.clone())
            });
            QueueStatus {
                age_ms: now.saturating_sub(entry.created_ms),
                state: if host.is_some() { "reserved" } else { "queued" }.to_string(),
                host,
                entry,
            }
        })
        .collect();
    Ok(Status {
        hosts: reports,
        queue,
    })
}

/// Mark a child pane in the sidebar. With `ticket`, attach the pane to its
/// reservation and drop the queue entry: the spawn succeeded.
pub fn mark(
    fleet: &Fleet,
    machine: Option<&str>,
    pane: &str,
    lead: &str,
    name: &str,
    ticket: Option<&str>,
    clear: bool,
) -> Result<Marked> {
    let host = match machine {
        Some(m) => fleet.find(m)?,
        None => fleet.find(crate::hosts::LOCAL)?,
    };
    if clear {
        herdr::unmark(fleet.shell, host.machine(), pane)?;
        return Ok(Marked {
            marked: false,
            pane: pane.to_string(),
            ticket: None,
        });
    }
    herdr::mark(fleet.shell, host.machine(), pane, lead, name)?;
    if let Some(t) = ticket {
        fleet.set_pane(host, t, pane)?;
        fleet.ledger.with_lock(|l| l.dequeue(t))?;
    }
    Ok(Marked {
        marked: true,
        pane: pane.to_string(),
        ticket: ticket.map(ToString::to_string),
    })
}

pub fn notify(fleet: &Fleet, title: &str, body: &str) -> Result<()> {
    herdr::notify(fleet.shell, title, body)
}

pub fn queue(fleet: &Fleet, pane: &str, count: usize) -> Result<Reported> {
    herdr::queue_count(fleet.shell, pane, count)?;
    Ok(Reported {
        reported: true,
        pane: pane.to_string(),
        count,
    })
}

/// One screen of `status` for humans and the `--watch` board.
#[must_use]
pub fn render_status(s: &Status) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<12} {:>7} {:>5} {:>6} {:>6} {:>5} {:>8}  state",
        "host", "load1", "cores", "disk%", "agents", "resv", "headroom"
    );
    for r in &s.hosts {
        match (&r.probe, &r.headroom) {
            (Some(p), Some(h)) => {
                let state = if h.eligible {
                    "ok".to_string()
                } else {
                    format!("full: {}", h.reason.clone().unwrap_or_default())
                };
                let _ = writeln!(
                    out,
                    "{:<12} {:>7.2} {:>5} {:>6.1} {:>6} {:>5} {:>8.2}  {state}",
                    r.host.label,
                    p.load1,
                    p.cores,
                    p.disk_free_percent,
                    p.agents.live(),
                    p.reservations.len(),
                    h.headroom,
                );
            }
            _ => {
                let _ = writeln!(
                    out,
                    "{:<12} {:>7} {:>5} {:>6} {:>6} {:>5} {:>8}  error: {}",
                    r.host.label,
                    "-",
                    "-",
                    "-",
                    "-",
                    "-",
                    "-",
                    r.error.clone().unwrap_or_default()
                );
            }
        }
    }
    if s.queue.is_empty() {
        let _ = writeln!(out, "\nqueue: empty");
    } else {
        let _ = writeln!(
            out,
            "\n{:<22} {:<16} {:<9} {:<12} {:>6}",
            "ticket", "lead", "state", "host", "age"
        );
        for q in &s.queue {
            let _ = writeln!(
                out,
                "{:<22} {:<16} {:<9} {:<12} {:>5}s",
                q.entry.ticket,
                q.entry.lead,
                q.state,
                q.host
                    .clone()
                    .or_else(|| q.entry.machine.clone())
                    .unwrap_or_else(|| "any".into()),
                q.age_ms / 1000
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Policy;
    use crate::ledger::Ledger;
    use crate::shell::Fake;

    const MACHINES: &str = r#"[{"id":"8103","label":"netcup","target":"netcup-dev","session":"default","enabled":true,"selected":false}]"#;

    fn remote_probe_body(load1: f64, reservations: &str) -> String {
        format!(
            r#"{{"host":"8103","label":"netcup","load1":{load1},"cores":16,"disk_free_percent":40.0,
                "agents":{{"total":1,"by_status":{{"working":1}}}},"reservations":{reservations}}}"#
        )
    }

    struct Rig {
        shell: Fake,
        ledger: Ledger,
        policy: Policy,
    }

    fn rig(name: &str, local_load: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("wrangle-cmd-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let shell = Fake::new();
        shell
            .on("herdr machine list --json", MACHINES)
            .with_load(local_load)
            .on("df -Pk /tmp/home", "FS 1000 500 400 60% /\n")
            .on("herdr agent list", r#"{"result":{"agents":[]}}"#);
        Rig {
            shell,
            ledger: Ledger::new(dir),
            policy: Policy::default(),
        }
    }

    fn fleet(r: &Rig) -> Fleet<'_> {
        Fleet::discover(&r.shell, &r.ledger, &r.policy, "/tmp/home".to_string()).unwrap()
    }

    fn remote_key(fleet: &Fleet, words: &[&str]) -> String {
        let netcup = fleet.find("netcup").unwrap();
        let argv = fleet.remote_argv(
            netcup,
            &words.iter().map(ToString::to_string).collect::<Vec<_>>(),
        );
        format!("ssh {}", argv.join(" "))
    }

    #[test]
    fn admit_prefers_the_host_with_more_headroom() {
        // Local: nearly full. Remote netcup: idle 16 cores.
        let cores = f64::from(crate::probe::cores());
        let rig = rig("prefer", &format!("{{ {:.2} 0 0 }}", 1.5 * cores - 0.5));
        let fleet = fleet(&rig);
        rig.shell.on(
            &remote_key(&fleet, &["probe"]),
            &remote_probe_body(1.0, "[]"),
        );
        // Without a pin the plan would choose netcup. The reserve call's key
        // embeds a random ticket, so the rest of the test pins local to
        // exercise the in-process path.
        let reports = probe(&fleet, None).unwrap();
        let probes: Vec<Probe> = reports.iter().filter_map(|r| r.probe.clone()).collect();
        assert_eq!(
            plan(&probes, &fleet.thresholds()),
            Plan::Admit {
                host: "8103".into()
            }
        );

        let AdmitOutput::Admitted(first) = admit(&fleet, "lead-a", Some("local"), None).unwrap()
        else {
            unreachable!("expected admitted")
        };
        assert!(first.host.is_local());
        assert!(first.ticket.starts_with("w-"));
        // Local now carries one reservation, so a second pinned admit is full.
        let AdmitOutput::Queued(queued) = admit(&fleet, "lead-b", Some("local"), None).unwrap()
        else {
            unreachable!("expected queued")
        };
        assert!(queued.reason.contains("local: load"));
        let st = status(&fleet).unwrap();
        assert_eq!(st.queue.len(), 1);
        assert_eq!(st.queue[0].state, "queued");

        // Retry the ticket after the first reservation is released.
        assert_eq!(release_ticket(&fleet, &first.ticket).unwrap().released, 1);
        let AdmitOutput::Admitted(retried) =
            admit(&fleet, "ignored", None, Some(&queued.ticket)).unwrap()
        else {
            unreachable!("expected admitted")
        };
        assert_eq!(retried.ticket, queued.ticket);
        assert!(retried.host.is_local(), "queued pin must be kept");
        // Idempotent: the same ticket is reported admitted without a new reservation.
        let again = admit(&fleet, "ignored", None, Some(&queued.ticket)).unwrap();
        assert!(matches!(again, AdmitOutput::Admitted(_)));
        let st = status(&fleet).unwrap();
        assert_eq!(st.queue[0].state, "reserved");
        assert_eq!(st.queue[0].host.as_deref(), Some("local"));

        // mark attaches the pane and dequeues; cancel on a gone ticket fails.
        rig.shell.on(
            "herdr pane report-metadata w1:p2 --source wrangle --display-agent ↳ reviewer \
             --token sub=● --token owner=lead-b",
            "",
        );
        let marked = mark(
            &fleet,
            None,
            "w1:p2",
            "lead-b",
            "reviewer",
            Some(&queued.ticket),
            false,
        )
        .unwrap();
        assert!(marked.marked);
        assert_eq!(status(&fleet).unwrap().queue.len(), 0);
        assert_eq!(release_pane(&fleet, "w1:p2", None).unwrap().released, 1);
        assert!(matches!(
            cancel(&fleet, &queued.ticket),
            Err(Error::UnknownTicket(_))
        ));
    }

    #[test]
    fn unknown_ticket_is_an_error() {
        let rig = rig("unknown", "{ 0 0 0 }");
        let fleet = fleet(&rig);
        assert!(matches!(
            admit(&fleet, "lead", None, Some("w-nope")),
            Err(Error::UnknownTicket(_))
        ));
    }

    #[test]
    fn render_status_shows_errors_and_queue() {
        let s = Status {
            hosts: vec![HostReport {
                host: Host::local(),
                ok: false,
                probe: None,
                headroom: None,
                error: Some("herdr: no server".into()),
            }],
            queue: vec![QueueStatus {
                entry: QueueEntry {
                    ticket: "w-1".into(),
                    lead: "lead".into(),
                    created_ms: 0,
                    machine: None,
                },
                state: "queued".into(),
                host: None,
                age_ms: 5_000,
            }],
        };
        let text = render_status(&s);
        assert!(text.contains("error: herdr: no server"));
        assert!(text.contains("w-1"));
        assert!(text.contains("any"));
    }
}
