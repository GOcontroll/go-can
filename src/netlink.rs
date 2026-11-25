//! CAN-link state mutation.
//!
//! v0.1: shells out to `ip link` for actual netlink calls. CAN-specific
//! IFLA_INFO_DATA encoding via raw rtnetlink is non-trivial; using `ip` from
//! iproute2 (already a runtime dep on Debian) is pragmatic and well-tested.
//! v0.2 may switch to native rtnetlink if the cold-start cost matters.

use std::process::Command;

use serde_json::Value;

use crate::config::{CanConfig, DEFAULT_DATA_BITRATE};
use crate::error::Error;

/// Limits of one bit timing phase, as the driver reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitTimingConst {
    pub tseg1_min: u32,
    pub tseg1_max: u32,
    pub tseg2_min: u32,
    pub tseg2_max: u32,
    pub brp_min: u32,
    pub brp_max: u32,
    pub brp_inc: u32,
}

impl BitTimingConst {
    fn from_json(v: &Value) -> Option<Self> {
        let num = |a: &str, b: &str| v.get(a)?.get(b)?.as_u64().map(|n| n as u32);
        Some(Self {
            tseg1_min: num("tseg1", "min")?,
            tseg1_max: num("tseg1", "max")?,
            tseg2_min: num("tseg2", "min")?,
            tseg2_max: num("tseg2", "max")?,
            brp_min: num("brp", "min")?,
            brp_max: num("brp", "max")?,
            brp_inc: v.get("brp_inc").and_then(Value::as_u64).unwrap_or(1) as u32,
        })
    }

    /// Whether `bitrate` can be made exactly from `clock`.
    ///
    /// The kernel settles for the nearest rate it can make, up to 5 % off. A
    /// classic bus forgives that; the data phase of CAN FD does not, so a
    /// picker should only offer what the controller can really do. The clock
    /// of a Multibus module is 170 MHz, which rules out 4 and 8 Mbit/s.
    pub fn is_exact(&self, clock: u32, bitrate: u32) -> bool {
        if clock == 0 || bitrate == 0 {
            return false;
        }
        let (clock, bitrate) = (clock as u64, bitrate as u64);
        let lowest = 1 + (self.tseg1_min + self.tseg2_min) as u64;
        let highest = 1 + (self.tseg1_max + self.tseg2_max) as u64;
        let mut brp = self.brp_min.max(1) as u64;
        while brp <= self.brp_max as u64 {
            let divider = brp * bitrate;
            if clock % divider == 0 && (lowest..=highest).contains(&(clock / divider)) {
                return true;
            }
            brp += self.brp_inc.max(1) as u64;
        }
        false
    }
}

/// What a CAN interface can do, read from `ip -details -json link show`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkCaps {
    /// The controller does CAN FD and its driver says so.
    pub fd: bool,
    /// The interface is in CAN FD right now.
    pub fd_active: bool,
    /// The data bitrate it runs at, when it is in CAN FD.
    pub data_bitrate: Option<u32>,
    pub clock: Option<u32>,
    pub data_const: Option<BitTimingConst>,
}

impl LinkCaps {
    pub fn parse(json: &str) -> Option<Self> {
        let doc: Value = serde_json::from_str(json).ok()?;
        let data = doc.get(0)?.get("linkinfo")?.get("info_data")?;
        let has = |key: &str, flag: &str| {
            data.get(key)
                .and_then(Value::as_array)
                .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(flag)))
        };
        let data_const = data
            .get("data_bittiming_const")
            .and_then(BitTimingConst::from_json);
        Some(Self {
            // A driver with fixed bit timing lists no constants; the supported
            // control modes are the direct answer where the kernel prints them.
            fd: has("ctrlmode_supported", "FD") || data_const.is_some(),
            fd_active: has("ctrlmode", "FD"),
            data_bitrate: data
                .get("data_bittiming")
                .and_then(|timing| timing.get("bitrate"))
                .and_then(Value::as_u64)
                .map(|n| n as u32),
            clock: data.get("clock").and_then(Value::as_u64).map(|n| n as u32),
            data_const,
        })
    }

    /// Whether the controller can make this data bitrate exactly. True when
    /// the driver gives nothing to work it out from.
    pub fn data_bitrate_exact(&self, bitrate: u32) -> bool {
        match (self.clock, &self.data_const) {
            (Some(clock), Some(limits)) => limits.is_exact(clock, bitrate),
            _ => true,
        }
    }
}

