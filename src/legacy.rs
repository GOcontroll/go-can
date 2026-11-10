//! Migration of the legacy ifupdown CAN configuration to go-can.
//!
//! Images from before go-can (up to 2026-04-26) set up the CAN buses with
//! ifupdown, from /etc/network/interfaces.d/can.conf, one stanza per bus:
//!
//! ```text
//! auto can0
//! iface can0 inet manual
//! pre-up /sbin/ip link set $IFACE type can bitrate 250000 triple-sampling on loopback off restart-ms 100 #can0
//! pre-up /sbin/ip link set $IFACE txqueuelen 20
//! ```
//!
//! Installing go-can next to that file leaves two mechanisms that bring up the
//! same bus at boot (ifupdown and can@.service), and which settings a bus ends
//! up with is then unpredictable. `go-can migrate` takes the settings of every
//! bus over into /etc/gocontroll/can.d/ and moves can.conf out of
//! interfaces.d, so that from the next boot go-can alone sets up the buses.
//!
//! All or nothing: the file is only migrated when every line in it is
//! understood. Another interface, an ifupdown option or an `ip` parameter that
//! go-can cannot express leaves both can.conf and can.d untouched.
//!
//! The running buses are never touched. The migrated settings are the ones the
//! buses run with now, and they take effect at the next boot.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{self, CanConfig};
use crate::error::Error;

/// The file the legacy images wrote.
pub const LEGACY_PATH: &str = "/etc/network/interfaces.d/can.conf";
/// Where can.conf goes after a migration: outside interfaces.d, which
/// ifupdown sources with a wildcard, so a renamed copy there would still run.
pub const BACKUP_DIR: &str = "/etc/gocontroll";
const BACKUP_NAME: &str = "can.conf.migrated";

/// One bus as can.conf set it up.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyIface {
    pub name: String,
    /// Brought up at boot by ifupdown (`auto` or `allow-hotplug`).
    pub at_boot: bool,
    /// The settings, with the kernel defaults for what can.conf leaves out.
    pub cfg: CanConfig,
    /// can.conf sets no txqueuelen; `cfg` carries go-can's default.
    pub txqueuelen_default: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Write the settings to can.d.
    Migrate,
    /// can.d already has settings for this bus; go-can keeps them.
    KeepExisting { existing: CanConfig },
    /// ifupdown did not bring this bus up at boot; nothing is written, so
    /// go-can does not bring it up either.
    NotAtBoot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanItem {
    pub iface: LegacyIface,
    pub action: Action,
}

impl PlanItem {
    pub fn action_name(&self) -> &'static str {
        match self.action {
            Action::Migrate => "migrate",
            Action::KeepExisting { .. } => "keep_existing",
            Action::NotAtBoot => "not_at_boot",
        }
    }
}

/// Parse can.conf. `Err` carries the reason the file cannot be migrated.
pub fn parse(text: &str) -> Result<Vec<LegacyIface>, String> {
    let mut at_boot: Vec<String> = Vec::new();
    let mut stanzas: Vec<(String, Builder)> = Vec::new();
    let mut current: Option<usize> = None;

    for (n, line) in logical_lines(text) {
        let keyword = line.split_whitespace().next().unwrap_or_default();
        let rest = line[keyword.len()..].trim();
        match keyword {
            "auto" | "allow-auto" | "allow-hotplug" => {
                current = None;
                for name in rest.split_whitespace() {
                    check_can_name(name, n)?;
                    at_boot.push(name.to_string());
                }
            }
            "iface" => {
                let tok: Vec<&str> = rest.split_whitespace().collect();
                if tok.len() != 3 || tok[1] != "inet" || tok[2] != "manual" {
                    return Err(format!(
                        "line {n}: only 'iface <bus> inet manual' is supported, found 'iface {rest}'"
                    ));
                }
                check_can_name(tok[0], n)?;
                if stanzas.iter().any(|(name, _)| name == tok[0]) {
                    return Err(format!("line {n}: {} is defined twice", tok[0]));
                }
                stanzas.push((tok[0].to_string(), Builder::default()));
                current = Some(stanzas.len() - 1);
            }
            "pre-up" | "up" | "post-up" | "down" | "pre-down" | "post-down" => {
                let idx = current
                    .ok_or_else(|| format!("line {n}: '{keyword}' outside an iface stanza"))?;
                let (name, builder) = &mut stanzas[idx];
                parse_command(keyword, rest, name, builder, n)?;
            }
            other => return Err(format!("line {n}: '{other}' is not supported")),
        }
    }

    stanzas
        .into_iter()
        .map(|(name, b)| {
            let txqueuelen_default = b.txqueuelen.is_none();
            let cfg = b.build(&name)?;
            Ok(LegacyIface {
                at_boot: at_boot.contains(&name),
                name,
                cfg,
                txqueuelen_default,
            })
        })
        .collect()
}

