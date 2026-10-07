//! `wrangle`: admit, queue, and spawn coding agents across Herdr hosts.
//!
//! Exit codes: 0 ok, 1 every eligible host is full (the JSON body carries
//! the queued ticket), 2 any other error.

mod cli;
mod commands;
mod config;
mod error;
mod herdr;
mod hook;
mod hosts;
mod ledger;
mod node;
mod plan;
mod probe;
mod shell;
mod spawn;

use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;
use serde::Serialize;

use cli::{Cli, Command, HostCommand};
use commands::AdmitOutput;
use error::{Error, Result};
use hosts::Fleet;
use ledger::Ledger;
use node::Node;
use shell::System;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            if json {
                let body = serde_json::json!({ "error": e.to_string() });
                println!("{body}");
            } else {
                eprintln!("wrangle: {e}");
            }
            e.exit_code()
        }
    }
}

fn print<T: Serialize>(json: bool, value: &T, text: impl FnOnce() -> String) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if json {
        serde_json::to_writer(&mut out, value)?;
        writeln!(out)?;
    } else {
        write!(out, "{}", text())?;
    }
    Ok(())
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

fn run(cli: Cli) -> Result<ExitCode> {
    let json = cli.json;
    let policy = config::load(&cli.config.unwrap_or_else(config::default_path))?;
    let shell = System;
    let ledger = Ledger::new(ledger::default_dir());
    match cli.command {
        Command::Host(cmd) => host(cmd, shell, &ledger, &policy, json),
        cmd => {
            let fleet = Fleet::discover(&shell, &ledger, &policy, home())?;
            lead(cmd, &fleet, json)
        }
    }
}

