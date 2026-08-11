//! The printer models this workspace can drive, and the per-model facts the
//! transports need. Values, not behavior: keeping UUIDs and name prefixes
//! here lets the CLI, server, and browser share one source of truth without
//! core doing any I/O.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrinterModel {
    /// LX-D02, the original reverse-engineered target (protocol/).
    LxD02,
    /// X6/X6h "cat printer" family (protocol_x6/).
    X6,
}

impl PrinterModel {
    /// Infer the model from a BLE advertised name, if it looks like a
    /// printer we support. `X6h-` matches case-insensitively on the prefix's
    /// first letter only: parzivail notes `X6H` (capital H) is a distinct
    /// model, so it is deliberately not claimed here.
    pub fn from_device_name(name: &str) -> Option<Self> {
        if name.starts_with("LX") {
            Some(Self::LxD02)
        } else if name.starts_with("X6h-") || name.starts_with("x6h-") {
            Some(Self::X6)
        } else {
            None
        }
    }

    pub fn service_uuid16(self) -> u16 {
        match self {
            Self::LxD02 => 0xFFE6,
            Self::X6 => 0xAE30,
        }
    }

    pub fn write_char_uuid16(self) -> u16 {
        match self {
            Self::LxD02 => 0xFFE1,
            Self::X6 => 0xAE01,
        }
    }

    pub fn notify_char_uuid16(self) -> u16 {
        match self {
            Self::LxD02 => 0xFFE2,
            Self::X6 => 0xAE02,
        }
    }
}

impl fmt::Display for PrinterModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad`, not `write_str`, so width specifiers work in table output.
        f.pad(match self {
            Self::LxD02 => "lx-d02",
            Self::X6 => "x6",
        })
    }
}

impl FromStr for PrinterModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "lx-d02" => Ok(Self::LxD02),
            "x6" => Ok(Self::X6),
            other => Err(format!(
                "unknown printer model '{other}' (expected 'lx-d02' or 'x6')"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_models_from_advertised_names() {
        assert_eq!(
            PrinterModel::from_device_name("LX-D02"),
            Some(PrinterModel::LxD02)
        );
        assert_eq!(
            PrinterModel::from_device_name("LXP-42"),
            Some(PrinterModel::LxD02)
        );
        assert_eq!(
            PrinterModel::from_device_name("X6h-A1B2"),
            Some(PrinterModel::X6)
        );
        assert_eq!(
            PrinterModel::from_device_name("x6h-A1B2"),
            Some(PrinterModel::X6)
        );
        assert_eq!(PrinterModel::from_device_name("GB01"), None);
        // "X6H-" (capital H) is a *different* model per parzivail; do not match it.
        assert_eq!(PrinterModel::from_device_name("X6H-A1B2"), None);
    }

    #[test]
    fn uuids_per_model() {
        assert_eq!(PrinterModel::LxD02.service_uuid16(), 0xFFE6);
        assert_eq!(PrinterModel::LxD02.write_char_uuid16(), 0xFFE1);
        assert_eq!(PrinterModel::LxD02.notify_char_uuid16(), 0xFFE2);
        assert_eq!(PrinterModel::X6.service_uuid16(), 0xAE30);
        assert_eq!(PrinterModel::X6.write_char_uuid16(), 0xAE01);
        assert_eq!(PrinterModel::X6.notify_char_uuid16(), 0xAE02);
    }

    #[test]
    fn string_round_trip_for_config_and_cli() {
        assert_eq!("lx-d02".parse::<PrinterModel>(), Ok(PrinterModel::LxD02));
        assert_eq!("x6".parse::<PrinterModel>(), Ok(PrinterModel::X6));
        assert!("gb01".parse::<PrinterModel>().is_err());
        assert_eq!(PrinterModel::LxD02.to_string(), "lx-d02");
        assert_eq!(PrinterModel::X6.to_string(), "x6");
    }
}
