//! The kbctl binary.

#![allow(clippy::result_large_err)]

use std::process::ExitCode;

use clap::Parser;
use miette::MietteHandlerOpts;

mod application;
mod backends;
mod cli;
mod error;
mod indicators;
mod input;
mod model;
mod output;
mod platform;
mod registry;
mod runtime;
mod state;
#[cfg(test)]
mod test_support;
mod transport;

use application::Operation;
use cli::{Cli, Command, DeviceCommand, IndicatorCommand};
use error::Result;

fn main() -> ExitCode {
    let args = Cli::parse();
    let verbose = args.verbose > 0 || std::env::var_os("RUST_BACKTRACE").is_some();
    install_miette_hook(verbose);

    let operation = match args.command {
        Command::Device {
            command: DeviceCommand::List,
        } => Operation::Devices,
        Command::Device {
            command: DeviceCommand::Inspect { device },
        } => Operation::Inspect(device),
        Command::Indicator {
            command: IndicatorCommand::Status { device },
        } => Operation::Indicators { device },
        Command::Indicator {
            command: IndicatorCommand::Watch,
        } => Operation::Watch,
        Command::Status => Operation::Status,
    };
    let result = run(&operation, args.json, args.verbose > 0);

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => {
            // Rendered here rather than returned as `miette::Result`, so the
            // exit code is chosen explicitly instead of inherited from
            // whatever the report wrapper decides.
            if args.json {
                if let Err(render) = output::stdout(
                    &output::Document::failure(&operation, &error),
                    true,
                    args.verbose > 0,
                ) {
                    if render.is_broken_pipe() {
                        return ExitCode::SUCCESS;
                    }
                    eprintln!("{:?}", miette::Report::new(render));
                }
            } else {
                eprintln!("{:?}", miette::Report::new(error));
            }
            ExitCode::FAILURE
        }
    }
}

fn run(operation: &Operation, json: bool, detail: bool) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mut host = platform::linux::application::LinuxHost::new(operation)?;
        application::execute(&mut host, operation, |document| {
            output::stdout(document, json, detail)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (operation, json, detail);
        Err(error::Error::UnsupportedPlatform)
    }
}

/// Install miette's diagnostic handler.
///
/// `--verbose` (or `RUST_BACKTRACE`, so a bug report already carries it)
/// prints the full cause chain; otherwise only the primary diagnostic
/// shows, since a source error the user did not ask for is noise more
/// often than it is the answer. Colour is left to miette's own
/// auto-detection so `NO_COLOR`/`TERM=dumb` still produce plain output.
fn install_miette_hook(verbose: bool) {
    let _ = miette::set_hook(Box::new(move |_| {
        let opts = MietteHandlerOpts::new();
        let opts = if verbose {
            opts.with_cause_chain()
        } else {
            opts.without_cause_chain()
        };
        Box::new(opts.build())
    }));
}
