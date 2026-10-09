//! Run the real binary against a fake `herdr` on PATH and a private state
//! directory. Only the local host exists (the fake machine list is empty).

// Test-only crate: the rig helpers may unwrap and panic like the #[test] fns.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A command without the `GIT_*` variables of the caller. A git hook (such as
/// the pre-commit check) exports `GIT_DIR` and `GIT_INDEX_FILE`; inherited,
/// they point the test's git calls at the real repository.
fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd
}

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
        const SCRIPT: &str = r#"#!/bin/sh
echo "$@" >> "@LOG@"
case "$*" in
  'machine list --json') echo '[]' ;;
  'agent list') echo '@AGENTS@' ;;
  'workspace create'*) echo '{"result":{"root_pane":{"pane_id":"w8:p1","workspace_id":"w8","tab_id":"w8:t1"}}}' ;;
  'tab create'*) echo '{"result":{"root_pane":{"pane_id":"w4:p9","workspace_id":"w4","tab_id":"w4:t9"}}}' ;;
  'agent start '*'--kind broken'*) echo 'agent_pane_busy' >&2; exit 1 ;;
  'agent start '*'--kind blocked'*) echo '{"error":{"code":"agent_not_ready"}}'; exit 1 ;;
  *) : ;;
esac
"#;
        let script = SCRIPT
            .replace("@LOG@", &self.dir.join("calls.log").display().to_string())
            .replace("@AGENTS@", agents);
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
        self.run_env(args, config, &[])
    }

    fn run_env(
        &self,
        args: &[&str],
        config: Option<&Path>,
        env: &[(&str, &str)],
    ) -> (i32, serde_json::Value, String) {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = isolated(env!("CARGO_BIN_EXE_wrangle"));
        cmd.args(args)
            .arg("--json")
            .env("PATH", path)
            .env("WRANGLE_STATE", self.dir.join("state"))
            .env("HOME", self.dir.join("home"))
            .env_remove("HERDR_PANE_ID")
            .envs(env.iter().copied());
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
fn spawn_release_writes_private_history_and_report_needs_no_herdr() {
    let rig = Rig::new("lifecycle");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    let config = rig.config("load_per_core_max = 1000\ndisk_free_min_percent = 0\n");
    let (code, spawned, _) = rig.run(
        &[
            "spawn",
            "--lead",
            "lead",
            "--kind",
            "pi",
            "--name",
            "child",
            "--message",
            "private task contents",
            "--cwd",
            "/tmp",
        ],
        Some(&config),
    );
    assert_eq!(code, 0, "{spawned}");
    let ticket = spawned["ticket"].as_str().unwrap();
    let (code, _, _) = rig.run(&["release", "--ticket", ticket], Some(&config));
    assert_eq!(code, 0);
    let contents = fs::read_to_string(rig.dir.join("state/events.jsonl")).unwrap();
    assert!(!contents.contains("private task contents"));
    let events: Vec<serde_json::Value> = contents
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 3);
    for (event, expected) in events.iter().zip(["admitted", "spawned", "released"]) {
        assert_eq!(event["event"], expected);
        assert_eq!(event["ticket"], ticket);
    }
    // Reports read the ledger without discovering or probing the fleet.
    fs::remove_file(rig.dir.join("bin/herdr")).unwrap();
    let (code, report, _) = rig.run(&["report", "--since", "1h"], Some(&config));
    assert_eq!(code, 0);
    assert_eq!(report["tickets"][0]["queue_wait_ms"], 0);
    let runtime = events[2]["ts_ms"].as_u64().unwrap() - events[1]["ts_ms"].as_u64().unwrap();
    assert_eq!(report["tickets"][0]["run_time_ms"], runtime);
    let text = Command::new(env!("CARGO_BIN_EXE_wrangle"))
        .args(["report", "--since", "1h"])
        .env("WRANGLE_STATE", rig.dir.join("state"))
        .env("WRANGLE_CONFIG", &config)
        .output()
        .unwrap();
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains(ticket));
    assert!(text.contains("run time (ms)"));
    assert!(text.lines().nth(1).unwrap().ends_with(&runtime.to_string()));
}

