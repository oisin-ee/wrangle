//! The admission rule, with no I/O. `evaluate` scores one host; `choose`
//! picks the host with the most headroom; `plan` combines both.

use serde::{Deserialize, Serialize};

use crate::config::Thresholds;
use crate::probe::Probe;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Headroom {
    pub host: String,
    /// `load_per_core_max × cores − load1 − reservation_load × reservations`.
    pub headroom: f64,
    pub eligible: bool,
    /// Why the host is not eligible, when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub live_agents: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case")]
pub enum Plan {
    Admit { host: String },
    Full,
}

#[must_use]
pub fn evaluate(p: &Probe, t: &Thresholds) -> Headroom {
    let reserved = t.reservation_load * p.reservations.len() as f64;
    let headroom = t.load_per_core_max * f64::from(p.cores) - p.load1 - reserved;
    let reason = if p.disk_free_percent < t.disk_free_min_percent {
        Some(format!(
            "disk {:.1}% free < {:.1}%",
            p.disk_free_percent, t.disk_free_min_percent
        ))
    } else if headroom <= 0.0 {
        Some(format!(
            "load {:.2} + {} reserved ≥ {:.2} × {} cores",
            p.load1,
            p.reservations.len(),
            t.load_per_core_max,
            p.cores
        ))
    } else {
        None
    };
    Headroom {
        host: p.host.clone(),
        headroom,
        eligible: reason.is_none(),
        reason,
        live_agents: p.agents.live(),
    }
}

/// Largest positive headroom; ties go to the host with fewer live agents,
/// then to the lexically smaller host id so the choice is deterministic.
#[must_use]
pub fn choose(evals: &[Headroom]) -> Option<&Headroom> {
    evals.iter().filter(|h| h.eligible).min_by(|a, b| {
        b.headroom
            .total_cmp(&a.headroom)
            .then(a.live_agents.cmp(&b.live_agents))
            .then(a.host.cmp(&b.host))
    })
}

#[must_use]
pub fn plan(probes: &[Probe], t: &Thresholds) -> Plan {
    let evals: Vec<Headroom> = probes.iter().map(|p| evaluate(p, t)).collect();
    match choose(&evals) {
        Some(h) => Plan::Admit {
            host: h.host.clone(),
        },
        None => Plan::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Reservation;
    use crate::probe::AgentCounts;

    fn thresholds() -> Thresholds {
        Thresholds {
            load_per_core_max: 1.5,
            disk_free_min_percent: 15.0,
            reservation_load: 1.0,
            reservation_ttl_ms: 180_000,
        }
    }

    fn probe(host: &str, load1: f64, cores: u32, disk: f64, reservations: usize) -> Probe {
        Probe {
            host: host.to_string(),
            label: host.to_string(),
            load1,
            cores,
            disk_free_percent: disk,
            agents: AgentCounts::default(),
            reservations: (0..reservations)
                .map(|i| Reservation {
                    ticket: format!("t{i}"),
                    lead: "lead".to_string(),
                    created_ms: 0,
                    pane: None,
                })
                .collect(),
            panes: Vec::new(),
        }
    }

    #[test]
    fn headroom_formula() {
        let h = evaluate(&probe("a", 4.0, 8, 50.0, 2), &thresholds());
        // 1.5 × 8 − 4 − 2 = 6
        assert!((h.headroom - 6.0).abs() < 1e-9);
        assert!(h.eligible);
    }

    #[test]
    fn disk_rule_blocks_before_load() {
        let h = evaluate(&probe("a", 0.0, 8, 10.0, 0), &thresholds());
        assert!(!h.eligible);
        assert!(h.reason.unwrap().starts_with("disk"));
    }

    #[test]
    fn load_at_limit_is_full() {
        let h = evaluate(&probe("a", 12.0, 8, 50.0, 0), &thresholds());
        assert!(!h.eligible);
        assert!(h.reason.unwrap().starts_with("load"));
    }

    #[test]
    fn reservations_count_as_load() {
        let t = thresholds();
        assert!(evaluate(&probe("a", 10.0, 8, 50.0, 1), &t).eligible);
        assert!(!evaluate(&probe("a", 10.0, 8, 50.0, 2), &t).eligible);
    }

    #[test]
    fn picks_most_headroom_then_fewer_agents_then_id() {
        let t = thresholds();
        let mut probes = vec![
            probe("b", 2.0, 8, 50.0, 0), // 10
            probe("a", 2.0, 8, 50.0, 0), // 10
            probe("c", 6.0, 8, 50.0, 0), // 6
        ];
        assert_eq!(plan(&probes, &t), Plan::Admit { host: "a".into() });
        probes[1].agents = AgentCounts {
            total: 3,
            by_status: [("working".to_string(), 3)].into_iter().collect(),
        };
        assert_eq!(plan(&probes, &t), Plan::Admit { host: "b".into() });
    }

    #[test]
    fn full_when_no_host_is_eligible() {
        let t = thresholds();
        let probes = vec![probe("a", 30.0, 8, 50.0, 0), probe("b", 1.0, 8, 5.0, 0)];
        assert_eq!(plan(&probes, &t), Plan::Full);
        assert_eq!(plan(&[], &t), Plan::Full);
    }

    #[test]
    fn balances_as_reservations_accumulate() {
        // Two equal hosts: successive admissions alternate because each
        // reservation lowers that host's headroom by one.
        let t = thresholds();
        let mut a = probe("a", 2.0, 8, 50.0, 0);
        let mut b = probe("b", 2.0, 8, 50.0, 0);
        let mut picks = Vec::new();
        for i in 0..4 {
            let Plan::Admit { host } = plan(&[a.clone(), b.clone()], &t) else {
                unreachable!("two idle hosts cannot be full");
            };
            let r = Reservation {
                ticket: format!("t{i}"),
                lead: "l".into(),
                created_ms: 0,
                pane: None,
            };
            if host == "a" {
                a.reservations.push(r);
            } else {
                b.reservations.push(r);
            }
            picks.push(host);
        }
        assert_eq!(picks, ["a", "b", "a", "b"]);
    }
}
