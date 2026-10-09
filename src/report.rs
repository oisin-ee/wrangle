//! Reports from this lead host's retained lifecycle stream. No fleet probe needed.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::ledger::{Event, EventKind, Ledger, now_ms};

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub tickets: Vec<TicketReport>,
    pub hosts: Vec<HostRefusals>,
}

#[derive(Debug, Serialize)]
pub struct TicketReport {
    pub ticket: String,
    pub lead: String,
    pub host: Option<String>,
    pub pane: Option<String>,
    pub event: EventKind,
    pub queue_wait_ms: Option<u64>,
    pub run_time_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct HostRefusals {
    pub host: String,
    pub count: usize,
    pub reasons: BTreeMap<String, usize>,
}

/// Whole-number durations with an explicit unit. Reject overflow and bare numbers.
pub fn duration_ms(value: &str) -> std::result::Result<u64, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let multiplier = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return Err("duration needs a unit: ms, s, m, h, or d (for example 24h)".into()),
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .ok_or_else(|| "duration must be a nonnegative whole number within range".into())
}

pub fn report(ledger: &Ledger, since_ms: Option<u64>) -> Result<Report> {
    let events = ledger.with_lock(Ledger::events)?;
    let cutoff = since_ms.map_or(0, |since| now_ms().saturating_sub(since));
    summarize(&events, cutoff)
}

/// Select tickets with activity since the cutoff, but pair against all retained
/// events so a queue wait that began before the cutoff stays measurable.
fn summarize(events: &[Event], cutoff: u64) -> Result<Report> {
    let mut tickets: BTreeMap<&str, Vec<&Event>> = BTreeMap::new();
    let mut hosts: BTreeMap<String, HostRefusals> = BTreeMap::new();
    for event in events {
        tickets.entry(&event.ticket).or_default().push(event);
        if event.ts_ms >= cutoff {
            for (host, reason) in &event.refusals {
                let totals = hosts.entry(host.clone()).or_insert_with(|| HostRefusals {
                    host: host.clone(),
                    count: 0,
                    reasons: BTreeMap::new(),
                });
                totals.count += 1;
                *totals.reasons.entry(reason.clone()).or_default() += 1;
            }
        }
    }
    let mut report = Report {
        tickets: Vec::new(),
        hosts: hosts.into_values().collect(),
    };
    for history in tickets.values() {
        let Some(last) = history.last() else { continue };
        if !history.iter().any(|e| e.ts_ms >= cutoff) {
            continue;
        }
        let timestamp = |kind| history.iter().find(|e| e.event == kind).map(|e| e.ts_ms);
        let queued = timestamp(EventKind::Queued);
        let admitted = timestamp(EventKind::Admitted);
        let duration = |start: Option<u64>, end: Option<u64>| -> Result<Option<u64>> {
            match (start, end) {
                (Some(start), Some(end)) => end.checked_sub(start).map(Some).ok_or_else(|| {
                    Error::Invalid(format!("event timestamps reversed for {}", last.ticket))
                }),
                _ => Ok(None),
            }
        };
        report.tickets.push(TicketReport {
            ticket: last.ticket.clone(),
            lead: last.lead.clone(),
            host: history.iter().rev().find_map(|e| e.host.clone()),
            pane: history.iter().rev().find_map(|e| e.pane.clone()),
            event: last.event,
            // Immediate admission has no queued event and no queue wait.
            queue_wait_ms: duration(queued.or(admitted), admitted)?,
            run_time_ms: duration(
                timestamp(EventKind::Spawned),
                timestamp(EventKind::Released),
            )?,
        });
    }
    Ok(report)
}

pub fn render(report: &Report) -> String {
    let mut out = String::from(
        "ticket                 lead             host         queue wait (ms)  run time (ms)\n",
    );
    for ticket in &report.tickets {
        let display = |value: Option<u64>| value.map_or_else(|| "-".into(), |ms| ms.to_string());
        let _ = writeln!(
            out,
            "{:<22} {:<16} {:<12} {:>15}  {:>13}",
            ticket.ticket,
            ticket.lead,
            ticket.host.as_deref().unwrap_or("-"),
            display(ticket.queue_wait_ms),
            display(ticket.run_time_ms)
        );
    }
    let _ = writeln!(out, "\nhost refusals (queued admission attempts):");
    for host in &report.hosts {
        let _ = writeln!(out, "{}: {}", host.host, host.count);
        for (reason, count) in &host.reasons {
            let _ = writeln!(out, "  {count}: {reason}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_require_units_and_reject_overflow() {
        for (input, ms) in [
            ("2ms", 2),
            ("3s", 3000),
            ("4m", 240_000),
            ("1h", 3_600_000),
            ("1d", 86_400_000),
        ] {
            assert_eq!(duration_ms(input).unwrap(), ms);
        }
        for input in ["", "20", "1w", "-1h", "1.5h", "18446744073709551615d"] {
            assert!(duration_ms(input).is_err(), "{input}");
        }
    }

    #[test]
    fn since_keeps_earlier_duration_endpoints_but_filters_refusals() {
        let mut queued = Event::new("t", "lead", EventKind::Queued);
        queued.ts_ms = 100;
        queued.refusals.insert("local".into(), "disk".into());
        let mut retried = queued.clone();
        retried.ts_ms = 200;
        let mut admitted = Event::new("t", "lead", EventKind::Admitted);
        admitted.ts_ms = 300;
        let report = summarize(&[queued, retried, admitted], 150).unwrap();
        assert_eq!(report.tickets[0].queue_wait_ms, Some(200));
        assert_eq!(report.tickets[0].run_time_ms, None);
        assert_eq!(report.hosts[0].count, 1);
    }
}