/// Decide per bus what the migration does, against the settings in `config_dir`.
pub fn plan(ifaces: Vec<LegacyIface>, config_dir: &Path) -> Result<Vec<PlanItem>, Error> {
    let mut items = Vec::new();
    for iface in ifaces {
        let existing = config_dir.join(format!("{}.conf", iface.name));
        let action = if !iface.at_boot {
            Action::NotAtBoot
        } else if existing.exists() {
            let text = fs::read_to_string(&existing)?;
            Action::KeepExisting {
                existing: CanConfig::from_conf_string(&text)?,
            }
        } else {
            Action::Migrate
        };
        items.push(PlanItem { iface, action });
    }
    Ok(items)
}

/// Write the settings of every bus to migrate, then move `legacy` into
/// `backup_dir`. Returns where can.conf went. When the move fails, the
/// settings written here are removed again, so nothing has changed.
pub fn execute(
    items: &[PlanItem],
    legacy: &Path,
    config_dir: &Path,
    backup_dir: &Path,
) -> Result<PathBuf, Error> {
    let mut written: Vec<PathBuf> = Vec::new();
    for item in items.iter().filter(|i| i.action == Action::Migrate) {
        if let Err(e) = config::save_in(config_dir, &item.iface.name, &item.iface.cfg) {
            remove_all(&written);
            return Err(e);
        }
        written.push(config_dir.join(format!("{}.conf", item.iface.name)));
    }

    let moved = fs::create_dir_all(backup_dir)
        .map_err(Error::from)
        .and_then(|_| {
            let target = free_backup_path(backup_dir);
            move_file(legacy, &target).map(|_| target)
        });
    match moved {
        Ok(target) => Ok(target),
        Err(e) => {
            remove_all(&written);
            Err(e)
        }
    }
}

// --- parsing helpers ---

#[derive(Debug, Default)]
struct Builder {
    bitrate: Option<u32>,
    data_bitrate: Option<u32>,
    sample_point: Option<String>,
    restart_ms: u32,
    triple_sampling: bool,
    loopback: bool,
    listen_only: bool,
    fd: bool,
    txqueuelen: Option<u32>,
}

impl Builder {
    fn build(self, name: &str) -> Result<CanConfig, String> {
        let bitrate = self
            .bitrate
            .ok_or_else(|| format!("{name} has no bitrate"))?;
        if self.fd && self.data_bitrate.is_none() {
            return Err(format!("{name} has fd on without a dbitrate"));
        }
        Ok(CanConfig {
            bitrate,
            data_bitrate: self.data_bitrate,
            fd: self.fd,
            restart_ms: self.restart_ms,
            txqueuelen: self
                .txqueuelen
                .unwrap_or_else(|| CanConfig::default().txqueuelen),
            sample_point: self.sample_point,
            triple_sampling: self.triple_sampling,
            loopback: self.loopback,
            listen_only: self.listen_only,
        })
    }
}

/// Lines with their 1-based line number: continuations (`\`) joined, comment
/// lines and blank lines dropped.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start = 0;
    for (i, raw) in text.lines().enumerate() {
        if buf.is_empty() {
            start = i + 1;
        }
        let l = raw.trim_end();
        if let Some(cont) = l.strip_suffix('\\') {
            buf.push_str(cont);
            buf.push(' ');
            continue;
        }
        buf.push_str(l);
        let line = std::mem::take(&mut buf);
        let t = line.trim();
        if !t.is_empty() && !t.starts_with('#') {
            out.push((start, t.to_string()));
        }
    }
    let t = buf.trim();
    if !t.is_empty() && !t.starts_with('#') {
        out.push((start, t.to_string()));
    }
    out
}

fn check_can_name(name: &str, n: usize) -> Result<(), String> {
    let ok = name
        .strip_prefix("can")
        .map(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(format!("line {n}: '{name}' is not a CAN bus"))
    }
}

