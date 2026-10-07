//! Measure one host: load, cores, home disk, Herdr agents, and the live
//! reservations in its ledger. Runs on the host it describes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ledger::{Ledger, Reservation};
use crate::shell::Shell;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCounts {
    pub total: usize,
    /// `agent_status` → count, as Herdr reports it (`idle`, `working`, `done`, …).
    #[serde(default)]
    pub by_status: BTreeMap<String, usize>,
}

impl AgentCounts {
    /// Agents that still hold a pane and may run: everything not `done`.
    #[must_use]
    pub fn live(&self) -> usize {
        self.total
            .saturating_sub(self.by_status.get("done").copied().unwrap_or(0))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// `local` or the Herdr machine id.
    pub host: String,
    pub label: String,
    pub load1: f64,
    pub cores: u32,
    pub disk_free_percent: f64,
    pub agents: AgentCounts,
    pub reservations: Vec<Reservation>,
}

/// Measure this machine. `home` is the filesystem the disk rule watches.
pub fn local(
    shell: &dyn Shell,
    ledger: &Ledger,
    ttl_ms: u64,
    host: &str,
    label: &str,
    home: &str,
) -> Result<Probe> {
    Ok(Probe {
        host: host.to_string(),
        label: label.to_string(),
        load1: load1(shell)?,
        cores: cores(),
        disk_free_percent: disk_free_percent(shell, home)?,
        agents: agents(shell)?,
        reservations: ledger.reservations(ttl_ms)?,
    })
}

#[must_use]
pub fn cores() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

#[cfg(target_os = "linux")]
pub fn load1(_shell: &dyn Shell) -> Result<f64> {
    let text = std::fs::read_to_string("/proc/loadavg")?;
    parse_loadavg(&text).ok_or_else(|| Error::Parse {
        origin: "/proc/loadavg".to_string(),
        detail: text.trim().to_string(),
    })
}

#[cfg(not(target_os = "linux"))]
pub fn load1(shell: &dyn Shell) -> Result<f64> {
    let out = shell.run_ok("sysctl", &["-n".to_string(), "vm.loadavg".to_string()])?;
    parse_loadavg(&out.stdout).ok_or_else(|| Error::Parse {
        origin: "sysctl vm.loadavg".to_string(),
        detail: out.stdout.trim().to_string(),
    })
}

/// First number in `0.52 0.58 0.59 1/389 12345` or `{ 0.52 0.58 0.59 }`.
#[must_use]
pub fn parse_loadavg(text: &str) -> Option<f64> {
    text.split(|c: char| c.is_whitespace() || c == '{' || c == '}')
        .find(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}

pub fn disk_free_percent(shell: &dyn Shell, path: &str) -> Result<f64> {
    let out = shell.run_ok("df", &["-Pk".to_string(), path.to_string()])?;
    parse_df(&out.stdout).ok_or_else(|| Error::Parse {
        origin: format!("df -Pk {path}"),
        detail: out.stdout.trim().to_string(),
    })
}

/// Free percent from POSIX `df -Pk`: `Available / 1024-blocks × 100` on the
/// last line.
#[must_use]
pub fn parse_df(text: &str) -> Option<f64> {
    let line = text.lines().rfind(|l| !l.trim().is_empty())?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    // Filesystem 1024-blocks Used Available Capacity Mounted-on
    if cols.len() < 5 {
        return None;
    }
    let total: f64 = cols[1].parse().ok()?;
    let available: f64 = cols[3].parse().ok()?;
    if total <= 0.0 {
        return None;
    }
    Some(available / total * 100.0)
}

pub fn agents(shell: &dyn Shell) -> Result<AgentCounts> {
    let out = shell.run_ok("herdr", &["agent".to_string(), "list".to_string()])?;
    parse_agent_list(&out.stdout)
}

#[derive(Deserialize)]
struct AgentList {
    result: AgentListResult,
}

#[derive(Deserialize)]
struct AgentListResult {
    #[serde(default)]
    agents: Vec<AgentRow>,
}

#[derive(Deserialize)]
struct AgentRow {
    #[serde(default)]
    agent_status: Option<String>,
}

/// Count agents in `herdr agent list` JSON (`{"result":{"agents":[…]}}`).
pub fn parse_agent_list(text: &str) -> Result<AgentCounts> {
    let list: AgentList = serde_json::from_str(text).map_err(|e| Error::Parse {
        origin: "herdr agent list".to_string(),
        detail: e.to_string(),
    })?;
    let mut counts = AgentCounts::default();
    for row in list.result.agents {
        counts.total += 1;
        let status = row.agent_status.unwrap_or_else(|| "unknown".to_string());
        *counts.by_status.entry(status).or_insert(0) += 1;
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loadavg_linux_and_sysctl_forms() {
        assert_eq!(
            parse_loadavg("34.20 28.66 21.17 16/2698 1480037\n"),
            Some(34.2)
        );
        assert_eq!(parse_loadavg("{ 33.81 24.81 15.46 }\n"), Some(33.81));
        assert_eq!(parse_loadavg(""), None);
    }

    #[test]
    fn df_last_line_free_percent() {
        let text = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                    /dev/disk3s5 1000 700 250 76% /System/Volumes/Data\n";
        let free = parse_df(text).unwrap();
        assert!((free - 25.0).abs() < 1e-9);
        assert_eq!(parse_df("Filesystem\n"), None);
    }

    #[test]
    fn agent_list_counts_by_status() {
        let text = r#"{"id":"cli:agent:list","result":{"agents":[
            {"agent":"pi","agent_status":"idle","pane_id":"w1:p1"},
            {"agent":"pi","agent_status":"done","pane_id":"w1:p2"},
            {"agent":"pi","agent_status":"working","pane_id":"w1:p3"},
            {"agent":"claude","pane_id":"w1:p4"}
        ],"type":"agent_list"}}"#;
        let c = parse_agent_list(text).unwrap();
        assert_eq!(c.total, 4);
        assert_eq!(c.by_status["idle"], 1);
        assert_eq!(c.by_status["unknown"], 1);
        assert_eq!(c.live(), 3);
    }

    #[test]
    fn agent_list_garbage_is_a_parse_error() {
        assert!(matches!(parse_agent_list("nope"), Err(Error::Parse { .. })));
    }
}