/// The lead-side commands.
fn lead(cmd: Command, fleet: &Fleet, json: bool) -> Result<ExitCode> {
    match cmd {
        Command::Probe { machine } => {
            let reports = commands::probe(fleet, machine.as_deref())?;
            print(json, &reports, || {
                commands::render_status(&commands::Status {
                    hosts: reports.clone(),
                    queue: Vec::new(),
                })
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Admit {
            lead,
            machine,
            ticket,
        } => {
            let out = commands::admit(
                fleet,
                lead.as_deref().unwrap_or_default(),
                machine.as_deref(),
                ticket.as_deref(),
            )?;
            let code = match &out {
                AdmitOutput::Admitted(_) => ExitCode::SUCCESS,
                AdmitOutput::Queued(_) => Error::Full.exit_code(),
            };
            print(json, &out, || match &out {
                AdmitOutput::Admitted(a) => format!(
                    "admitted {} on {} (headroom {:.2})\n",
                    a.ticket, a.host.label, a.headroom.headroom
                ),
                AdmitOutput::Queued(q) => format!("queued {}: {}\n", q.ticket, q.reason),
            })?;
            Ok(code)
        }
        Command::Release {
            ticket,
            pane,
            machine,
        } => {
            let out = match (ticket, pane) {
                (Some(t), _) => commands::release_ticket(fleet, &t)?,
                (None, Some(p)) => commands::release_pane(fleet, &p, machine.as_deref())?,
                (None, None) => return Err(Error::Invalid("--ticket or --pane".into())),
            };
            print(json, &out, || format!("released {}\n", out.released))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Status { watch, interval } => {
            loop {
                let st = commands::status(fleet)?;
                if watch && !json {
                    // Clear the screen between frames of the board.
                    print!("\x1b[2J\x1b[H");
                }
                print(json, &st, || commands::render_status(&st))?;
                if !watch {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(interval.max(1)));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Cancel { ticket } => {
            let out = commands::cancel(fleet, &ticket)?;
            print(json, &out, || format!("cancelled {}\n", out.ticket))?;
            Ok(ExitCode::SUCCESS)
        }
        cmd => report(cmd, fleet, json),
    }
}

/// The sidebar and notification commands.
fn spawn_cmd(fleet: &Fleet, req: &spawn::SpawnRequest<'_>, json: bool) -> Result<ExitCode> {
    let out = spawn::spawn(fleet, req)?;
    print(json, &out, || match &out {
        spawn::SpawnOutput::Spawned(s) => {
            format!("spawned {} on {} in {}\n", s.name, s.host.label, s.pane_id)
        }
        spawn::SpawnOutput::Queued(q) => format!("queued {}: {}\n", q.ticket, q.reason),
    })?;
    Ok(match out {
        spawn::SpawnOutput::Spawned(_) => ExitCode::SUCCESS,
        spawn::SpawnOutput::Queued(_) => ExitCode::from(error::EXIT_FULL),
    })
}

fn report(cmd: Command, fleet: &Fleet, json: bool) -> Result<ExitCode> {
    match cmd {
        Command::Mark {
            pane,
            lead,
            name,
            machine,
            ticket,
            clear,
        } => {
            let out = commands::mark(
                fleet,
                machine.as_deref(),
                &pane,
                lead.as_deref().unwrap_or_default(),
                name.as_deref().unwrap_or_default(),
                ticket.as_deref(),
                clear,
            )?;
            print(json, &out, || {
                format!(
                    "{} {}\n",
                    if out.marked { "marked" } else { "cleared" },
                    out.pane
                )
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Queue { pane, count } => {
            let out = commands::queue(fleet, &pane, count)?;
            print(json, &out, || {
                format!("reported {} queued on {}\n", out.count, out.pane)
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Prepare {
            machine,
            branch,
            base,
            repo,
        } => {
            let host = match machine.as_deref() {
                Some(m) => fleet.find(m)?,
                None => fleet.find(hosts::LOCAL)?,
            };
            let cwd = std::env::current_dir()?.display().to_string();
            let inputs = hook::HookInputs {
                repo: repo.as_deref().unwrap_or(&cwd),
                branch: &branch,
                base: &base,
            };
            let out = hook::run(fleet, host, inputs)?;
            print(json, &out, || {
                format!("prepared {} on {}\n", out.pane_id, out.host.label)
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Spawn {
            ticket,
            lead,
            machine,
            kind,
            name,
            label,
            message,
            branch,
            base,
            repo,
            cwd,
            workspace,
        } => {
            let req = spawn::SpawnRequest {
                ticket: ticket.as_deref(),
                lead: lead.as_deref(),
                machine: machine.as_deref(),
                kind: &kind,
                name: &name,
                label: label.as_deref(),
                message: &message,
                branch: branch.as_deref(),
                base: base.as_deref(),
                repo: repo.as_deref(),
                cwd: cwd.as_deref(),
                workspace: workspace.as_deref(),
            };
            spawn_cmd(fleet, &req, json)
        }
        Command::Notify { title, body } => {
            commands::notify(fleet, &title, &body)?;
            print(json, &serde_json::json!({ "notified": true }), || {
                format!("notified: {title}\n")
            })?;
            Ok(ExitCode::SUCCESS)
        }
        _ => Ok(ExitCode::SUCCESS),
    }
}

fn node<'a>(
    shell: &'a System,
    ledger: &'a Ledger,
    home: &'a str,
    base: config::Thresholds,
    identity: &'a cli::HostIdentity,
    thresholds: cli::ThresholdArgs,
) -> Node<'a> {
    Node {
        shell,
        ledger,
        thresholds: thresholds.over(base),
        host: &identity.host_id,
        label: &identity.label,
        home,
    }
}

/// The per-host primitives, run on this machine.
fn host(
    cmd: HostCommand,
    shell: System,
    ledger: &Ledger,
    policy: &config::Policy,
    json: bool,
) -> Result<ExitCode> {
    let base = config::Thresholds::from(policy);
    let home = home();
    let shell = &shell;
    match cmd {
        HostCommand::Probe {
            identity,
            thresholds,
        } => {
            let p = node(shell, ledger, &home, base, &identity, thresholds).probe()?;
            print(json, &p, || format!("{p:#?}\n"))?;
            Ok(ExitCode::SUCCESS)
        }
        HostCommand::Reserve {
            lead,
            ticket,
            identity,
            thresholds,
        } => {
            let out =
                node(shell, ledger, &home, base, &identity, thresholds).reserve(&lead, &ticket)?;
            print(json, &out, || {
                format!(
                    "{} {} (headroom {:.2})\n",
                    if out.reserved { "reserved" } else { "full" },
                    out.ticket,
                    out.headroom.headroom
                )
            })?;
            Ok(if out.reserved {
                ExitCode::SUCCESS
            } else {
                Error::Full.exit_code()
            })
        }
        HostCommand::Release {
            ticket,
            pane,
            identity,
            thresholds,
        } => {
            let n = node(shell, ledger, &home, base, &identity, thresholds);
            let out = match (ticket, pane) {
                (Some(t), _) => n.release_ticket(&t)?,
                (None, Some(p)) => n.release_pane(&p)?,
                (None, None) => return Err(Error::Invalid("--ticket or --pane".into())),
            };
            print(json, &out, || format!("released {}\n", out.released))?;
            Ok(ExitCode::SUCCESS)
        }
        HostCommand::SetPane {
            ticket,
            pane,
            identity,
            thresholds,
        } => {
            let found =
                node(shell, ledger, &home, base, &identity, thresholds).set_pane(&ticket, &pane)?;
            let out = hosts::SetPaneOutcome { found };
            print(json, &out, || format!("found {found}\n"))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}