/// One `pre-up`/`up`/`down`/… command. ifupdown hands it to the shell, so a
/// `#` after whitespace starts a comment, as in the GOcontroll file (`#can0`).
fn parse_command(
    keyword: &str,
    cmd: &str,
    name: &str,
    b: &mut Builder,
    n: usize,
) -> Result<(), String> {
    let cmd = strip_shell_comment(cmd);
    let tok: Vec<&str> = cmd.split_whitespace().collect();
    let unsupported = || format!("line {n}: '{keyword} {cmd}' is not supported");
    let is_self = |t: &str| t == "$IFACE" || t == "${IFACE}" || t == name;
    let program = tok.first().map(|p| p.rsplit('/').next().unwrap_or(p));

    match program {
        Some("ip") => {
            if tok.len() < 5 || tok[1] != "link" || tok[2] != "set" || !is_self(tok[3]) {
                return Err(unsupported());
            }
            let rest = &tok[4..];
            match (keyword, rest) {
                ("up" | "post-up", ["up"]) => Ok(()),
                ("down" | "pre-down" | "post-down", ["down"]) => Ok(()),
                ("pre-up", ["txqueuelen", v]) => {
                    b.txqueuelen = Some(parse_u32(v, "txqueuelen", n)?);
                    Ok(())
                }
                ("pre-up", ["type", "can", params @ ..]) => parse_can_params(params, b, n),
                _ => Err(unsupported()),
            }
        }
        Some("ifconfig") => match (keyword, tok.as_slice()) {
            ("up" | "post-up", [_, i, "up"]) if is_self(i) => Ok(()),
            ("down" | "pre-down" | "post-down", [_, i, "down"]) if is_self(i) => Ok(()),
            _ => Err(unsupported()),
        },
        _ => Err(unsupported()),
    }
}

/// The parameters after `ip link set … type can`. Applied in order, as `ip`
/// does; what is not given keeps its kernel default.
fn parse_can_params(params: &[&str], b: &mut Builder, n: usize) -> Result<(), String> {
    if params.len() % 2 != 0 {
        return Err(format!("line {n}: 'type can' parameters do not come in pairs"));
    }
    for pair in params.chunks(2) {
        let (key, value) = (pair[0], pair[1]);
        match key {
            "bitrate" => b.bitrate = Some(parse_u32(value, key, n)?),
            "dbitrate" => b.data_bitrate = Some(parse_u32(value, key, n)?),
            "restart-ms" => b.restart_ms = parse_u32(value, key, n)?,
            "sample-point" => {
                value
                    .parse::<f64>()
                    .map_err(|_| format!("line {n}: sample-point '{value}' is not a number"))?;
                b.sample_point = Some(value.to_string());
            }
            "triple-sampling" => b.triple_sampling = parse_on_off(value, key, n)?,
            "loopback" => b.loopback = parse_on_off(value, key, n)?,
            "listen-only" => b.listen_only = parse_on_off(value, key, n)?,
            "fd" => b.fd = parse_on_off(value, key, n)?,
            other => {
                return Err(format!(
                    "line {n}: the CAN parameter '{other}' cannot be set with go-can"
                ))
            }
        }
    }
    Ok(())
}

fn strip_shell_comment(cmd: &str) -> &str {
    let bytes = cmd.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return cmd[..i].trim_end();
        }
    }
    cmd
}

fn parse_u32(v: &str, key: &str, n: usize) -> Result<u32, String> {
    v.parse::<u32>()
        .map_err(|_| format!("line {n}: {key} '{v}' is not an unsigned integer"))
}

fn parse_on_off(v: &str, key: &str, n: usize) -> Result<bool, String> {
    match v {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(format!("line {n}: {key} '{v}' is not on or off")),
    }
}

// --- file helpers ---

fn free_backup_path(dir: &Path) -> PathBuf {
    let first = dir.join(BACKUP_NAME);
    if !first.exists() {
        return first;
    }
    (1..)
        .map(|i| dir.join(format!("{BACKUP_NAME}.{i}")))
        .find(|p| !p.exists())
        .expect("unbounded range")
}

/// rename, or copy + remove when the two paths are on different file systems.
fn move_file(from: &Path, to: &Path) -> Result<(), Error> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to)?;
    if let Err(e) = fs::remove_file(from) {
        let _ = fs::remove_file(to);
        return Err(e.into());
    }
    Ok(())
}

