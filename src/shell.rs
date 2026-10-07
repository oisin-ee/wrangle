//! The one seam between the engine and the operating system. Tests supply a
//! fake; the binary supplies `System`.

use std::ffi::OsStr;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// Captured result of a finished process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

pub trait Shell: Send + Sync + std::fmt::Debug {
    /// Run `program args…` with no stdin, capturing both streams. Returns
    /// `Err` only when the process could not be started.
    fn run(&self, program: &str, args: &[String]) -> Result<Output>;

    /// Read a whole file (for `/proc/loadavg`). Fakes script this too so the
    /// probe tests run the same on every platform.
    fn read_file(&self, path: &str) -> Result<String> {
        Ok(std::fs::read_to_string(path)?)
    }

    /// Run and treat a non-zero status as an error.
    fn run_ok(&self, program: &str, args: &[String]) -> Result<Output> {
        let out = self.run(program, args)?;
        if out.ok() {
            Ok(out)
        } else {
            Err(Error::Command {
                program: program.to_string(),
                args: args.join(" "),
                status: out.status.to_string(),
                stderr: out.stderr.trim().to_string(),
            })
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct System;

impl Shell for System {
    fn run(&self, program: &str, args: &[String]) -> Result<Output> {
        let out = Command::new(program)
            .args(args.iter().map(OsStr::new))
            .stdin(Stdio::null())
            .output()?;
        Ok(Output {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Scripted shell for tests: a map from `"program arg1 arg2"` to the output
/// to return, plus a log of every call. Scripts may change after the fake
/// has been lent out, so both maps sit behind a mutex.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct Fake {
    responses: std::sync::Mutex<std::collections::HashMap<String, Output>>,
    pub calls: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl Fake {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on(&self, command: &str, stdout: &str) -> &Self {
        self.on_output(
            command,
            Output {
                status: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        )
    }

    pub fn on_output(&self, command: &str, output: Output) -> &Self {
        if let Ok(mut r) = self.responses.lock() {
            r.insert(command.to_string(), output);
        }
        self
    }

    /// Script the one-minute load (the probe reads `/proc/loadavg` first).
    pub fn with_load(&self, load: &str) -> &Self {
        self.on("read /proc/loadavg", load)
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
impl Shell for Fake {
    /// Files are scripted under the key `read <path>`.
    fn read_file(&self, path: &str) -> Result<String> {
        self.run("read", &[path.to_string()]).map(|o| o.stdout)
    }

    fn run(&self, program: &str, args: &[String]) -> Result<Output> {
        let key = std::iter::once(program.to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(key.clone());
        }
        let scripted = self
            .responses
            .lock()
            .ok()
            .and_then(|r| r.get(&key).cloned());
        scripted.ok_or_else(|| Error::Command {
            program: program.to_string(),
            args: args.join(" "),
            status: "fake".to_string(),
            stderr: format!("no scripted response for `{key}`"),
        })
    }
}

/// Single-quote one argument for a POSIX shell on the far side of ssh.
#[must_use]
pub fn quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_leaves_safe_words_alone() {
        assert_eq!(quote("w-abc-12"), "w-abc-12");
        assert_eq!(quote("--ttl-ms=5"), "--ttl-ms=5");
    }

    #[test]
    fn quote_wraps_and_escapes() {
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote(""), "''");
    }
}
