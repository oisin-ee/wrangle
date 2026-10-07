//! The fleet: this machine plus every enabled Herdr machine profile. Each
//! primitive runs in-process for the local host and over `ssh -T` for the
//! rest (the same transport Shepherdr uses for `herdr`).

use std::thread;

use serde::{Deserialize, Serialize};

use crate::config::{Policy, Thresholds};
use crate::error::{Error, Result};
use crate::ledger::Ledger;
use crate::node::{Node, ReleaseOutcome, ReserveOutcome};
use crate::plan::{Headroom, evaluate};
use crate::probe::Probe;
use crate::shell::{Shell, quote};

pub const LOCAL: &str = "local";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// `local` or the Herdr machine id.
    pub id: String,
    pub label: String,
    /// ssh target; `None` for this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl Host {
    #[must_use]
    pub fn local() -> Self {
        Self {
            id: LOCAL.to_string(),
            label: LOCAL.to_string(),
            target: None,
        }
    }

    #[must_use]
    pub fn is_local(&self) -> bool {
        self.target.is_none()
    }

    /// `--machine` value for `herdr` and Shepherdr: `None` for local.
    #[must_use]
    pub fn machine(&self) -> Option<&str> {
        if self.is_local() {
            None
        } else {
            Some(&self.id)
        }
    }
}

/// One host's probe, or why it could not be probed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostReport {
    #[serde(flatten)]
    pub host: Host,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<Probe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headroom: Option<Headroom>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct MachineRow {
    id: String,
    label: String,
    target: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Parse `herdr machine list --json`, keeping enabled machines.
pub fn parse_machine_list(text: &str) -> Result<Vec<Host>> {
    let rows: Vec<MachineRow> = serde_json::from_str(text).map_err(|e| Error::Parse {
        origin: "herdr machine list --json".to_string(),
        detail: e.to_string(),
    })?;
    Ok(rows
        .into_iter()
        .filter(|m| m.enabled)
        .map(|m| Host {
            id: m.id,
            label: m.label,
            target: Some(m.target),
        })
        .collect())
}

#[derive(Debug)]
pub struct Fleet<'a> {
    pub shell: &'a dyn Shell,
    pub ledger: &'a Ledger,
    pub policy: &'a Policy,
    pub home: String,
    pub hosts: Vec<Host>,
}

impl<'a> Fleet<'a> {
    /// Local plus every enabled machine profile.
    pub fn discover(
        shell: &'a dyn Shell,
        ledger: &'a Ledger,
        policy: &'a Policy,
        home: String,
    ) -> Result<Self> {
        let out = shell.run_ok(
            "herdr",
            &[
                "machine".to_string(),
                "list".to_string(),
                "--json".to_string(),
            ],
        )?;
        let mut hosts = vec![Host::local()];
        hosts.extend(parse_machine_list(&out.stdout)?);
        Ok(Self {
            shell,
            ledger,
            policy,
            home,
            hosts,
        })
    }

    #[must_use]
    pub fn thresholds(&self) -> Thresholds {
        Thresholds::from(self.policy)
    }

    /// Resolve `--machine` by id or label; `local`, or omitted, is this host.
    pub fn find(&self, machine: &str) -> Result<&Host> {
        self.hosts
            .iter()
            .find(|h| h.id == machine || h.label == machine)
            .ok_or_else(|| Error::UnknownMachine(machine.to_string()))
    }

    /// Hosts a call may use: the pinned one, or all of them.
    pub fn eligible(&self, machine: Option<&str>) -> Result<Vec<&Host>> {
        match machine {
            Some(m) => Ok(vec![self.find(m)?]),
            None => Ok(self.hosts.iter().collect()),
        }
    }

    fn node<'b>(&'b self, host: &'b Host) -> Node<'b> {
        Node {
            shell: self.shell,
            ledger: self.ledger,
            thresholds: self.thresholds(),
            host: &host.id,
            label: &host.label,
            home: &self.home,
        }
    }

