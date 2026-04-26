//! go-can — GOcontroll CAN interface configuration CLI
//!
//! Single source of truth for CAN-config on Moduline controllers.
//! Replaces the old /etc/network/interfaces.d/can.conf mechanism with
//! per-interface KEY=VALUE configs in /etc/gocontroll/can.d/, plus a
//! systemd-template (can@.service) that reads them at boot.

use clap::Parser;

mod baseboard;
mod cli;
mod config;
mod defaults;
mod error;
mod netlink;
mod output;

use cli::{Cli, Command};
use error::Error;

fn main() {
    let cli = Cli::parse();
    let result = run(cli);
    std::process::exit(match result {
        Ok(()) => 0,
        Err(e) => {
            // Errors go to stderr; --json output went to stdout already (or nothing).
            eprintln!("error: {e}");
            e.exit_code()
        }
    });
}

fn run(cli: Cli) -> Result<(), Error> {
    let json = cli.json;
    let quiet = cli.quiet;

    match cli.command {
        Command::List => {
            let snapshot = output::Snapshot::collect()?;
            output::print_list(&snapshot, json)?;
        }
        Command::Show { iface } => {
            let info = output::IfaceInfo::collect(&iface)?;
            output::print_show(&info, json)?;
        }
        Command::Set { iface, key, value } => {
            cli::handle_set(&iface, &key, &value, quiet)?;
        }
        Command::Apply { iface } => {
            cli::handle_apply(&iface, quiet)?;
        }
        Command::Defaults { auto_detect, baseboard } => {
            cli::handle_defaults(auto_detect, baseboard.as_deref(), json, quiet)?;
        }
        Command::DetectBaseboard => {
            let bb = baseboard::detect()?;
            if json {
                let j = serde_json::json!({
                    "schema_version": 1,
                    "baseboard": bb.as_str(),
                });
                println!("{}", serde_json::to_string_pretty(&j)?);
            } else {
                println!("{}", bb.as_str());
            }
        }
        Command::Reset { iface } => {
            cli::handle_reset(&iface, quiet)?;
        }
    }
    Ok(())
}
