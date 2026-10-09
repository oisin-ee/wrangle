//! Admission policy. Read once by the lead; the numeric parts travel to remote
//! hosts on the command line so every host applies the lead's rule.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Default `prepare` hook: the agent harness task that creates a writer's
/// worktree and linked Herdr workspace, printing `{pane_id, workspace_id}`.
pub const DEFAULT_PREPARE: &[&str] = &[
    "sh",
    "-c",
    "cd {repo} && mise run -q agent:worktree -- {branch} {base}",
];

/// Default way to run `wrangle` on a remote host.
pub const DEFAULT_REMOTE_COMMAND: &[&str] = &["mise", "x", "--", "wrangle"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// Admit while `load1 / cores` stays below this.
    pub load_per_core_max: f64,
    /// Admit while the home filesystem keeps at least this much free.
    pub disk_free_min_percent: f64,
    /// Load units each unexpired reservation adds, so leads admitting in the
    /// same minute do not all see the same idle host.
    pub reservation_load: f64,
    /// A reservation older than this is dropped on the next probe.
    pub reservation_ttl_ms: u64,
    /// Poll interval for queued tickets (used by the Pi shim).
    pub poll_ms: u64,
    /// argv prefix that reaches `wrangle` on a remote host over
    /// `ssh -T -o BatchMode=yes`. mise shims are not on the non-interactive
    /// PATH, but `mise` itself is, so the default goes through `mise x`.
    pub remote_command: Vec<String>,
    pub hooks: Hooks,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hooks {
    /// argv template run on the admitted host before a spawn. Placeholders:
    /// `{repo}`, `{branch}`, `{base}`. Must print one JSON object with `pane_id`.
    pub prepare: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            load_per_core_max: 1.5,
            disk_free_min_percent: 15.0,
            reservation_load: 1.0,
            reservation_ttl_ms: 180_000,
            poll_ms: 30_000,
            remote_command: DEFAULT_REMOTE_COMMAND
                .iter()
                .map(ToString::to_string)
                .collect(),
            hooks: Hooks::default(),
        }
    }
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            prepare: DEFAULT_PREPARE.iter().map(ToString::to_string).collect(),
        }
    }
}

/// The subset of the policy a remote host needs to re-check admission under
/// its own lock. Serialised on the command line of `wrangle host reserve`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    pub load_per_core_max: f64,
    pub disk_free_min_percent: f64,
    pub reservation_load: f64,
    pub reservation_ttl_ms: u64,
}

impl From<&Policy> for Thresholds {
    fn from(p: &Policy) -> Self {
        Self {
            load_per_core_max: p.load_per_core_max,
            disk_free_min_percent: p.disk_free_min_percent,
            reservation_load: p.reservation_load,
            reservation_ttl_ms: p.reservation_ttl_ms,
        }
    }
}

/// The optional repository file, at the root of the main checkout.
pub const REPO_FILE: &str = "wrangle.toml";

/// `wrangle.toml` in a repository: how that repository prepares a child.
/// Admission thresholds stay in the host's policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoConfig {
    pub hooks: RepoHooks,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoHooks {
    /// Replaces the policy's `hooks.prepare` for this repository.
    pub prepare: Option<Vec<String>>,
}

impl RepoConfig {
    /// The `prepare` template for this repository: its own, else the policy's.
    #[must_use]
    pub fn prepare<'a>(&'a self, policy: &'a Policy) -> &'a [String] {
        self.hooks
            .prepare
            .as_deref()
            .unwrap_or(&policy.hooks.prepare)
    }
}

/// Read `wrangle.toml` from `checkout`. A missing file means no override; an
/// invalid file is an error, like the policy file.
pub fn load_repo(checkout: &Path) -> Result<RepoConfig> {
    let path = checkout.join(REPO_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|source| Error::Config {
            path: path.display().to_string(),
            source,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RepoConfig::default()),
        Err(e) => Err(e.into()),
    }
}

/// `$WRANGLE_CONFIG`, else `$XDG_CONFIG_HOME/wrangle/config.toml`, else
/// `~/.config/wrangle/config.toml`.
#[must_use]
pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("WRANGLE_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("wrangle").join("config.toml")
}

/// A missing file means defaults. A present but invalid file is an error, so a
/// typo never silently reverts to the defaults.
pub fn load(path: &Path) -> Result<Policy> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|source| Error::Config {
            path: path.display().to_string(),
            source,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Policy::default()),
        Err(e) => Err(e.into()),
    }
}

pub fn parse(text: &str) -> std::result::Result<Policy, toml::de::Error> {
    toml::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_file_overrides_only_what_it_names() {
        let policy = Policy::default();
        let none: RepoConfig = toml::from_str("[hooks]\n").unwrap();
        assert_eq!(none.prepare(&policy), policy.hooks.prepare.as_slice());
        let own: RepoConfig =
            toml::from_str("[hooks]\nprepare = [\"./prepare\", \"{branch}\"]\n").unwrap();
        assert_eq!(own.prepare(&policy), ["./prepare", "{branch}"]);
        assert!(toml::from_str::<RepoConfig>("load_per_core_max = 2.0\n").is_err());
    }

    #[test]
    fn missing_repo_file_is_no_override() {
        let dir = std::env::temp_dir().join(format!("wrangle-repo-cfg-{}", std::process::id()));
        assert_eq!(load_repo(&dir).unwrap(), RepoConfig::default());
    }

    #[test]
    fn empty_text_is_defaults() {
        assert_eq!(parse("").unwrap(), Policy::default());
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let p = parse("load_per_core_max = 2.0\n[hooks]\nprepare = [\"true\"]\n").unwrap();
        assert!((p.load_per_core_max - 2.0).abs() < f64::EPSILON);
        assert!((p.disk_free_min_percent - 15.0).abs() < f64::EPSILON);
        assert_eq!(p.hooks.prepare, vec!["true".to_string()]);
    }

    #[test]
    fn unknown_key_is_rejected() {
        assert!(parse("max_agents = 3\n").is_err());
    }

    #[test]
    fn missing_file_is_defaults() {
        let p = load(Path::new("/nonexistent/wrangle/config.toml")).unwrap();
        assert_eq!(p, Policy::default());
    }
}
