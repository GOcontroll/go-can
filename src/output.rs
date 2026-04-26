//! Human + JSON output for `list` and `show`.
//!
//! JSON schema is stable: `schema_version: 1`. Bump on any breaking change.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::baseboard::{self, Baseboard};
use crate::config::{self, CanConfig};
use crate::error::Error;

const NET_SYSFS: &str = "/sys/class/net";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub baseboard: String,
    pub interfaces: Vec<IfaceSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IfaceSummary {
    pub name: String,
    pub present: bool,
    pub up: bool,
    pub configured: bool,
    pub config_path: String,
}

impl Snapshot {
    pub fn collect() -> Result<Self, Error> {
        let bb = baseboard::detect().unwrap_or(Baseboard::Unknown);
        let configured = config::list_configured()?;
        let live = list_can_devices()?;

        // Union of configured + present interfaces.
        let mut names: Vec<String> = configured.iter().chain(live.iter()).cloned().collect();
        names.sort();
        names.dedup();

        let mut interfaces = Vec::new();
        for name in names {
            let present = live.contains(&name);
            let up = if present { is_up(&name)? } else { false };
            let is_cfg = configured.contains(&name);
            interfaces.push(IfaceSummary {
                config_path: config::config_path(&name).display().to_string(),
                name,
                present,
                up,
                configured: is_cfg,
            });
        }

        Ok(Snapshot {
            schema_version: SCHEMA_VERSION,
            baseboard: bb.as_str().to_string(),
            interfaces,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IfaceInfo {
    pub schema_version: u32,
    pub name: String,
    pub present: bool,
    pub config: Option<CanConfig>,
    pub config_path: String,
    pub live: Option<LiveState>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LiveState {
    pub up: bool,
    pub state: Option<String>,
    pub mtu: Option<u32>,
}

impl IfaceInfo {
    pub fn collect(iface: &str) -> Result<Self, Error> {
        let cfg_path = config::config_path(iface);
        let config = config::load(iface).ok();
        let present = Path::new(&format!("{NET_SYSFS}/{iface}")).exists();
        let live = if present {
            Some(LiveState {
                up: is_up(iface)?,
                state: read_sysfs_str(iface, "operstate"),
                mtu: read_sysfs_uint(iface, "mtu"),
            })
        } else {
            None
        };
        Ok(IfaceInfo {
            schema_version: SCHEMA_VERSION,
            name: iface.to_string(),
            present,
            config,
            config_path: cfg_path.display().to_string(),
            live,
        })
    }
}

pub fn print_list(snap: &Snapshot, json: bool) -> Result<(), Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(snap)?);
        return Ok(());
    }
    println!("Baseboard: {}\n", snap.baseboard);
    if snap.interfaces.is_empty() {
        println!("(no CAN interfaces present or configured)");
        return Ok(());
    }
    println!("{:<8}  {:<7}  {:<7}  {:<10}  {}", "NAME", "PRESENT", "UP", "CONFIGURED", "CONFIG");
    for s in &snap.interfaces {
        println!(
            "{:<8}  {:<7}  {:<7}  {:<10}  {}",
            s.name,
            yn(s.present),
            yn(s.up),
            yn(s.configured),
            s.config_path,
        );
    }
    Ok(())
}

pub fn print_show(info: &IfaceInfo, json: bool) -> Result<(), Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(info)?);
        return Ok(());
    }
    println!("Interface:    {}", info.name);
    println!("Present:      {}", yn(info.present));
    if let Some(live) = &info.live {
        println!("Up:           {}", yn(live.up));
        println!("Operstate:    {}", live.state.as_deref().unwrap_or("?"));
        if let Some(mtu) = live.mtu {
            println!("MTU:          {mtu}");
        }
    }
    println!("Config path:  {}", info.config_path);
    if let Some(cfg) = &info.config {
        println!();
        println!("Config:");
        println!("  bitrate:          {}", cfg.bitrate);
        if let Some(dbr) = cfg.data_bitrate {
            println!("  data-bitrate:     {dbr}");
        }
        println!("  fd:               {}", on_off(cfg.fd));
        println!("  triple-sampling:  {}", on_off(cfg.triple_sampling));
        println!("  restart-ms:       {}", cfg.restart_ms);
        println!("  txqueuelen:       {}", cfg.txqueuelen);
        if let Some(sp) = &cfg.sample_point {
            println!("  sample-point:     {sp}");
        }
        println!("  loopback:         {}", on_off(cfg.loopback));
        println!("  listen-only:      {}", on_off(cfg.listen_only));
    } else {
        println!("Config:       (no config file present)");
    }
    Ok(())
}

// --- helpers ---

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}
fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn list_can_devices() -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    let dir = Path::new(NET_SYSFS);
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let s = name.to_string_lossy();
        // Match canN where N is decimal.
        if s.starts_with("can") && s[3..].chars().all(|c| c.is_ascii_digit()) && s.len() > 3 {
            out.push(s.to_string());
        }
    }
    out.sort();
    Ok(out)
}

fn is_up(iface: &str) -> Result<bool, Error> {
    let flags_path = format!("{NET_SYSFS}/{iface}/flags");
    let s = match fs::read_to_string(&flags_path) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };
    // /sys/class/net/<iface>/flags is a hex value; bit 0 = IFF_UP.
    let s = s.trim();
    let s = s.strip_prefix("0x").unwrap_or(s);
    let v = u32::from_str_radix(s, 16).unwrap_or(0);
    Ok(v & 0x1 != 0)
}

fn read_sysfs_str(iface: &str, file: &str) -> Option<String> {
    fs::read_to_string(format!("{NET_SYSFS}/{iface}/{file}"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_sysfs_uint(iface: &str, file: &str) -> Option<u32> {
    read_sysfs_str(iface, file).and_then(|s| s.parse().ok())
}
