//! The lead-side commands: each one orchestrates the fleet primitives and
//! returns a serialisable result that `main` prints as JSON or text.

use std::collections::HashSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::herdr;
use crate::hosts::{Fleet, Host, HostReport};
use crate::lead::{self, LeadView};
use crate::ledger::{Event, EventKind, Ledger, QueueEntry, new_ticket, now_ms};
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
    /// Leads first: who orchestrates what, across every host.
    #[serde(default)]
    pub leads: Vec<LeadView>,
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
                        fleet.ledger.with_lock(|l| {
                            let mut event = Event::new(&ticket, &lead, EventKind::Admitted);
                            event.host = Some(host.id.clone());
                            l.append_event(&event)
                        })?;
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

    queue_admission(fleet.ledger, ticket, &lead, pin.as_deref(), reports)
}

fn queue_admission(
    ledger: &Ledger,
    ticket: String,
    lead: &str,
    pin: Option<&str>,
    reports: Vec<HostReport>,
) -> Result<AdmitOutput> {
    let reason = full_reason(&reports);
    ledger.with_lock(|l| {
        // Retrying a full host must not reset the original queue wait.
        let created_ms = l
            .queue()?
            .iter()
            .find(|e| e.ticket == ticket)
            .map_or_else(now_ms, |e| e.created_ms);
        l.enqueue(QueueEntry {
            ticket: ticket.clone(),
            lead: lead.to_string(),
            created_ms,
            machine: pin.map(ToString::to_string),
        })?;
        let mut event = Event::new(&ticket, lead, EventKind::Queued);
        event.refusals = reports
            .iter()
            .map(|r| (r.host.id.clone(), refusal_reason(r)))
            .collect();
        l.append_event(&event)
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
        panes: Vec::new(),
    }
}

fn full_reason(reports: &[HostReport]) -> String {
    let mut parts: Vec<String> = reports
        .iter()
        .map(|r| {
            let why = refusal_reason(r);
            format!("{}: {why}", r.host.label)
        })
        .collect();
    if parts.is_empty() {
        parts.push("no eligible host".to_string());
    }
    parts.join("; ")
}

fn refusal_reason(report: &HostReport) -> String {
    report
        .error
        .clone()
        .or_else(|| report.headroom.as_ref().and_then(|h| h.reason.clone()))
        .unwrap_or_else(|| "lost the reservation race".to_string())
}

/// Drop a ticket's reservation on every host and its queue entry here.
pub fn release_ticket(fleet: &Fleet, ticket: &str) -> Result<Released> {
    finish_ticket(fleet, ticket, EventKind::Released)
}

fn finish_ticket(fleet: &Fleet, ticket: &str, kind: EventKind) -> Result<Released> {
    let prior = fleet
        .ledger
        .with_lock(|l| Ok(l.events()?.into_iter().rev().find(|e| e.ticket == ticket)))?;
    // Do not claim a run ended if its known host refused the release.
    let released = if let Some(host) = prior.as_ref().and_then(|e| e.host.as_deref()) {
        let known = fleet.release_ticket(fleet.find(host)?, ticket)?.released;
        known + fleet.release_ticket_everywhere(ticket)
    } else {
        fleet.release_ticket_everywhere(ticket)
    };
    fleet.ledger.with_lock(|l| {
        let entry = l.dequeue(ticket)?;
        let prior = l.events()?.into_iter().rev().find(|e| e.ticket == ticket);
        // History survives reservation TTL, so a late release still ends the run.
        let active = prior.as_ref().is_some_and(|e| !terminal(e.event));
        if kind == EventKind::Cancelled && released == 0 && entry.is_none() && !active {
            return Err(Error::UnknownTicket(ticket.to_string()));
        }
        if let Some(mut event) = prior
            .filter(|e| !terminal(e.event))
            .or_else(|| entry.as_ref().map(|q| Event::new(ticket, &q.lead, kind)))
        {
            event.ts_ms = now_ms();
            event.event = kind;
            event.refusals.clear();
            l.append_event(&event)?;
        }
        Ok(Released {
            released,
            dequeued: entry.is_some(),
        })
    })
}

fn terminal(event: EventKind) -> bool {
    matches!(event, EventKind::Released | EventKind::Cancelled)
}

fn finish_pane(ledger: &Ledger, host: &str, pane: &str) -> Result<()> {
    ledger.with_lock(|l| {
        let mut seen = HashSet::new();
        for mut event in l.events()?.into_iter().rev() {
            if seen.insert(event.ticket.clone())
                && !terminal(event.event)
                && event.host.as_deref() == Some(host)
                && event.pane.as_deref() == Some(pane)
            {
                event.ts_ms = now_ms();
                event.event = EventKind::Released;
                event.refusals.clear();
                l.append_event(&event)?;
            }
        }
        Ok(())
    })
}

/// Drop the reservation attached to a pane on one host (default: this one).
pub fn release_pane(fleet: &Fleet, pane: &str, machine: Option<&str>) -> Result<Released> {
    let host = match machine {
        Some(m) => fleet.find(m)?,
        None => fleet.find(crate::hosts::LOCAL)?,
    };
    let out = fleet.release_pane(host, pane)?;
    finish_pane(fleet.ledger, &host.id, pane)?;
    Ok(Released {
        released: out.released,
        dequeued: false,
    })
}

pub fn cancel(fleet: &Fleet, ticket: &str) -> Result<Cancelled> {
    let r = finish_ticket(fleet, ticket, EventKind::Cancelled)?;
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
        leads: lead::leads(&reports),
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
        fleet.ledger.with_lock(|l| {
            l.dequeue(t)?;
            if !l
                .events()?
                .iter()
                .any(|e| e.ticket == t && e.event == EventKind::Spawned)
            {
                let mut event = Event::new(t, lead, EventKind::Spawned);
                event.host = Some(host.id.clone());
                event.pane = Some(pane.to_string());
                l.append_event(&event)?;
            }
            Ok(())
        })?;
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
    render_leads(&mut out, &s.leads);
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

fn render_leads(out: &mut String, leads: &[LeadView]) {
    if leads.is_empty() {
        let _ = writeln!(out, "leads: none\n");
        return;
    }
    for l in leads {
        let head = match (&l.repo, &l.pane) {
            (Some(repo), Some(pane)) => format!(
                "{} {repo}  {pane} on {} ({})",
                herdr::LEAD_MARK,
                l.host.as_deref().unwrap_or("?"),
                l.status.as_deref().unwrap_or("unknown")
            ),
            _ => format!("? {}  (no lead pane)", l.lead),
        };
        let kids = if l.children.is_empty() {
            "no children".to_string()
        } else {
            format!(
                "{} children: {}",
                l.children.len(),
                lead::tally(&l.children)
            )
        };
        let _ = writeln!(out, "{head} · {kids}");
        for c in &l.children {
            let _ = writeln!(
                out,
                "  ↳ {:<24} {:<10} {} on {}",
                c.name, c.status, c.pane, c.host
            );
        }
    }
    out.push('\n');
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
        let history = rig.ledger.events().unwrap();
        let history: Vec<_> = history
            .iter()
            .filter(|e| e.ticket == queued.ticket)
            .collect();
        assert_eq!(
            history.iter().map(|e| e.event).collect::<Vec<_>>(),
            [EventKind::Queued, EventKind::Admitted]
        );
        let report = crate::report::report(&rig.ledger, None).unwrap();
        let row = report
            .tickets
            .iter()
            .find(|r| r.ticket == queued.ticket)
            .unwrap();
        assert_eq!(row.queue_wait_ms, Some(history[1].ts_ms - history[0].ts_ms));
        let st = status(&fleet).unwrap();
        assert_eq!(st.queue[0].state, "reserved");
        assert_eq!(st.queue[0].host.as_deref(), Some("local"));

        // mark attaches the pane and dequeues; cancel on a gone ticket fails.
        rig.shell.on(
            "herdr pane report-metadata w1:p2 --source wrangle --display-agent ↳ reviewer \
             --token sub=● --token lead=lead-b",
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
    fn lifecycle_records_three_events_and_reports_durations() {
        let rig = rig("lifecycle", "{ 0 0 0 }");
        let fleet = fleet(&rig);
        let AdmitOutput::Admitted(admitted) = admit(&fleet, "lead", Some("local"), None).unwrap()
        else {
            unreachable!("expected admitted");
        };
        rig.shell.on(
            "herdr pane report-metadata w1:p2 --source wrangle --display-agent ↳ child --token sub=● --token lead=lead",
            "",
        );
        for _ in 0..2 {
            mark(
                &fleet,
                None,
                "w1:p2",
                "lead",
                "child",
                Some(&admitted.ticket),
                false,
            )
            .unwrap();
        }
        release_pane(&fleet, "w1:p2", None).unwrap();
        release_ticket(&fleet, &admitted.ticket).unwrap();
        let events = rig.ledger.events().unwrap();
        assert_eq!(
            events.iter().map(|e| e.event).collect::<Vec<_>>(),
            [EventKind::Admitted, EventKind::Spawned, EventKind::Released]
        );
        assert!(
            events
                .iter()
                .all(|e| e.ticket == admitted.ticket && e.lead == "lead")
        );
        assert_eq!(events[2].pane.as_deref(), Some("w1:p2"));
        let report = crate::report::report(&rig.ledger, None).unwrap();
        let ticket = &report.tickets[0];
        assert_eq!(ticket.queue_wait_ms, Some(0));
        let runtime = events[2].ts_ms - events[1].ts_ms;
        assert_eq!(ticket.run_time_ms, Some(runtime));
        let text = crate::report::render(&report);
        assert!(text.contains("queue wait (ms)"));
        assert!(text.contains(&admitted.ticket));
        assert!(text.lines().nth(1).unwrap().ends_with(&runtime.to_string()));
    }

    #[test]
    fn disk_and_load_refusals_appear_in_status_and_report() {
        let rig = rig("refusals", "{ 0 0 0 }");
        rig.shell.on("df -Pk /tmp/home", "FS 1000 863 137 87% /\n");
        let fleet = fleet(&rig);
        rig.shell.on(
            &remote_key(&fleet, &["probe"]),
            &remote_probe_body(1000.0, "[]"),
        );
        let AdmitOutput::Queued(queued) = admit(&fleet, "lead", None, None).unwrap() else {
            unreachable!("expected queued");
        };
        let created = rig.ledger.queue().unwrap()[0].created_ms;
        admit(&fleet, "ignored", None, Some(&queued.ticket)).unwrap();
        assert_eq!(rig.ledger.queue().unwrap()[0].created_ms, created);
        let status = status(&fleet).unwrap();
        let json = serde_json::to_value(&status).unwrap();
        let text = render_status(&status);
        for (index, host) in status.hosts.iter().enumerate() {
            let h = host.headroom.as_ref().unwrap();
            assert!(!h.eligible);
            let reason = h.reason.as_ref().unwrap();
            assert!(text.contains(reason));
            assert_eq!(json["hosts"][index]["headroom"]["reason"], *reason);
        }
        let report = crate::report::report(&rig.ledger, None).unwrap();
        assert_eq!(report.tickets[0].queue_wait_ms, None);
        let local = report.hosts.iter().find(|h| h.host == "local").unwrap();
        assert_eq!(local.count, 2);
        assert_eq!(local.reasons["disk 13.7% free < 15.0%"], 2);
        assert!(crate::report::render(&report).contains("disk 13.7% free < 15.0%"));
        assert!(
            serde_json::to_string(&report)
                .unwrap()
                .contains("disk 13.7% free < 15.0%")
        );
        cancel(&fleet, &queued.ticket).unwrap();
        let events = rig.ledger.events().unwrap();
        assert_eq!(events.last().unwrap().event, EventKind::Cancelled);
        assert!(!events.iter().any(|e| e.event == EventKind::Released));
        assert!(cancel(&fleet, &queued.ticket).is_err());
    }

    #[test]
    fn failed_remote_release_does_not_end_the_run() {
        let rig = rig("failed-release", "{ 0 0 0 }");
        let fleet = fleet(&rig);
        rig.ledger
            .with_lock(|l| {
                let mut event = Event::new("remote", "lead", EventKind::Spawned);
                event.host = Some("8103".into());
                event.pane = Some("w1:p2".into());
                l.append_event(&event)
            })
            .unwrap();
        // No remote release response is registered: the host is unreachable.
        assert!(release_ticket(&fleet, "remote").is_err());
        assert!(cancel(&fleet, "remote").is_err());
        assert_eq!(rig.ledger.events().unwrap().len(), 1);
        assert_eq!(
            crate::report::report(&rig.ledger, None).unwrap().tickets[0].run_time_ms,
            None
        );
    }

    #[test]
    fn release_after_reservation_expiry_still_finishes_the_run() {
        let mut rig = rig("expired-run", "{ 0 0 0 }");
        rig.policy.reservation_ttl_ms = 0;
        let fleet = fleet(&rig);
        rig.ledger
            .with_lock(|l| {
                let mut event = Event::new("expired", "lead", EventKind::Spawned);
                event.host = Some("local".into());
                event.pane = Some("w1:p2".into());
                l.append_event(&event)
            })
            .unwrap();
        assert_eq!(release_pane(&fleet, "w1:p2", None).unwrap().released, 0);
        assert_eq!(rig.ledger.events().unwrap()[1].event, EventKind::Released);
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
            leads: vec![LeadView {
                lead: "lead:rondo".into(),
                repo: Some("rondo".into()),
                pane: Some("w42:p1".into()),
                host: Some("local".into()),
                status: Some("working".into()),
                children: vec![lead::ChildView {
                    pane: "w42:pX".into(),
                    host: "netcup".into(),
                    name: "unit-a".into(),
                    status: "idle".into(),
                }],
            }],
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
        assert!(text.starts_with("⌂ rondo  w42:p1 on local (working) · 1 children: 1 idle\n"));
        assert!(text.contains("  ↳ unit-a"));
        assert!(text.contains("error: herdr: no server"));
        assert!(text.contains("w-1"));
        assert!(text.contains("any"));
    }
}
