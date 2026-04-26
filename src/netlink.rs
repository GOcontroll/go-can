//! CAN-link state mutation.
//!
//! v0.1: shells out to `ip link` for actual netlink calls. CAN-specific
//! IFLA_INFO_DATA encoding via raw rtnetlink is non-trivial; using `ip` from
//! iproute2 (already a runtime dep on Debian) is pragmatic and well-tested.
//! v0.2 may switch to native rtnetlink if the cold-start cost matters.

use std::process::Command;

use crate::config::CanConfig;
use crate::error::Error;

/// Apply config to live interface. Equivalent to:
///   ip link set <iface> down
///   ip link set <iface> type can bitrate ... [fd on dbitrate ...] [...] restart-ms ...
///   ip link set <iface> txqueuelen ...
///   ip link set <iface> up
pub fn apply(iface: &str, cfg: &CanConfig) -> Result<(), Error> {
    // 1) Take the link down (idempotent — ignore failure if already down).
    let _ = run_ip(&["link", "set", iface, "down"]);

    // 2) Configure CAN parameters.
    let mut args: Vec<String> = vec![
        "link".into(),
        "set".into(),
        iface.into(),
        "type".into(),
        "can".into(),
        "bitrate".into(),
        cfg.bitrate.to_string(),
    ];
    if let Some(sp) = &cfg.sample_point {
        args.push("sample-point".into());
        args.push(sp.clone());
    }
    args.push("restart-ms".into());
    args.push(cfg.restart_ms.to_string());
    if cfg.triple_sampling {
        args.push("triple-sampling".into());
        args.push("on".into());
    } else {
        args.push("triple-sampling".into());
        args.push("off".into());
    }
    args.push("loopback".into());
    args.push(if cfg.loopback { "on".into() } else { "off".into() });
    args.push("listen-only".into());
    args.push(if cfg.listen_only { "on".into() } else { "off".into() });
    if cfg.fd {
        args.push("fd".into());
        args.push("on".into());
        if let Some(dbr) = cfg.data_bitrate {
            args.push("dbitrate".into());
            args.push(dbr.to_string());
        }
    } else {
        args.push("fd".into());
        args.push("off".into());
    }
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_ip(&arg_refs)?;

    // 3) Tx queue length.
    run_ip(&["link", "set", iface, "txqueuelen", &cfg.txqueuelen.to_string()])?;

    // 4) Bring up.
    run_ip(&["link", "set", iface, "up"])?;

    Ok(())
}

/// Bring the interface down (used by `can@.service` ExecStop).
#[allow(dead_code)]
pub fn down(iface: &str) -> Result<(), Error> {
    run_ip(&["link", "set", iface, "down"])
}

fn run_ip(args: &[&str]) -> Result<(), Error> {
    let out = Command::new("/usr/sbin/ip")
        .args(args)
        .output()
        .or_else(|_| Command::new("ip").args(args).output())
        .map_err(|e| Error::IpLinkFailed(format!("could not exec ip: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::IpLinkFailed(format!(
            "ip {}: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(())
}