/// Capabilities of a live interface. `None` when it is not there, is not a CAN
/// interface, or `ip` cannot be asked.
pub fn link_caps(iface: &str) -> Option<LinkCaps> {
    let args = ["-details", "-json", "link", "show", iface];
    let out = Command::new("/usr/sbin/ip")
        .args(args)
        .output()
        .or_else(|_| Command::new("ip").args(args).output())
        .ok()?;
    if !out.status.success() {
        return None;
    }
    LinkCaps::parse(&String::from_utf8_lossy(&out.stdout))
}

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
        // The kernel refuses 'fd on' without a data bitrate.
        args.push("dbitrate".into());
        args.push(cfg.data_bitrate.unwrap_or(DEFAULT_DATA_BITRATE).to_string());
    } else if link_caps(iface).is_some_and(|caps| caps.fd) {
        // An interface keeps CAN FD over a down and up, so leaving the word
        // out would leave a bus that was switched to FD before in FD. Only
        // said to a controller that does FD: a classic-only one has nothing
        // to switch off and may refuse the word.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A Multibus module bus (gs_usb, firmware 0.3.0) as kernel 6.12 prints it.
    const MODULE_BUS: &str = r#"[{"ifname":"mb_can3","linkinfo":{"info_kind":"can","info_data":{
        "ctrlmode":["FD"],
        "data_bittiming":{"bitrate":5000000,"sample_point":"0.735","tq":5,"brp":1},
        "ctrlmode_supported":["LOOPBACK","LISTEN-ONLY","ONE-SHOT","FD","CC-LEN8-DLC"],
        "bittiming_const":{"name":"gs_usb","tseg1":{"min":2,"max":256},"tseg2":{"min":1,"max":128},
            "sjw":{"min":1,"max":128},"brp":{"min":1,"max":512},"brp_inc":1},
        "data_bittiming_const":{"name":"gs_usb","tseg1":{"min":1,"max":32},"tseg2":{"min":1,"max":16},
            "sjw":{"min":1,"max":16},"brp":{"min":1,"max":32},"brp_inc":1},
        "clock":170000000}}}]"#;

    /// A classic-only baseboard bus.
    const CLASSIC_BUS: &str = r#"[{"ifname":"can0","linkinfo":{"info_kind":"can","info_data":{
        "ctrlmode":["TRIPLE-SAMPLING"],
        "ctrlmode_supported":["LOOPBACK","LISTEN-ONLY","TRIPLE-SAMPLING","ONE-SHOT"],
        "bittiming_const":{"name":"mcp251x","tseg1":{"min":3,"max":16},"tseg2":{"min":2,"max":8},
            "sjw":{"min":1,"max":4},"brp":{"min":1,"max":64},"brp_inc":1},
        "clock":8000000}}}]"#;

    #[test]
    fn a_module_bus_offers_the_data_bitrates_its_170_mhz_clock_can_make() {
        let caps = LinkCaps::parse(MODULE_BUS).unwrap();
        assert!(caps.fd && caps.fd_active);
        assert_eq!(caps.data_bitrate, Some(5_000_000));
        for exact in [500_000, 1_000_000, 2_000_000, 5_000_000] {
            assert!(caps.data_bitrate_exact(exact), "{exact}");
        }
        // 170 MHz / 4 Mbit/s is 42.5 time quanta, / 8 Mbit/s is 21.25.
        for off in [4_000_000, 8_000_000] {
            assert!(!caps.data_bitrate_exact(off), "{off}");
        }
    }

    #[test]
    fn a_classic_controller_has_no_fd() {
        let caps = LinkCaps::parse(CLASSIC_BUS).unwrap();
        assert!(!caps.fd && !caps.fd_active);
        assert_eq!(caps.data_const, None);
    }

    #[test]
    fn a_40_mhz_fd_controller_makes_every_usual_data_bitrate() {
        let limits = BitTimingConst {
            tseg1_min: 1, tseg1_max: 32, tseg2_min: 1, tseg2_max: 16,
            brp_min: 1, brp_max: 256, brp_inc: 1,
        };
        for rate in [500_000, 1_000_000, 2_000_000, 4_000_000, 5_000_000, 8_000_000] {
            assert!(limits.is_exact(40_000_000, rate), "{rate}");
        }
    }

    #[test]
    fn something_that_is_not_a_can_link_has_no_capabilities() {
        assert_eq!(LinkCaps::parse(r#"[{"ifname":"eth0"}]"#), None);
        assert_eq!(LinkCaps::parse("nonsense"), None);
    }
}
