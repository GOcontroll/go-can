//! Per-interface config persistence in /etc/gocontroll/can.d/<iface>.conf
//!
//! Format: KEY=VALUE, shell-sourceable so external scripts can read it without
//! a parser. One file per interface — modular, fleet-rsync-friendly.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;

const CONFIG_DIR: &str = "/etc/gocontroll/can.d";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanConfig {
    pub bitrate: u32,
    /// CAN-FD data-phase bitrate (only used when fd=true).
    #[serde(rename = "data_bitrate")]
    pub data_bitrate: Option<u32>,
    pub fd: bool,
    pub restart_ms: u32,
    pub txqueuelen: u32,
    /// Arbitration-phase sample point (e.g. "0.875"). Empty = kernel default.
    pub sample_point: Option<String>,
    pub triple_sampling: bool,
    pub loopback: bool,
    pub listen_only: bool,
}

impl Default for CanConfig {
    fn default() -> Self {
        Self {
            bitrate: 250_000,
            data_bitrate: None,
            fd: false,
            restart_ms: 100,
            txqueuelen: 20,
            sample_point: None,
            triple_sampling: true,
            loopback: false,
            listen_only: false,
        }
    }
}

impl CanConfig {
    /// Apply a single key=value pair from the CLI's `go-can set <iface> <key> <value>`.
    pub fn set_key(&mut self, key: &str, value: &str) -> Result<(), Error> {
        // Accept both `key-with-dashes` and `key_with_underscores`.
        let k = key.replace('-', "_");
        match k.as_str() {
            "bitrate" => self.bitrate = parse_uint(value, "bitrate")?,
            "data_bitrate" => {
                self.data_bitrate = if value.is_empty() {
                    None
                } else {
                    Some(parse_uint(value, "data-bitrate")?)
                };
            }
            "fd" => self.fd = parse_bool(value, "fd")?,
            "restart_ms" => self.restart_ms = parse_uint(value, "restart-ms")?,
            "txqueuelen" => self.txqueuelen = parse_uint(value, "txqueuelen")?,
            "sample_point" => {
                self.sample_point = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            "triple_sampling" => self.triple_sampling = parse_bool(value, "triple-sampling")?,
            "loopback" => self.loopback = parse_bool(value, "loopback")?,
            "listen_only" => self.listen_only = parse_bool(value, "listen-only")?,
            _ => {
                return Err(Error::UserError(format!(
                    "unknown key '{key}' (valid: bitrate, data-bitrate, fd, restart-ms, \
                     txqueuelen, sample-point, triple-sampling, loopback, listen-only)"
                )));
            }
        }
        Ok(())
    }

    /// Render to KEY=VALUE shell-sourceable form (with header comment).
    pub fn to_conf_string(&self, iface: &str) -> String {
        let yes_no = |b: bool| if b { "on" } else { "off" };
        let opt_str = |o: &Option<String>| o.as_deref().unwrap_or("").to_string();
        let opt_uint = |o: Option<u32>| o.map(|v| v.to_string()).unwrap_or_default();

        format!(
            "# /etc/gocontroll/can.d/{iface}.conf — managed by go-can\n\
             BITRATE={bitrate}\n\
             TRIPLE_SAMPLING={triple}\n\
             RESTART_MS={restart}\n\
             TXQUEUELEN={txq}\n\
             LOOPBACK={lb}\n\
             LISTEN_ONLY={lo}\n\
             FD={fd}\n\
             DATA_BITRATE={dbr}\n\
             SAMPLE_POINT={sp}\n",
            iface = iface,
            bitrate = self.bitrate,
            triple = yes_no(self.triple_sampling),
            restart = self.restart_ms,
            txq = self.txqueuelen,
            lb = yes_no(self.loopback),
            lo = yes_no(self.listen_only),
            fd = yes_no(self.fd),
            dbr = opt_uint(self.data_bitrate),
            sp = opt_str(&self.sample_point),
        )
    }

    pub fn from_conf_string(s: &str) -> Result<Self, Error> {
        let mut cfg = Self::default();
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| Error::ParseError(format!("malformed line: {line}")))?;
            let v = v.trim_matches(|c: char| c == '"' || c == '\'');
            // Map back to dashed-keys for set_key().
            let key = match k.trim() {
                "BITRATE" => "bitrate",
                "DATA_BITRATE" => "data-bitrate",
                "FD" => "fd",
                "RESTART_MS" => "restart-ms",
                "TXQUEUELEN" => "txqueuelen",
                "SAMPLE_POINT" => "sample-point",
                "TRIPLE_SAMPLING" => "triple-sampling",
                "LOOPBACK" => "loopback",
                "LISTEN_ONLY" => "listen-only",
                other => {
                    // Unknown keys are silently ignored — forwards-compat.
                    eprintln!("[go-can] warn: ignoring unknown config key {other}");
                    continue;
                }
            };
            cfg.set_key(key, v)?;
        }
        Ok(cfg)
    }
}

