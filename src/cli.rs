//! CLI definition (clap derive) + handlers for set/apply/defaults/reset

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::baseboard::{self, Baseboard};
use crate::config;
use crate::defaults;
use crate::error::Error;
use crate::legacy::{self, Action};
use crate::netlink;

/// GOcontroll CAN interface configuration tool.
///
/// Run without arguments to open the interactive TUI.
#[derive(Parser, Debug)]
#[command(name = "go-can", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

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

    /// Take the CAN settings over from the legacy ifupdown file and move it
    /// aside, so that go-can alone sets up the buses from the next boot.
    /// Running buses are not touched. Runs from the package's postinst.
    Migrate {
        /// The legacy ifupdown file.
        #[arg(long, default_value = crate::legacy::LEGACY_PATH)]
        from: PathBuf,
        /// Show what would happen; change nothing.
        #[arg(long)]
        dry_run: bool,
    },

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

/// `go-can migrate`: see crate::legacy. A missing file is not an error, so the
/// postinst can call this on every controller.
pub fn handle_migrate(from: &Path, dry_run: bool, json: bool, quiet: bool) -> Result<(), Error> {
    let text = match std::fs::read_to_string(from) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if json {
                let j = serde_json::json!({
                    "schema_version": 1,
                    "source": from.display().to_string(),
                    "found": false,
                    "dry_run": dry_run,
                    "migrated": false,
                });
                println!("{}", serde_json::to_string_pretty(&j)?);
            } else if !quiet {
                println!("[go-can] {} not found: nothing to migrate", from.display());
            }
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let ifaces = legacy::parse(&text).map_err(|reason| {
        Error::UserError(format!(
            "{f} not migrated: {reason}. Nothing was changed. Apply the bitrate of every CAN bus \
             with go-can, then remove {f}.",
            f = from.display()
        ))
    })?;
    let config_dir = Path::new(config::CONFIG_DIR);
    let items = legacy::plan(ifaces, config_dir)?;
    let backup = if dry_run {
        None
    } else {
        Some(legacy::execute(
            &items,
            from,
            config_dir,
            Path::new(legacy::BACKUP_DIR),
        )?)
    };

    if json {
        let ifs: Vec<serde_json::Value> = items
            .iter()
            .map(|i| {
                let existing = match &i.action {
                    Action::KeepExisting { existing } => Some(existing),
                    _ => None,
                };
                serde_json::json!({
                    "name": i.iface.name,
                    "action": i.action_name(),
                    "at_boot": i.iface.at_boot,
                    "config": i.iface.cfg,
                    "existing": existing,
                })
            })
            .collect();
        let j = serde_json::json!({
            "schema_version": 1,
            "source": from.display().to_string(),
            "found": true,
            "dry_run": dry_run,
            "migrated": backup.is_some(),
            "backup": backup.as_ref().map(|p| p.display().to_string()),
            "interfaces": ifs,
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    if quiet {
        return Ok(());
    }

    let would = if dry_run { "would be " } else { "" };
    println!("[go-can] CAN settings in {}:", from.display());
    for i in &items {
        let f = &i.iface;
        let line = match &i.action {
            Action::Migrate => format!(
                "{}: {} bit/s, {would}taken over into {}/{}.conf",
                f.name,
                f.cfg.bitrate,
                config::CONFIG_DIR,
                f.name
            ),
            Action::KeepExisting { existing } if *existing == f.cfg => {
                format!("{}: go-can already has the same settings", f.name)
            }
            Action::KeepExisting { existing } => format!(
                "{}: go-can keeps its own settings ({} bit/s); can.conf differs ({} bit/s)",
                f.name, existing.bitrate, f.cfg.bitrate
            ),
            Action::NotAtBoot => format!(
                "{}: not brought up at boot by can.conf, so go-can does not bring it up either",
                f.name
            ),
        };
        println!("  {line}");
        if i.action == Action::Migrate && f.txqueuelen_default {
            println!(
                "  {}: can.conf sets no txqueuelen; go-can uses {}",
                f.name, f.cfg.txqueuelen
            );
        }
    }
    match backup {
        Some(p) => {
            println!("[go-can] moved {} to {}", from.display(), p.display());
            println!(
                "[go-can] the CAN buses keep running as they are; from the next boot go-can sets them up"
            );
        }
        None => println!("[go-can] dry run: nothing was changed"),
    }
    Ok(())
}