    /// `ssh -T -o BatchMode=yes -- <target> <remote_command> host <args…>`,
    /// each remote word single-quoted for the far shell. The host's id and
    /// label travel along because the far side does not know them.
    pub(crate) fn remote_argv(&self, host: &Host, args: &[String]) -> Vec<String> {
        let t = self.thresholds();
        let target = host.target.clone().unwrap_or_default();
        let words: Vec<String> = self
            .policy
            .remote_command
            .iter()
            .cloned()
            .chain(["host".to_string()])
            .chain(args.iter().cloned())
            .chain([
                "--host-id".to_string(),
                host.id.clone(),
                "--label".to_string(),
                host.label.clone(),
                "--load-per-core-max".to_string(),
                t.load_per_core_max.to_string(),
                "--disk-free-min-percent".to_string(),
                t.disk_free_min_percent.to_string(),
                "--reservation-load".to_string(),
                t.reservation_load.to_string(),
                "--reservation-ttl-ms".to_string(),
                t.reservation_ttl_ms.to_string(),
                "--json".to_string(),
            ])
            .collect();
        let command = words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
        vec![
            "-T".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "--".to_string(),
            target,
            command,
        ]
    }

    /// Run a `host` primitive on `host` and parse its JSON. Exit 1 carries a
    /// JSON body too (`reserved: false`); anything else is an ssh failure.
    fn remote<T: for<'de> Deserialize<'de>>(&self, host: &Host, args: &[String]) -> Result<T> {
        let Some(target) = host.target.as_deref() else {
            return Err(Error::Invalid(format!("{} is the local host", host.id)));
        };
        let out = self.shell.run("ssh", &self.remote_argv(host, args))?;
        // Exit 1 with a JSON body is `reserved: false`; exit 1 with no body is
        // the remote launcher failing (for example mise without the tool).
        if (out.status != 0 && out.status != 1) || out.stdout.trim().is_empty() {
            return Err(Error::Ssh {
                target: target.to_string(),
                detail: format!(
                    "exit {}: {}",
                    out.status,
                    out.stderr.trim().lines().next().unwrap_or("no output")
                ),
            });
        }
        serde_json::from_str(&out.stdout).map_err(|e| Error::Parse {
            origin: format!("ssh {target} wrangle host {}", args.join(" ")),
            detail: format!("{e}: {}", out.stdout.trim()),
        })
    }

    pub fn probe(&self, host: &Host) -> Result<Probe> {
        if host.is_local() {
            self.node(host).probe()
        } else {
            self.remote(host, &["probe".to_string()])
        }
    }

    /// Probe every given host in parallel; failures become reports.
    pub fn probe_all(&self, hosts: &[&Host]) -> Vec<HostReport> {
        let t = self.thresholds();
        thread::scope(|s| {
            let handles: Vec<_> = hosts
                .iter()
                .map(|host| s.spawn(move || (host, self.probe(host))))
                .collect();
            handles
                .into_iter()
                .map(|h| match h.join() {
                    Ok((host, Ok(probe))) => HostReport {
                        host: (*host).clone(),
                        ok: true,
                        headroom: Some(evaluate(&probe, &t)),
                        probe: Some(probe),
                        error: None,
                    },
                    Ok((host, Err(e))) => HostReport {
                        host: (*host).clone(),
                        ok: false,
                        probe: None,
                        headroom: None,
                        error: Some(e.to_string()),
                    },
                    Err(_) => HostReport {
                        host: Host::local(),
                        ok: false,
                        probe: None,
                        headroom: None,
                        error: Some("probe thread panicked".to_string()),
                    },
                })
                .collect()
        })
    }

    pub fn reserve(&self, host: &Host, lead: &str, ticket: &str) -> Result<ReserveOutcome> {
        if host.is_local() {
            self.node(host).reserve(lead, ticket)
        } else {
            self.remote(
                host,
                &[
                    "reserve".to_string(),
                    "--lead".to_string(),
                    lead.to_string(),
                    "--ticket".to_string(),
                    ticket.to_string(),
                ],
            )
        }
    }

    pub fn release_ticket(&self, host: &Host, ticket: &str) -> Result<ReleaseOutcome> {
        if host.is_local() {
            self.node(host).release_ticket(ticket)
        } else {
            self.remote(
                host,
                &[
                    "release".to_string(),
                    "--ticket".to_string(),
                    ticket.to_string(),
                ],
            )
        }
    }

    pub fn release_pane(&self, host: &Host, pane: &str) -> Result<ReleaseOutcome> {
        if host.is_local() {
            self.node(host).release_pane(pane)
        } else {
            self.remote(
                host,
                &[
                    "release".to_string(),
                    "--pane".to_string(),
                    pane.to_string(),
                ],
            )
        }
    }

    pub fn set_pane(&self, host: &Host, ticket: &str, pane: &str) -> Result<bool> {
        if host.is_local() {
            self.node(host).set_pane(ticket, pane)
        } else {
            let out: SetPaneOutcome = self.remote(
                host,
                &[
                    "set-pane".to_string(),
                    "--ticket".to_string(),
                    ticket.to_string(),
                    "--pane".to_string(),
                    pane.to_string(),
                ],
            )?;
            Ok(out.found)
        }
    }