pub fn config_path(iface: &str) -> PathBuf {
    Path::new(CONFIG_DIR).join(format!("{iface}.conf"))
}

pub fn load(iface: &str) -> Result<CanConfig, Error> {
    let p = config_path(iface);
    let s = fs::read_to_string(&p).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            Error::UserError(format!("no config for {iface} (expected {})", p.display()))
        }
        _ => Error::Io(e),
    })?;
    CanConfig::from_conf_string(&s)
}

/// Load existing config or return default if none on disk.
pub fn load_or_default(iface: &str) -> CanConfig {
    load(iface).unwrap_or_default()
}

pub fn save(iface: &str, cfg: &CanConfig) -> Result<(), Error> {
    fs::create_dir_all(CONFIG_DIR)?;
    let p = config_path(iface);
    let mut tmp = tempfile_in(CONFIG_DIR, &format!("{iface}.conf."))?;
    tmp.write_all(cfg.to_conf_string(iface).as_bytes())?;
    tmp.flush()?;
    let tmp_path = tmp.into_path();
    fs::rename(&tmp_path, &p)?;
    // Ensure mode 644.
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

/// List all interfaces with a config file in /etc/gocontroll/can.d/.
pub fn list_configured() -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    let dir = Path::new(CONFIG_DIR);
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stripped) = name.strip_suffix(".conf") {
            out.push(stripped.to_string());
        }
    }
    out.sort();
    Ok(out)
}

// --- helpers ---

fn parse_uint(s: &str, field: &str) -> Result<u32, Error> {
    s.parse::<u32>()
        .map_err(|_| Error::UserError(format!("{field}: expected unsigned integer, got '{s}'")))
}

fn parse_bool(s: &str, field: &str) -> Result<bool, Error> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => Ok(true),
        "off" | "no" | "false" | "0" => Ok(false),
        _ => Err(Error::UserError(format!(
            "{field}: expected on/off/yes/no/true/false, got '{s}'"
        ))),
    }
}

/// Minimal tempfile (no extra crate dep) — file in same dir, deletes on drop unless `into_path()` called.
struct TempFile {
    path: PathBuf,
    file: Option<fs::File>,
}

impl TempFile {
    fn into_path(mut self) -> PathBuf {
        // Take ownership of file (close it) and disable destructor's unlink.
        self.file.take();
        let p = std::mem::take(&mut self.path);
        std::mem::forget(self);
        p
    }
}

impl Write for TempFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.as_mut().expect("tempfile open").write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().expect("tempfile open").flush()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.file.is_some() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn tempfile_in(dir: &str, prefix: &str) -> Result<TempFile, Error> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = Path::new(dir).join(format!("{prefix}{nanos}.tmp"));
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    Ok(TempFile {
        path,
        file: Some(file),
    })
}
