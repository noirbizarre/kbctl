//! Argument types only.
//!
//! No behaviour lives here: parsing is one concern and doing the work is
//! another, and keeping them apart is what lets `build.rs` and the tests reuse
//! the argument definitions without running the program.

use clap::{Parser, Subcommand};

/// A unified control plane for programmable keyboards
#[derive(Debug, Parser)]
#[command(name = "kbctl", version, about, long_about = None)]
pub struct Cli {
    /// Increase verbosity. Repeat for more.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Render JSON (colored on terminals, compact and uncolored when piped).
    #[arg(long, global = true)]
    pub json: bool,

    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Discover host input devices (metadata access only).
    Device {
        #[command(subcommand)]
        command: DeviceCommand,
    },
    /// Read endpoint-scoped Caps Lock, Num Lock and Scroll Lock state.
    Indicator {
        #[command(subcommand)]
        command: IndicatorCommand,
    },
    /// Summarize discovery and per-endpoint host indicator state.
    Status,
}

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    /// List all discovered input groups and their endpoints.
    List,
    /// Inspect a current sysfs path or /dev/input/event* locator.
    #[command(
        after_help = "Example: kbctl device inspect /dev/input/event0\nPaths are current locators, not persistent device identities."
    )]
    Inspect { device: std::path::PathBuf },
}

#[derive(Debug, Subcommand)]
pub enum IndicatorCommand {
    /// Query kernel-maintained indicator state for each eligible endpoint.
    Status,
    /// Watch initial state and changes until Ctrl-C (NDJSON when piped with --json).
    #[command(
        after_help = "Example: kbctl --json indicator watch\nEndpoints can disagree. Kernel state does not prove physical LED illumination."
    )]
    Watch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    // Catches the derive mistakes that would otherwise only surface as a panic
    // the first time a user runs the binary.
    #[test]
    fn the_command_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
