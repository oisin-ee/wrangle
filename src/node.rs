//! Primitives that run on the host they describe. The lead calls them
//! in-process for its own machine and over ssh (`wrangle host …`) for the
//! others, so both paths share one implementation.

use serde::{Deserialize, Serialize};

use crate::config::Thresholds;
use crate::error::Result;
use crate::ledger::{Ledger, Reservation, now_ms};
use crate::plan::{Headroom, evaluate};
use crate::probe::{self, Probe};
use crate::shell::Shell;

#[derive(Debug, Clone, Copy)]
pub struct Node<'a> {
    pub shell: &'a dyn Shell,
    pub ledger: &'a Ledger,
    pub thresholds: Thresholds,
    pub host: &'a str,
    pub label: &'a str,
    /// Filesystem the disk rule watches (normally `$HOME`).
    pub home: &'a str,
}

/// What `reserve` decided, with the probe it decided on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReserveOutcome {
    pub reserved: bool,
    pub ticket: String,
    pub probe: Probe,
    pub headroom: Headroom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseOutcome {
    pub released: usize,
}

impl Node<'_> {
    pub fn probe(&self) -> Result<Probe> {
        probe::local(
            self.shell,
            self.ledger,
            self.thresholds.reservation_ttl_ms,
            self.host,
            self.label,
            self.home,
        )
    }

    /// Re-check admission under this host's lock and write the reservation
    /// when the host still has headroom. A reservation with the same ticket
    /// is refreshed, not duplicated.
    pub fn reserve(&self, lead: &str, ticket: &str) -> Result<ReserveOutcome> {
        let ttl = self.thresholds.reservation_ttl_ms;
        self.ledger.with_lock(|ledger| {
            let mut probe = self.probe()?;
            // The ticket's own earlier reservation must not count against it.
            probe.reservations.retain(|r| r.ticket != ticket);
            let headroom = evaluate(&probe, &self.thresholds);
            if headroom.eligible {
                ledger.reserve(
                    Reservation {
                        ticket: ticket.to_string(),
                        lead: lead.to_string(),
                        created_ms: now_ms(),
                        pane: None,
                    },
                    ttl,
                )?;
            }
            Ok(ReserveOutcome {
                reserved: headroom.eligible,
                ticket: ticket.to_string(),
                probe,
                headroom,
            })
        })
    }

    pub fn release_ticket(&self, ticket: &str) -> Result<ReleaseOutcome> {
        let ttl = self.thresholds.reservation_ttl_ms;
        self.ledger.with_lock(|ledger| {
            Ok(ReleaseOutcome {
                released: ledger.release(ttl, |r| r.ticket == ticket)?,
            })
        })
    }

    pub fn release_pane(&self, pane: &str) -> Result<ReleaseOutcome> {
        let ttl = self.thresholds.reservation_ttl_ms;
        self.ledger.with_lock(|ledger| {
            Ok(ReleaseOutcome {
                released: ledger.release(ttl, |r| r.pane.as_deref() == Some(pane))?,
            })
        })
    }

    /// Attach the spawned pane to its reservation so `pane.closed` can free it.
    pub fn set_pane(&self, ticket: &str, pane: &str) -> Result<bool> {
        let ttl = self.thresholds.reservation_ttl_ms;
        self.ledger
            .with_lock(|ledger| ledger.set_pane(ticket, pane, ttl))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Fake;

    fn fake_shell(load: &str, agents: &str) -> Fake {
        let fake = Fake::new();
        fake.with_load(load)
            .on("df -Pk /tmp/home", "FS 1000 500 400 60% /\n")
            .on("herdr agent list", agents);
        fake
    }

    fn ledger(name: &str) -> Ledger {
        let dir = std::env::temp_dir().join(format!("wrangle-node-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Ledger::new(dir)
    }

    fn thresholds() -> Thresholds {
        Thresholds {
            load_per_core_max: 1.5,
            disk_free_min_percent: 15.0,
            reservation_load: 1.0,
            reservation_ttl_ms: 60_000,
        }
    }

    #[test]
    fn reserve_admits_until_headroom_is_gone() {
        let cores = f64::from(probe::cores());
        // Load leaves exactly two units of headroom.
        let load = format!("{{ {:.2} 0 0 }}", 1.5 * cores - 2.0);
        let shell = fake_shell(&load, r#"{"result":{"agents":[]}}"#);
        let ledger = ledger("reserve");
        let node = Node {
            shell: &shell,
            ledger: &ledger,
            thresholds: thresholds(),
            host: "local",
            label: "local",
            home: "/tmp/home",
        };
        assert!(node.reserve("lead", "t1").unwrap().reserved);
        assert!(node.reserve("lead", "t2").unwrap().reserved);
        let third = node.reserve("lead", "t3").unwrap();
        assert!(!third.reserved);
        assert_eq!(third.probe.reservations.len(), 2);
        // Refreshing an existing ticket does not count itself.
        assert!(node.reserve("lead", "t2").unwrap().reserved);
        assert_eq!(node.release_ticket("t1").unwrap().released, 1);
        assert!(node.set_pane("t2", "w1:p9").unwrap());
        assert_eq!(node.release_pane("w1:p9").unwrap().released, 1);
        assert_eq!(node.release_pane("w1:p9").unwrap().released, 0);
    }
}
