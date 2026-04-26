//! Baseboard detection from device-tree

use std::fs;

use serde::{Deserialize, Serialize};

use crate::error::Error;

const HARDWARE_PATH: &str = "/sys/firmware/devicetree/base/hardware";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Baseboard {
    /// Moduline L4 (formerly Moduline IV) — 4× CAN
    L4,
    /// Moduline M1 (formerly Moduline Mini) — 2× CAN
    M1,
    /// Moduline HMI1 (formerly Display) — 2× CAN, FD-capable
    HMI1,
    Unknown,
}

impl Baseboard {
    pub fn as_str(&self) -> &'static str {
        match self {
            Baseboard::L4 => "L4",
            Baseboard::M1 => "M1",
            Baseboard::HMI1 => "HMI1",
            Baseboard::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Result<Self, Error> {
        match s.to_uppercase().as_str() {
            "L4" => Ok(Baseboard::L4),
            "M1" => Ok(Baseboard::M1),
            "HMI1" => Ok(Baseboard::HMI1),
            _ => Err(Error::UserError(format!(
                "unknown baseboard '{s}' (valid: L4, M1, HMI1)"
            ))),
        }
    }

    /// Number of CAN interfaces this baseboard exposes.
    pub fn can_count(&self) -> usize {
        match self {
            Baseboard::L4 => 4,
            Baseboard::M1 | Baseboard::HMI1 => 2,
            Baseboard::Unknown => 0,
        }
    }
}

/// Read /sys/firmware/devicetree/base/hardware and classify.
///
/// Matches both the new naming (L4 / M1 / HMI1) and legacy (IV / Mini / Display)
/// so this works on freshly-flashed AND existing devices that report old DTB strings.
pub fn detect() -> Result<Baseboard, Error> {
    let raw = fs::read_to_string(HARDWARE_PATH).map_err(|e| {
        Error::SystemError(format!("could not read {HARDWARE_PATH}: {e}"))
    })?;
    let raw = raw.trim_end_matches('\0').trim();
    Ok(detect_from(raw))
}

/// Pure classification helper (testable).
pub fn detect_from(raw: &str) -> Baseboard {
    let upper = raw.to_uppercase();
    // Match L4 first (longer + new name preferred over legacy IV).
    if upper.contains("L4") || upper.contains(" IV ") || upper.starts_with("MODULINE IV") {
        Baseboard::L4
    } else if upper.contains("M1") || upper.contains("MINI") {
        Baseboard::M1
    } else if upper.contains("HMI1") || upper.contains("DISPLAY") {
        Baseboard::HMI1
    } else {
        Baseboard::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_new_names() {
        assert_eq!(detect_from("Moduline L4 V3.06-D"), Baseboard::L4);
        assert_eq!(detect_from("Moduline M1 V1.11"), Baseboard::M1);
        assert_eq!(detect_from("Moduline HMI1 V1.0"), Baseboard::HMI1);
    }

    #[test]
    fn detects_legacy_names() {
        assert_eq!(detect_from("Moduline IV V3.06-D"), Baseboard::L4);
        assert_eq!(detect_from("Moduline Mini V1.11"), Baseboard::M1);
        assert_eq!(detect_from("Moduline Display V1.0"), Baseboard::HMI1);
    }

    #[test]
    fn unknown_when_unrecognized() {
        assert_eq!(detect_from("Some Other Board"), Baseboard::Unknown);
        assert_eq!(detect_from(""), Baseboard::Unknown);
    }
}
