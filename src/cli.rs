//! Command line surface. Lead-side commands orchestrate the fleet; `host`
//! primitives run on the machine they describe (locally or over ssh).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::config::Thresholds;

#[derive(Debug, Parser)]
#[command(
    name = "wrangle",
    version,
    about = "Admit, queue, and spawn coding agents across Herdr hosts",
    propagate_version = true
)]
pub struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Policy file (default: `$WRANGLE_CONFIG` or `~/.config/wrangle/config.toml`).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Measure every host (or one): load, cores, disk, agents, reservations, headroom.
    Probe {
        /// Machine id or label; `local` for this host.
        #[arg(long)]
        machine: Option<String>,
    },
    /// Reserve a slot on the host with the most headroom, or queue a ticket.
    Admit {
        /// Name of the lead that will own the child.
        #[arg(long, required_unless_present = "ticket")]
        lead: Option<String>,
        /// Pin the spawn to one machine instead of balancing.
        #[arg(long)]
        machine: Option<String>,
        /// Retry a queued ticket; prints the same result once it is reserved.
        #[arg(long)]
        ticket: Option<String>,
    },
    /// Drop a reservation by ticket (everywhere) or by pane (one host).
    Release {
        #[arg(long, conflicts_with = "pane", required_unless_present = "pane")]
        ticket: Option<String>,
        #[arg(long)]
        pane: Option<String>,
        /// Host of the pane; default `local`.
        #[arg(long, requires = "pane")]
        machine: Option<String>,
    },
    /// Hosts plus every queued and reserved ticket.
    Status {
        /// Refresh until interrupted.
        #[arg(long)]
        watch: bool,
        /// Seconds between refreshes with --watch.
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
    /// Drop a queued ticket and any reservation it holds.
    Cancel {
        #[arg(long)]
        ticket: String,
    },
    /// Mark a child pane in the Herdr sidebar (`↳ name`, red dot, owner).
    Mark {
        #[arg(long)]
        pane: String,
        #[arg(long, required_unless_present = "clear")]
        lead: Option<String>,
        #[arg(long, required_unless_present = "clear")]
        name: Option<String>,
        /// Host of the pane; default `local`.
        #[arg(long)]
        machine: Option<String>,
        /// Attach the pane to this ticket's reservation and drop the queue entry.
        #[arg(long)]
        ticket: Option<String>,
        /// Remove the mark instead.
        #[arg(long)]
        clear: bool,
    },
    /// Report the lead's queue length on its own pane (`N queued`).
    Queue {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        count: usize,
    },
    /// Show a toast in this host's Herdr window.
    Notify {
        title: String,
        #[arg(long, default_value = "")]
        body: String,
    },
    /// Per-host primitives. The lead runs these over ssh; do not call by hand.
    #[command(subcommand)]
    Host(HostCommand),
}

#[derive(Debug, Subcommand)]
pub enum HostCommand {
    /// Measure this machine.
    Probe {
        #[command(flatten)]
        identity: HostIdentity,
        #[command(flatten)]
        thresholds: ThresholdArgs,
    },
    /// Re-check admission under the host lock and write a reservation.
    Reserve {
        #[arg(long)]
        lead: String,
        #[arg(long)]
        ticket: String,
        #[command(flatten)]
        identity: HostIdentity,
        #[command(flatten)]
        thresholds: ThresholdArgs,
    },
    /// Drop a reservation on this machine.
    Release {
        #[arg(long, conflicts_with = "pane", required_unless_present = "pane")]
        ticket: Option<String>,
        #[arg(long)]
        pane: Option<String>,
        #[command(flatten)]
        identity: HostIdentity,
        #[command(flatten)]
        thresholds: ThresholdArgs,
    },
    /// Attach a pane to a reservation.
    SetPane {
        #[arg(long)]
        ticket: String,
        #[arg(long)]
        pane: String,
        #[command(flatten)]
        identity: HostIdentity,
        #[command(flatten)]
        thresholds: ThresholdArgs,
    },
}

#[derive(Debug, Clone, Args)]
pub struct HostIdentity {
    /// Machine id as the lead knows it.
    #[arg(long, default_value = "local")]
    pub host_id: String,
    #[arg(long, default_value = "local")]
    pub label: String,
}

/// The lead's thresholds, so every host applies the same rule. Unset flags
/// fall back to this host's own config.
#[derive(Debug, Clone, Copy, Args)]
pub struct ThresholdArgs {
    #[arg(long)]
    pub load_per_core_max: Option<f64>,
    #[arg(long)]
    pub disk_free_min_percent: Option<f64>,
    #[arg(long)]
    pub reservation_load: Option<f64>,
    #[arg(long)]
    pub reservation_ttl_ms: Option<u64>,
}

impl ThresholdArgs {
    #[must_use]
    pub fn over(self, base: Thresholds) -> Thresholds {
        Thresholds {
            load_per_core_max: self.load_per_core_max.unwrap_or(base.load_per_core_max),
            disk_free_min_percent: self
                .disk_free_min_percent
                .unwrap_or(base.disk_free_min_percent),
            reservation_load: self.reservation_load.unwrap_or(base.reservation_load),
            reservation_ttl_ms: self.reservation_ttl_ms.unwrap_or(base.reservation_ttl_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_tree_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn admit_requires_lead_unless_ticket() {
        assert!(Cli::try_parse_from(["wrangle", "admit"]).is_err());
        assert!(Cli::try_parse_from(["wrangle", "admit", "--lead", "x"]).is_ok());
        assert!(Cli::try_parse_from(["wrangle", "admit", "--ticket", "w-1"]).is_ok());
    }

    #[test]
    fn host_thresholds_override_base() {
        let cli = Cli::try_parse_from([
            "wrangle",
            "host",
            "probe",
            "--reservation-ttl-ms",
            "5",
            "--json",
        ])
        .unwrap();
        assert!(cli.json);
        let Command::Host(HostCommand::Probe {
            thresholds,
            identity,
        }) = cli.command
        else {
            unreachable!();
        };
        let t = thresholds.over(Thresholds::from(&crate::config::Policy::default()));
        assert_eq!(t.reservation_ttl_ms, 5);
        assert!((t.load_per_core_max - 1.5).abs() < f64::EPSILON);
        assert_eq!(identity.host_id, "local");
    }
}
