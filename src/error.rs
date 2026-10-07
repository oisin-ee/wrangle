//! One error type for the whole binary. `main` maps it to an exit code.

use std::process::ExitCode;

/// Exit code when every eligible host is at capacity. Not an error for callers
/// that queue; the JSON body carries the detail.
pub const EXIT_FULL: u8 = 1;
/// Exit code for every other failure.
pub const EXIT_ERROR: u8 = 2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("config {path}: {source}")]
    Config {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("{program} {args}: exit {status}: {stderr}")]
    Command {
        program: String,
        args: String,
        status: String,
        stderr: String,
    },
    #[error("ssh {target}: {detail}")]
    Ssh { target: String, detail: String },
    #[error("unexpected output from {origin}: {detail}")]
    Parse { origin: String, detail: String },
    #[error("unknown machine {0}; see `wrangle probe`")]
    UnknownMachine(String),
    #[error("ticket {0} is not queued or reserved")]
    UnknownTicket(String),
    #[error("every eligible host is full")]
    Full,
    #[error("{0}")]
    Invalid(String),
    #[error("prepare hook printed no `pane_id`: {detail}")]
    Hook { detail: String },
}

impl Error {
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Full => ExitCode::from(EXIT_FULL),
            _ => ExitCode::from(EXIT_ERROR),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
