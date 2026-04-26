//! Per-baseboard CAN defaults table

use crate::baseboard::Baseboard;
use crate::config::{self, CanConfig};
use crate::error::Error;

/// Default config for a single interface on the given baseboard.
/// Returns None if `iface` is not present on this baseboard (e.g. can3 on M1).
pub fn config_for(bb: Baseboard, iface: &str) -> Option<CanConfig> {
    let count = bb.can_count();
    let valid: Vec<String> = (0..count).map(|i| format!("can{i}")).collect();
    if !valid.iter().any(|c| c == iface) {
        return None;
    }
    Some(default_for(bb))
}

/// All interfaces this baseboard wants enabled, with their default configs.
pub fn all_for(bb: Baseboard) -> Vec<(String, CanConfig)> {
    let cfg = default_for(bb);
    (0..bb.can_count())
        .map(|i| (format!("can{i}"), cfg.clone()))
        .collect()
}

/// Write per-baseboard defaults to /etc/gocontroll/can.d/.
/// Returns names of interface configs written.
pub fn write_for(bb: Baseboard) -> Result<Vec<String>, Error> {
    if matches!(bb, Baseboard::Unknown) {
        return Err(Error::UserError(
            "baseboard unknown — cannot select defaults; pass --baseboard <L4|M1|HMI1>".into(),
        ));
    }
    let mut written = Vec::new();
    for (iface, cfg) in all_for(bb) {
        config::save(&iface, &cfg)?;
        written.push(iface);
    }
    Ok(written)
}

/// Per-baseboard CanConfig template. Currently identical for all three
/// (250 kbit/s classic CAN, triple-sampling on, restart 100ms, txq 20),
/// but separated so HMI1 can later default to FD opt-in via a per-board tweak.
fn default_for(bb: Baseboard) -> CanConfig {
    // Currently identical for L4/M1/HMI1 — 250 kbit/s classic CAN, triple-sampling on.
    // HMI1 hardware supports CAN-FD but ships classic by default for backwards-compat
    // with existing fleets. Users opt in via:
    //   go-can set can0 fd on
    //   go-can set can0 data-bitrate 1000000
    let _ = bb;
    CanConfig::default()
}
