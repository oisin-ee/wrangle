//! Run the real binary against a fake `herdr` on PATH and a private state
//! directory. Only the local host exists (the fake machine list is empty).

// Test-only crate: the rig helpers may unwrap and panic like the #[test] fns.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Rig {
    dir: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("wrangle-cli-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::create_dir_all(dir.join("state")).unwrap();
        fs::create_dir_all(dir.join("home")).unwrap();
        Self { dir }
    }

    /// A `herdr` that answers `machine list --json` and `agent list`, and
    /// records every other call to `calls.log`.
    fn fake_herdr(&self, agents: &str) {
        let script = format!(
            "#!/bin/sh\n\
             echo \"$@\" >> \"{log}\"\n\
             case \"$*\" in\n\
               'machine list --json') echo '[]' ;;\n\
               'agent list') echo '{agents}' ;;\n\
               *) : ;;\n\
             esac\n",
            log = self.dir.join("calls.log").display(),
        );
        let path = self.dir.join("bin").join("herdr");
        fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn config(&self, text: &str) -> PathBuf {
        let p = self.dir.join("config.toml");
        fs::write(&p, text).unwrap();
        p
    }

    fn run(&self, args: &[&str], config: Option<&Path>) -> (i32, serde_json::Value, String) {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_wrangle"));
        cmd.args(args)
            .arg("--json")
            .env("PATH", path)
            .env("WRANGLE_STATE", self.dir.join("state"))
            .env("HOME", self.dir.join("home"));
        match config {
            Some(c) => cmd.env("WRANGLE_CONFIG", c),
            None => cmd.env("WRANGLE_CONFIG", self.dir.join("missing.toml")),
        };
        let out = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("bad JSON ({e}): {stdout}\nstderr: {stderr}"));
        (out.status.code().unwrap_or(-1), value, stderr)
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.dir.join("calls.log")).unwrap_or_default()
    }
}

#[test]
fn probe_and_status_report_the_local_host() {
    let rig = Rig::new("probe");
    rig.fake_herdr(r#"{"result":{"agents":[{"agent_status":"idle"}]}}"#);
    let (code, v, _) = rig.run(&["probe"], None);
    assert_eq!(code, 0);
    assert_eq!(v[0]["id"], "local");
    assert!(v[0]["ok"].as_bool().unwrap(), "{v}");
    assert_eq!(v[0]["probe"]["agents"]["total"], 1);
    assert!(v[0]["probe"]["cores"].as_u64().unwrap() >= 1);

    let (code, v, _) = rig.run(&["status"], None);
    assert_eq!(code, 0);
    assert_eq!(v["hosts"].as_array().unwrap().len(), 1);
    assert_eq!(v["queue"].as_array().unwrap().len(), 0);
}

#[test]
fn admit_queues_when_full_and_admits_when_loosened() {
    let rig = Rig::new("admit");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    // Nothing is admissible: no load headroom at all.
    let strict = rig.config("load_per_core_max = 0.0\n");
    let (code, v, _) = rig.run(&["admit", "--lead", "lead-a"], Some(&strict));
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["queued"], true);
    let ticket = v["ticket"].as_str().unwrap().to_string();
    assert!(v["reason"].as_str().unwrap().contains("local: load"));

    let (_, v, _) = rig.run(&["status"], Some(&strict));
    assert_eq!(v["queue"][0]["ticket"], ticket);
    assert_eq!(v["queue"][0]["state"], "queued");

    // Loosen the rule: the queued ticket is admitted on retry.
    let loose = rig.config("load_per_core_max = 1000.0\ndisk_free_min_percent = 0\n");
    let (code, v, _) = rig.run(&["admit", "--ticket", &ticket], Some(&loose));
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["admitted"], true);
    assert_eq!(v["ticket"], ticket);
    assert_eq!(v["host"]["id"], "local");

    // The reservation shows up in the next probe and in status.
    let (_, v, _) = rig.run(&["probe"], Some(&loose));
    assert_eq!(v[0]["probe"]["reservations"][0]["ticket"], ticket);
    let (_, v, _) = rig.run(&["status"], Some(&loose));
    assert_eq!(v["queue"][0]["state"], "reserved");

    // mark reports metadata through herdr, attaches the pane, and dequeues.
    let (code, v, _) = rig.run(
        &[
            "mark", "--pane", "w1:p3", "--lead", "lead-a", "--name", "reviewer", "--ticket",
            &ticket,
        ],
        Some(&loose),
    );
    assert_eq!(code, 0, "{v}");
    assert!(rig.calls().contains(
        "pane report-metadata w1:p3 --source wrangle --display-agent ↳ reviewer --token sub=● --token owner=lead-a"
    ));
    let (_, v, _) = rig.run(&["status"], Some(&loose));
    assert_eq!(v["queue"].as_array().unwrap().len(), 0);
    assert_eq!(v["hosts"][0]["probe"]["reservations"][0]["pane"], "w1:p3");

    // release by pane (what the Herdr plugin's pane.closed hook runs).
    let (code, v, _) = rig.run(&["release", "--pane", "w1:p3"], Some(&loose));
    assert_eq!(code, 0);
    assert_eq!(v["released"], 1);

    // cancel of a ticket that no longer exists is an error.
    let (code, v, _) = rig.run(&["cancel", "--ticket", &ticket], Some(&loose));
    assert_eq!(code, 2);
    assert!(v["error"].as_str().unwrap().contains("not queued"));
}