#[test]
fn disk_refusal_is_in_status_and_history_report() {
    let rig = Rig::new("disk-refusal");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    let config = rig.config("disk_free_min_percent = 100\n");
    let (code, queued, _) = rig.run(&["admit", "--lead", "lead"], Some(&config));
    assert_eq!(code, 1, "{queued}");
    let (code, status, _) = rig.run(&["status"], Some(&config));
    assert_eq!(code, 0);
    let reason = status["hosts"][0]["headroom"]["reason"].as_str().unwrap();
    assert!(reason.starts_with("disk "));
    let (code, report, _) = rig.run(&["report"], Some(&config));
    assert_eq!(code, 0);
    assert_eq!(report["hosts"][0]["host"], "local");
    assert_eq!(report["hosts"][0]["count"], 1);
    assert_eq!(report["hosts"][0]["reasons"][reason], 1);
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
    let strict = rig.config("load_per_core_max = 0.0\ndisk_free_min_percent = 0\n");
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
        "pane report-metadata w1:p3 --source wrangle --display-agent ↳ reviewer --token sub=● --token lead=lead-a"
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
fn prepare_from_a_linked_worktree_enters_the_main_checkout() {
    let rig = Rig::new("prepare-worktree");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    let main = rig.dir.join("home").join("any").join("repo");
    let linked = main.join(".worktrees").join("unit");
    fs::create_dir_all(&main).unwrap();
    let git = |args: &[&str]| {
        let out = isolated("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&main)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "unit",
        linked.to_str().unwrap(),
    ]);
    let top = git(&["rev-parse", "--show-toplevel"]);

    let log = rig.dir.join("hook.log");
    let config = rig.config(&format!(
        "[hooks]\nprepare = [\"sh\", \"-c\", \"echo {{repo}} > {}; echo '{{\\\"pane_id\\\":\\\"w5:p1\\\"}}'\"]\n",
        log.display()
    ));
    let (code, v, stderr) = rig.run(
        &[
            "prepare",
            "--branch",
            "feat-x",
            "--repo",
            linked.to_str().unwrap(),
        ],
        Some(&config),
    );
    assert_eq!(code, 0, "{v} {stderr}");
    assert_eq!(fs::read_to_string(&log).unwrap().trim(), top);
}

#[test]
fn spawn_places_starts_marks_and_prompts_for_non_pi_harnesses() {
    let rig = Rig::new("spawn");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    let loose = rig.config("load_per_core_max = 1000\ndisk_free_min_percent = 0\n");
    let (code, v, stderr) = rig.run(
        &[
            "spawn",
            "--lead",
            "claude-main",
            "--kind",
            "claude",
            "--name",
            "unit-a",
            "--message",
            "do the thing",
            "--cwd",
            "/tmp/repo",
        ],
        Some(&loose),
    );
    assert_eq!(code, 0, "{v} {stderr}");
    assert_eq!(v["spawned"], true);
    assert_eq!(v["status"], "working");
    assert_eq!(v["pane_id"], "w8:p1");
    assert_eq!(v["workspace_id"], "w8");
    assert_eq!(v["host"]["id"], "local");
    let ticket = v["ticket"].as_str().unwrap().to_string();
    let calls = rig.calls();
    for expected in [
        "workspace create --cwd /tmp/repo --label unit-a --no-focus",
        "agent start unit-a --kind claude --pane w8:p1",
        "pane report-metadata w8:p1 --source wrangle --display-agent ↳ unit-a --token sub=● --token lead=claude-main",
        "agent prompt unit-a do the thing",
    ] {
        assert!(
            calls.contains(expected),
            "missing `{expected}` in:\n{calls}"
        );
    }
    // The reservation now carries the pane and the queue entry is gone.
    let (_, st, _) = rig.run(&["status"], Some(&loose));
    assert_eq!(st["queue"].as_array().unwrap().len(), 0);
    let resv = &st["hosts"][0]["probe"]["reservations"][0];
    assert_eq!(resv["ticket"], ticket);
    assert_eq!(resv["pane"], "w8:p1");

    // Without --lead the spawn claims the repo's lead on $HERDR_PANE_ID and
    // opens the child as a tab in the lead's workspace; a failed start closes
    // the tab and releases.
    let (code, v, _) = rig.run_env(
        &[
            "spawn",
            "--repo",
            "rondo",
            "--kind",
            "broken",
            "--name",
            "unit-b",
            "--message",
            "x",
        ],
        Some(&loose),
        &[("HERDR_PANE_ID", "w4:p1")],
    );
    assert_eq!(code, 2, "{v}");
    assert!(v["error"].as_str().unwrap().contains("agent start"), "{v}");
    let calls = rig.calls();
    assert!(
        calls.contains(
            "pane report-metadata w4:p1 --source wrangle --display-agent ⌂ rondo --token role=lead --token repo=rondo"
        ),
        "{calls}"
    );
    assert!(calls.contains("tab create --workspace w4"), "{calls}");
    assert!(calls.contains("tab close w4:t9"), "{calls}");
    let (_, st, _) = rig.run(&["status"], Some(&loose));
    let reservations = st["hosts"][0]["probe"]["reservations"].as_array().unwrap();
    assert_eq!(reservations.len(), 1, "{st}");
}

#[test]
fn one_lead_per_repo() {
    let rig = Rig::new("lead");
    rig.fake_herdr(
        r#"{"result":{"agents":[{"agent_status":"working","pane_id":"w42:p1","workspace_id":"w42","name":"main","tokens":{"role":"lead","repo":"rondo"}},{"agent_status":"idle","pane_id":"w42:pX","name":"kid","tokens":{"lead":"lead:rondo","sub":"\u25cf"}}]}}"#,
    );
    let (code, v, _) = rig.run_env(
        &["lead", "--repo", "rondo"],
        None,
        &[("HERDR_PANE_ID", "w42:p7")],
    );
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["lead_exists"], true);
    assert_eq!(v["pane"], "w42:p1");

    // A spawn from the second session is refused before it admits anything.
    let (code, v, _) = rig.run_env(
        &[
            "spawn",
            "--repo",
            "rondo",
            "--kind",
            "pi",
            "--name",
            "u",
            "--message",
            "m",
        ],
        None,
        &[("HERDR_PANE_ID", "w42:p7")],
    );
    assert_eq!(code, 2, "{v}");
    assert!(v["error"].as_str().unwrap().contains("agents send"), "{v}");
    assert!(!rig.calls().contains("agent start"));

    let (code, v, _) = rig.run(
        &["lead", "--repo", "rondo", "--pane", "w42:p7", "--take-over"],
        None,
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["lead"], "lead:rondo");
    assert_eq!(v["took_over"], "w42:p1");
    assert_eq!(v["workspace_id"], "w42");

    let (_, st, _) = rig.run(&["status"], None);
    assert_eq!(st["leads"][0]["lead"], "lead:rondo");
    assert_eq!(st["leads"][0]["children"][0]["name"], "kid");
}

