//! Typed, actionable failures rendered only at the executable edge.
use miette::Diagnostic;
use thiserror::Error;

pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum Error {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Runtime(#[from] crate::runtime::RuntimeError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    State(#[from] crate::model::state::StateError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Indicators(#[from] crate::indicators::Error),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Discovery(#[from] crate::model::input::Problem),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Registry(#[from] crate::registry::RegistryError),
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    #[diagnostic(transparent)]
    Ownership(#[from] crate::platform::linux::ownership::OwnershipError),
    // Selection remains part of the portable application contract, even without a platform host.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[error("no unique discovered device matches {} ({matches} matches)", selector.display())]
    #[diagnostic(
        code(kbctl::cli::device_selection),
        help(
            "Run `kbctl device list` and use an exact current sysfs or /dev/input/event* path. Process-local IDs are not selectors; retry discovery if it is degraded."
        )
    )]
    Selection {
        selector: std::path::PathBuf,
        matches: usize,
    },
    #[cfg(not(target_os = "linux"))]
    #[error("host input discovery and indicator access are unsupported on this platform")]
    #[diagnostic(
        code(kbctl::cli::unsupported_platform),
        help("Run these commands on Linux; other platform adapters are not implemented yet.")
    )]
    UnsupportedPlatform,
    #[error("cannot {operation}")]
    #[diagnostic(
        code(kbctl::error::io),
        help("Check the output destination or host polling/signal resources, then retry.")
    )]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot render JSON output")]
    #[diagnostic(
        code(kbctl::cli::json),
        help("Check the output destination and retry with a writable stream.")
    )]
    Json(#[from] console_json::Error),
}

impl Error {
    pub(crate) fn io(operation: &'static str, source: std::io::Error) -> Self {
        Self::Io { operation, source }
    }

    pub(crate) fn is_broken_pipe(&self) -> bool {
        // JSON preserves its underlying I/O cause; consumer shutdown is normal for a streaming CLI.
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(self);
        while let Some(error) = cause {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
                || error
                    .downcast_ref::<serde_json::Error>()
                    .is_some_and(|e| e.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe))
            {
                return true;
            }
            cause = error.source();
        }
        false
    }
}