    /// Release a ticket everywhere; sums what each reachable host dropped.
    pub fn release_ticket_everywhere(&self, ticket: &str) -> usize {
        thread::scope(|s| {
            let handles: Vec<_> = self
                .hosts
                .iter()
                .map(|host| s.spawn(move || self.release_ticket(host, ticket)))
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .filter_map(std::result::Result::ok)
                .map(|o| o.released)
                .sum()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetPaneOutcome {
    pub found: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Fake;

    const MACHINES: &str = r#"[
      {"id":"8103","label":"netcup","target":"netcup-dev","session":"default","enabled":true,"selected":false},
      {"id":"08c2","label":"momokaya-2","target":"momokaya-2-dev","session":"default","enabled":false,"selected":false}
    ]"#;

    #[test]
    fn machine_list_keeps_enabled_hosts() {
        let hosts = parse_machine_list(MACHINES).unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].label, "netcup");
        assert_eq!(hosts[0].machine(), Some("8103"));
        assert_eq!(Host::local().machine(), None);
    }

    fn fleet<'a>(shell: &'a Fake, ledger: &'a Ledger, policy: &'a Policy) -> Fleet<'a> {
        Fleet::discover(shell, ledger, policy, "/tmp/home".to_string()).unwrap()
    }

    #[test]
    fn remote_argv_quotes_and_carries_thresholds() {
        let shell = Fake::new();
        shell.on("herdr machine list --json", MACHINES);
        let ledger = Ledger::new(std::env::temp_dir().join("wrangle-hosts-argv"));
        let policy = Policy::default();
        let f = fleet(&shell, &ledger, &policy);
        let netcup = f.find("netcup").unwrap();
        let argv = f.remote_argv(
            netcup,
            &["reserve".into(), "--lead".into(), "my lead".into()],
        );
        assert_eq!(
            &argv[..5],
            &["-T", "-o", "BatchMode=yes", "--", "netcup-dev"]
        );
        assert_eq!(
            argv[5],
            "mise x -- wrangle host reserve --lead 'my lead' --host-id 8103 --label netcup \
             --load-per-core-max 1.5 --disk-free-min-percent 15 --reservation-load 1 \
             --reservation-ttl-ms 180000 --json"
        );
    }

    #[test]
    fn find_by_id_or_label_and_unknown() {
        let shell = Fake::new();
        shell.on("herdr machine list --json", MACHINES);
        let ledger = Ledger::new(std::env::temp_dir().join("wrangle-hosts-find"));
        let policy = Policy::default();
        let f = fleet(&shell, &ledger, &policy);
        assert_eq!(f.find("netcup").unwrap().id, "8103");
        assert_eq!(f.find("8103").unwrap().label, "netcup");
        assert!(f.find("local").unwrap().is_local());
        assert!(matches!(f.find("mars"), Err(Error::UnknownMachine(_))));
        assert_eq!(f.eligible(None).unwrap().len(), 2);
    }

    #[test]
    fn remote_probe_parses_json_and_reports_ssh_failure() {
        let shell = Fake::new();
        shell.on("herdr machine list --json", MACHINES);
        let ledger = Ledger::new(std::env::temp_dir().join("wrangle-hosts-remote"));
        let policy = Policy::default();
        let f = fleet(&shell, &ledger, &policy);
        let netcup = f.find("netcup").unwrap().clone();
        let argv = f.remote_argv(&netcup, &["probe".into()]);
        let key = format!("ssh {}", argv.join(" "));
        let body = r#"{"host":"8103","label":"netcup","load1":1.0,"cores":16,
            "disk_free_percent":18.0,"agents":{"total":0,"by_status":{}},"reservations":[]}"#;
        shell.on(&key, body);
        let reports = f.probe_all(&[&netcup]);
        assert!(reports[0].ok, "{:?}", reports[0].error);
        assert!(reports[0].headroom.as_ref().unwrap().eligible);

        shell.on_output(
            &key,
            crate::shell::Output {
                status: 255,
                stdout: String::new(),
                stderr: "Permission denied (publickey)".into(),
            },
        );
        let reports = f.probe_all(&[&netcup]);
        assert!(!reports[0].ok);
        assert!(
            reports[0]
                .error
                .as_deref()
                .unwrap()
                .contains("Permission denied")
        );
    }
}