#[test]
fn spawn_keeps_a_pane_whose_agent_is_blocked_on_a_startup_dialog() {
    let rig = Rig::new("spawn-blocked");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    let loose = rig.config("load_per_core_max = 1000\ndisk_free_min_percent = 0\n");
    let (code, v, _) = rig.run(
        &[
            "spawn",
            "--lead",
            "me",
            "--kind",
            "blocked",
            "--name",
            "unit-d",
            "--message",
            "later",
        ],
        Some(&loose),
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["status"], "blocked");
    assert!(v["next"].as_str().unwrap().contains("agent prompt unit-d"));
    let calls = rig.calls();
    assert!(
        !calls.contains("agent prompt unit-d later"),
        "prompt held back:\n{calls}"
    );
    assert!(!calls.contains("workspace close"), "pane kept:\n{calls}");
    assert!(
        calls.contains("--display-agent ↳ unit-d"),
        "marked:\n{calls}"
    );
}

#[test]
fn spawn_queues_when_full_and_resumes_with_the_ticket() {
    let rig = Rig::new("spawn-queue");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    // Full: exit 1 with a ticket; the same ticket resumes the spawn later.
    let strict = rig.config("load_per_core_max = 0\ndisk_free_min_percent = 0\n");
    let (code, v, _) = rig.run(
        &[
            "spawn",
            "--lead",
            "codex-main",
            "--kind",
            "codex",
            "--name",
            "unit-c",
            "--message",
            "y",
        ],
        Some(&strict),
    );
    assert_eq!(code, 1, "{v}");
    let ticket = v["ticket"].as_str().unwrap().to_string();
    // `config` writes one file; restore the loose policy before resuming.
    let loose = rig.config("load_per_core_max = 1000\ndisk_free_min_percent = 0\n");
    let (code, v, _) = rig.run(
        &[
            "spawn",
            "--ticket",
            &ticket,
            "--kind",
            "codex",
            "--name",
            "unit-c",
            "--message",
            "y",
        ],
        Some(&loose),
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ticket"], ticket);
    assert!(
        rig.calls().contains("--token lead=codex-main"),
        "lead comes from the queue entry"
    );
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

#[test]
fn status_sidebar_prints_the_herdr_rows() {
    let rig = Rig::new("sidebar");
    rig.fake_herdr(r#"{"result":{"agents":[]}}"#);
    let (code, v, _) = rig.run(&["status", "--sidebar"], None);
    assert_eq!(code, 0, "{v}");
    let text = v["sidebar"].as_str().unwrap();
    assert!(text.contains("[ui.sidebar.agents]"));
    assert!(text.contains("$lead"));
    assert!(text.contains("starts_with = \"⌂\""));
    // It is valid TOML with the rows Herdr reads.
    let parsed: toml::Value = toml::from_str(text).unwrap();
    assert!(parsed["ui"]["sidebar"]["agents"]["rows"].is_array());
}
