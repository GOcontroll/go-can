//! CLI definition (clap derive) + handlers for set/apply/defaults/reset

use clap::{Parser, Subcommand};

use crate::baseboard::{self, Baseboard};
use crate::config;
use crate::defaults;
use crate::error::Error;
use crate::netlink;

/// GOcontroll CAN interface configuration tool.
#[derive(Parser, Debug)]
#[command(name = "go-can", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Machine-readable JSON output (schema_version=1).
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-error output.
    #[arg(short, long, global = true)]
    pub quiet: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List all CAN interfaces present on the system.
    List,

    /// Show full state of one interface (config + live).
    Show {
        /// Interface name, e.g. can0.
        iface: String,
    },

    /// Set a parameter (writes to .conf + applies live).
    Set {
        /// Interface name, e.g. can0.
        iface: String,
        /// Parameter name. One of: bitrate, data-bitrate, fd, restart-ms,
        /// txqueuelen, sample-point, triple-sampling, loopback, listen-only.
        key: String,
        /// Parameter value.
        value: String,
    },

    /// Apply on-disk config to live interface (used by systemd).
    Apply {
        /// Interface name, e.g. can0.
        iface: String,
    },

    /// Write per-baseboard CAN defaults to /etc/gocontroll/can.d/.
    Defaults {
        /// Auto-detect baseboard from device-tree.
        #[arg(long, conflicts_with = "baseboard")]
        auto_detect: bool,

        /// Force a specific baseboard (L4, M1, HMI1).
        #[arg(long)]
        baseboard: Option<String>,
    },

    /// Print detected baseboard (L4/M1/HMI1/unknown).
    DetectBaseboard,

    /// Restore interface to baseboard defaults.
    Reset {
        /// Interface name, e.g. can0.
        iface: String,
    },

    /// Interactive TUI — browse and configure CAN interfaces with arrow keys.
    Tui,
}

pub fn handle_set(iface: &str, key: &str, value: &str, quiet: bool) -> Result<(), Error> {
    let mut cfg = config::load_or_default(iface);
    cfg.set_key(key, value)?;
    config::save(iface, &cfg)?;
    if !quiet {
        eprintln!("[go-can] {iface}: set {key}={value} (config saved)");
    }
    netlink::apply(iface, &cfg)?;
    if !quiet {
        eprintln!("[go-can] {iface}: applied live");
    }
    Ok(())
}

pub fn handle_apply(iface: &str, quiet: bool) -> Result<(), Error> {
    let cfg = config::load(iface)?;
    netlink::apply(iface, &cfg)?;
    if !quiet {
        eprintln!("[go-can] {iface}: applied (bitrate={} fd={})", cfg.bitrate, cfg.fd);
    }
    Ok(())
}

pub fn handle_defaults(
    auto_detect: bool,
    baseboard_arg: Option<&str>,
    json: bool,
    quiet: bool,
) -> Result<(), Error> {
    let bb = if auto_detect {
        baseboard::detect()?
    } else if let Some(s) = baseboard_arg {
        Baseboard::parse(s)?
    } else {
        return Err(Error::UserError(
            "either --auto-detect or --baseboard <name> required".into(),
        ));
    };

    let written = defaults::write_for(bb)?;
    if json {
        let j = serde_json::json!({
            "schema_version": 1,
            "baseboard": bb.as_str(),
            "interfaces_written": written,
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
    } else if !quiet {
        eprintln!(
            "[go-can] baseboard={} → wrote {} interface config(s): {}",
            bb.as_str(),
            written.len(),
            written.join(", ")
        );
    }
    Ok(())
}

pub fn handle_reset(iface: &str, quiet: bool) -> Result<(), Error> {
    let bb = baseboard::detect()?;
    let cfg = defaults::config_for(bb, iface)
        .ok_or_else(|| Error::UserError(format!("no default for {iface} on baseboard {}", bb.as_str())))?;
    config::save(iface, &cfg)?;
    netlink::apply(iface, &cfg)?;
    if !quiet {
        eprintln!("[go-can] {iface}: reset to baseboard defaults");
    }
    Ok(())
}
