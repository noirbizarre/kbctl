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
    /// Show endpoint/metadata details and diagnostic causes (JSON is always complete).
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
    /// List recognized typing keyboards, grouped by input attachment.
    List {
        /// Include partial keyboards, other input devices and unknown classifications.
        #[arg(long)]
        all: bool,
    },
    /// Inspect a device selector (or a current sysfs/event-node locator).
    #[command(
        after_help = "Use the readable selector printed by `kbctl device list`.\nExample: kbctl device inspect gmmk-pro\nLegacy technical selectors remain accepted. Transient inspection also accepts /dev/input/event0 or an exact sysfs path."
    )]
    Inspect {
        #[arg(value_name = "SELECTOR", value_hint = clap::ValueHint::Other)]
        device: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum IndicatorCommand {
    /// Query device-grouped kernel indicator state; --verbose shows endpoints.
    Status {
        /// Select a readable device handle (e.g. dygma-defy); --all listing also exposes partial keyboards.
        #[arg(long, value_name = "SELECTOR", value_hint = clap::ValueHint::Other)]
        device: Option<String>,
    },
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

    #[test]
    fn device_selection_and_detail_flags_parse_without_running_a_host() {
        let args = Cli::try_parse_from([
            "kbctl",
            "indicator",
            "status",
            "--device",
            "usb-0123",
            "--verbose",
            "--json",
        ])
        .unwrap();
        assert_eq!(args.verbose, 1);
        assert!(args.json);
        assert!(
            matches!(args.command, Command::Indicator { command: IndicatorCommand::Status { device: Some(device) } } if device == "usb-0123")
        );
        assert!(Cli::try_parse_from(["kbctl", "indicator", "status", "--device"]).is_err());
    }

    #[test]
    fn all_is_an_explicit_device_list_inventory_option() {
        let args = Cli::try_parse_from(["kbctl", "device", "list", "--all", "--json"]).unwrap();
        assert!(matches!(
            args.command,
            Command::Device {
                command: DeviceCommand::List { all: true }
            }
        ));
        assert!(Cli::try_parse_from(["kbctl", "indicator", "status", "--all"]).is_err());
    }

    #[test]
    fn selector_arguments_have_explicit_non_path_completion_hints() {
        let command = Cli::command();
        let inspect = command
            .find_subcommand("device")
            .unwrap()
            .find_subcommand("inspect")
            .unwrap();
        let status = command
            .find_subcommand("indicator")
            .unwrap()
            .find_subcommand("status")
            .unwrap();
        for command in [inspect, status] {
            let selector = command
                .get_arguments()
                .find(|arg| arg.get_id() == "device")
                .unwrap();
            assert_eq!(selector.get_value_hint(), clap::ValueHint::Other);
            assert_eq!(selector.get_value_names().unwrap(), ["SELECTOR"]);
        }
    }
}
