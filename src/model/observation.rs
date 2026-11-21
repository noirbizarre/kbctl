//! Small shared status vocabulary; storage, timestamps and snapshots belong to #9.

use super::{
    ModelError, Result,
    capability::{ReportedVersion, validate_versions},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Observation<T> {
    Known(T),
    Unknown,
    Unavailable(ReportedUnavailable),
}

/// A device's successful report of temporary unavailability is not an I/O error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportedUnavailable {
    Sleeping,
    ComponentMissing,
    LinkUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Percentage(u8);

impl Percentage {
    pub(crate) fn new(value: u16) -> Result<Self> {
        if value > 100 {
            return Err(ModelError::InvalidPercentage { value });
        }
        Ok(Self(value as u8))
    }
    pub(crate) fn value(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChargingState {
    Charging,
    Discharging,
    Full,
    NotCharging,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatteryState {
    pub(crate) level: Observation<Percentage>,
    pub(crate) charging: Observation<ChargingState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirmwareInfo {
    versions: Vec<ReportedVersion>,
}

impl FirmwareInfo {
    pub(crate) fn new(versions: Vec<ReportedVersion>) -> Result<Self> {
        // Reuse the existing version-fact validation; no alternate version rules
        // should emerge between capability metadata and firmware observations.
        validate_versions(&versions)?;
        Ok(Self { versions })
    }
    pub(crate) fn versions(&self) -> &[ReportedVersion] {
        &self.versions
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceObservation {
    Battery(BatteryState),
    Firmware(Observation<FirmwareInfo>),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_percentages_are_rejected_and_unknown_is_not_zero() {
        assert!(Percentage::new(101).is_err());
        assert_eq!(Percentage::new(100).unwrap().value(), 100);
        assert_ne!(
            Observation::Known(Percentage::new(0).unwrap()),
            Observation::Unknown
        );
    }
}