#[test]
fn queue_token_and_notify_go_through_herdr() {
    let rig = Rig::new("queue");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    let (code, _, _) = rig.run(&["queue", "--pane", "w1:p1", "--count", "2"], None);
    assert_eq!(code, 0);
    let (code, _, _) = rig.run(&["queue", "--pane", "w1:p1", "--count", "0"], None);
    assert_eq!(code, 0);
    let (code, _, _) = rig.run(&["notify", "queued", "--body", "ticket w-1"], None);
    assert_eq!(code, 0);
    let calls = rig.calls();
    assert!(calls.contains("--token queue=2 queued"));
    assert!(calls.contains("--clear-token queue"));
    assert!(calls.contains("notification show queued --body ticket w-1"));
}

#[test]
fn bad_config_is_an_error_not_a_default() {
    let rig = Rig::new("config");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    let bad = rig.config("max_agents = 3\n");
    let (code, v, _) = rig.run(&["probe"], Some(&bad));
    assert_eq!(code, 2);
    assert!(v["error"].as_str().unwrap().contains("config"));
}

#[test]
fn prepare_runs_the_hook_template_and_parses_pane_id() {
    let rig = Rig::new("prepare");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    // The hook logs its rendered inputs to a file, then prints the JSON line.
    let log = rig.dir.join("hook.log");
    let config = rig.config(&format!(
        "[hooks]\nprepare = [\"sh\", \"-c\", \"echo preparing {{branch}} from {{base}} in {{repo}} > {}; echo '{{\\\"pane_id\\\":\\\"w5:p1\\\",\\\"workspace_id\\\":\\\"w5\\\"}}'\"]\n",
        log.display()
    ));
    let (code, v, stderr) = rig.run(
        &[
            "prepare",
            "--branch",
            "feat-x",
            "--base",
            "main",
            "--repo",
            "/tmp/repo",
        ],
        Some(&config),
    );
    assert_eq!(code, 0, "{v} {stderr}");
    assert_eq!(v["pane_id"], "w5:p1");
    assert_eq!(v["workspace_id"], "w5");
    assert_eq!(v["host"]["id"], "local");
    assert_eq!(
        fs::read_to_string(&log).unwrap().trim(),
        "preparing feat-x from main in /tmp/repo"
    );

    let silent = rig.config("[hooks]\nprepare = [\"true\"]\n");
    let (code, v, _) = rig.run(&["prepare", "--branch", "feat-y"], Some(&silent));
    assert_eq!(code, 2);
    assert!(v["error"].as_str().unwrap().contains("pane_id"));
}

#[test]
fn host_primitives_run_standalone() {
    let rig = Rig::new("host");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    let (code, v, _) = rig.run(
        &[
            "host",
            "reserve",
            "--lead",
            "l",
            "--ticket",
            "w-x",
            "--host-id",
            "8103",
            "--label",
            "netcup",
            "--load-per-core-max",
            "1000",
            "--disk-free-min-percent",
            "0",
        ],
        None,
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["reserved"], true);
    assert_eq!(v["probe"]["host"], "8103");
    assert_eq!(v["probe"]["label"], "netcup");

    let (code, v, _) = rig.run(
        &[
            "host",
            "reserve",
            "--lead",
            "l",
            "--ticket",
            "w-y",
            "--load-per-core-max",
            "0",
        ],
        None,
    );
    assert_eq!(code, 1);
    assert_eq!(v["reserved"], false);

    let (code, v, _) = rig.run(
        &["host", "set-pane", "--ticket", "w-x", "--pane", "w9:p9"],
        None,
    );
    assert_eq!(code, 0);
    assert_eq!(v["found"], true);
    let (_, v, _) = rig.run(&["host", "release", "--pane", "w9:p9"], None);
    assert_eq!(v["released"], 1);
}