fn remove_all(paths: &[PathBuf]) {
    for p in paths {
        let _ = fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// can.conf as SetupRootfs wrote it for a Moduline IV (L4), 2025-04 to 2026-04-26.
    const L4: &str = "auto can0
iface can0 inet manual
pre-up /sbin/ip link set $IFACE type can bitrate 250000 triple-sampling on loopback off restart-ms 100 #can0
pre-up /sbin/ip link set $IFACE txqueuelen 20

auto can1
iface can1 inet manual
pre-up /sbin/ip link set $IFACE type can bitrate 250000 triple-sampling on loopback off restart-ms 100 #can1
pre-up /sbin/ip link set $IFACE txqueuelen 20

auto can2
iface can2 inet manual
pre-up /sbin/ip link set $IFACE type can bitrate 250000 triple-sampling on loopback off restart-ms 100 #can2
pre-up /sbin/ip link set $IFACE txqueuelen 20

auto can3
iface can3 inet manual
pre-up /sbin/ip link set $IFACE type can bitrate 250000 triple-sampling on loopback off restart-ms 100 #can3
pre-up /sbin/ip link set $IFACE txqueuelen 20
";

    /// The Display (HMI1) variant of postinstall.sh, with its FD line switched on for can1.
    const DISPLAY_FD: &str = "auto can0
iface can0 inet manual
pre-up /sbin/ip link set $IFACE type can bitrate 250000 triple-sampling on loopback off restart-ms 100 #can0
#pre-up /sbin/ip link set $IFACE type can bitrate 250000 loopback off restart-ms 100 fd on dbitrate 1000000 #can0 with fd
pre-up /sbin/ip link set $IFACE txqueuelen 20

auto can1
iface can1 inet manual
#pre-up /sbin/ip link set $IFACE type can bitrate 250000 triple-sampling on loopback off restart-ms 100 #can1
pre-up /sbin/ip link set $IFACE type can bitrate 250000 loopback off restart-ms 100 fd on dbitrate 1000000 #can1 with fd
pre-up /sbin/ip link set $IFACE txqueuelen 20
";

    fn scratch(tag: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir().join(format!(
            "go-can-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn gocontroll_file_equals_go_can_defaults() {
        let ifaces = parse(L4).unwrap();
        assert_eq!(ifaces.len(), 4);
        for (i, f) in ifaces.iter().enumerate() {
            assert_eq!(f.name, format!("can{i}"));
            assert!(f.at_boot);
            assert!(!f.txqueuelen_default);
            assert_eq!(f.cfg, CanConfig::default());
        }
    }

    #[test]
    fn edited_bitrate_is_taken_over() {
        let text = L4.replacen(
            "type can bitrate 250000 triple-sampling on loopback off restart-ms 100 #can1",
            "type can bitrate 500000 triple-sampling on loopback off restart-ms 100 #can1",
            1,
        );
        let ifaces = parse(&text).unwrap();
        assert_eq!(ifaces[1].cfg.bitrate, 500_000);
        assert_eq!(ifaces[0].cfg.bitrate, 250_000);
    }

    #[test]
    fn fd_line_without_triple_sampling() {
        let ifaces = parse(DISPLAY_FD).unwrap();
        assert_eq!(ifaces[0].cfg, CanConfig::default());
        let c1 = &ifaces[1].cfg;
        assert!(c1.fd);
        assert_eq!(c1.data_bitrate, Some(1_000_000));
        assert!(!c1.triple_sampling, "absent triple-sampling is the kernel default, off");
        assert_eq!(c1.restart_ms, 100);
        assert_eq!(c1.txqueuelen, 20);
    }

    #[test]
    fn absent_parameters_take_kernel_defaults() {
        let ifaces =
            parse("auto can0\niface can0 inet manual\npre-up ip link set can0 type can bitrate 125000\n")
                .unwrap();
        let c = &ifaces[0].cfg;
        assert_eq!(c.bitrate, 125_000);
        assert_eq!(c.restart_ms, 0);
        assert!(!c.triple_sampling && !c.loopback && !c.listen_only && !c.fd);
        assert!(ifaces[0].txqueuelen_default);
        assert_eq!(c.txqueuelen, CanConfig::default().txqueuelen);
    }

    #[test]
    fn continuation_and_up_down_lines() {
        let text = "allow-hotplug can0\niface can0 inet manual\n\
                    pre-up /usr/sbin/ip link set ${IFACE} type can \\\n  bitrate 1000000 restart-ms 50\n\
                    up /sbin/ifconfig $IFACE up\ndown ip link set $IFACE down\n";
        let ifaces = parse(text).unwrap();
        assert!(ifaces[0].at_boot);
        assert_eq!(ifaces[0].cfg.bitrate, 1_000_000);
        assert_eq!(ifaces[0].cfg.restart_ms, 50);
    }

    #[test]
    fn rejects_what_go_can_cannot_express() {
        let base = "auto can0\niface can0 inet manual\n";
        for line in [
            "pre-up ip link set $IFACE type can bitrate 250000 berr-reporting on",
            "pre-up ip link set $IFACE type can bitrate 250000 one-shot on",
            "pre-up ip link set $IFACE type can tq 125 prop-seg 6 phase-seg1 7 phase-seg2 2",
            "pre-up ip link set $IFACE type can bitrate 250000 triple-sampling",
            "pre-up ip link set can1 type can bitrate 250000",
            "pre-up /usr/local/bin/my-can-setup",
            "address 10.0.0.1",
            "pre-up ip link set $IFACE type can bitrate 250000 fd on",
        ] {
            let text = format!("{base}{line}\n");
            assert!(parse(&text).is_err(), "should reject: {line}");
        }
        assert!(parse("auto eth0\niface eth0 inet dhcp\n").is_err());
        assert!(parse("iface can0 inet static\n").is_err());
        assert!(parse("auto can0\niface can0 inet manual\n").is_err(), "no bitrate");
        assert!(parse(&format!("{L4}\niface can0 inet manual\n")).is_err(), "defined twice");
    }

    #[test]
    fn bus_without_auto_is_not_brought_up() {
        let text = L4.replacen("auto can3\n", "#auto can3\n", 1);
        let items = plan(parse(&text).unwrap(), &scratch("noauto")).unwrap();
        assert_eq!(items[3].action, Action::NotAtBoot);
        assert_eq!(items[0].action, Action::Migrate);
    }

    #[test]
    fn existing_go_can_settings_are_kept() {
        let dir = scratch("keep");
        let mut mine = CanConfig::default();
        mine.bitrate = 500_000;
        config::save_in(&dir, "can0", &mine).unwrap();
        let items = plan(parse(L4).unwrap(), &dir).unwrap();
        assert_eq!(items[0].action, Action::KeepExisting { existing: mine });
        assert_eq!(items[1].action, Action::Migrate);
    }

    #[test]
    fn execute_writes_settings_and_moves_the_file() {
        let root = scratch("exec");
        let legacy = root.join("interfaces.d").join("can.conf");
        let can_d = root.join("can.d");
        let backup_dir = root.join("gocontroll");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, DISPLAY_FD).unwrap();

        let items = plan(parse(DISPLAY_FD).unwrap(), &can_d).unwrap();
        let moved = execute(&items, &legacy, &can_d, &backup_dir).unwrap();

        assert!(!legacy.exists());
        assert_eq!(moved, backup_dir.join("can.conf.migrated"));
        assert_eq!(fs::read_to_string(&moved).unwrap(), DISPLAY_FD);
        let c1 = CanConfig::from_conf_string(&fs::read_to_string(can_d.join("can1.conf")).unwrap())
            .unwrap();
        assert_eq!(c1, items[1].iface.cfg);

        // A second can.conf later does not overwrite the first backup.
        fs::write(&legacy, L4).unwrap();
        let items = plan(parse(L4).unwrap(), &can_d).unwrap();
        let moved = execute(&items, &legacy, &can_d, &backup_dir).unwrap();
        assert_eq!(moved, backup_dir.join("can.conf.migrated.1"));
    }

    #[test]
    fn failed_move_removes_what_was_written() {
        let root = scratch("fail");
        let can_d = root.join("can.d");
        let missing = root.join("interfaces.d").join("can.conf");
        let items = plan(parse(L4).unwrap(), &can_d).unwrap();
        assert!(execute(&items, &missing, &can_d, &root.join("gocontroll")).is_err());
        for i in 0..4 {
            assert!(!can_d.join(format!("can{i}.conf")).exists());
        }
    }
}
