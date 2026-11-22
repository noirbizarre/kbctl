//! Successful device reports and typed partial patches, independent of protocols.

use std::collections::{BTreeMap, BTreeSet};

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
    Patch(DevicePatch),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NamespacedId {
    namespace: String,
    value: String,
}
impl NamespacedId {
    pub(crate) fn new(namespace: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        Ok(Self {
            namespace: super::identity::nonblank(namespace.into(), "status namespace")?,
            value: super::identity::nonblank(value.into(), "status identifier")?,
        })
    }
    pub(crate) fn namespace(&self) -> &str {
        &self.namespace
    }
    pub(crate) fn value(&self) -> &str {
        &self.value
    }
    pub(crate) fn encoded_size(&self) -> usize {
        self.namespace.len().saturating_add(self.value.len())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HostLock {
    Caps,
    Num,
    Scroll,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeviceIndicator {
    CapsLock,
    NumLock,
    ScrollLock,
    Named(NamespacedId),
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LightingZone {
    Component,
    Named(NamespacedId),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgb {
    pub(crate) red: u8,
    pub(crate) green: u8,
    pub(crate) blue: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reachability {
    Reachable,
    Unreachable,
    Sleeping,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeviceField {
    Indicator(DeviceIndicator),
    ActiveLayers,
    LightingEnabled(LightingZone),
    Brightness(LightingZone),
    Color(LightingZone),
    Effect(LightingZone),
    BatteryLevel,
    BatteryCharging,
    Firmware,
    Reachability,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusValue {
    Bool(bool),
    Percent(Percentage),
    Charging(ChargingState),
    Layers(BTreeSet<NamespacedId>),
    Rgb(Rgb),
    Identifier(NamespacedId),
    Firmware(FirmwareInfo),
    Reachability(Reachability),
}
impl DeviceField {
    pub(crate) fn accepts(&self, value: &StatusValue) -> bool {
        matches!(
            (self, value),
            (
                Self::Indicator(_) | Self::LightingEnabled(_),
                StatusValue::Bool(_)
            ) | (
                Self::Brightness(_) | Self::BatteryLevel,
                StatusValue::Percent(_)
            ) | (Self::BatteryCharging, StatusValue::Charging(_))
                | (Self::ActiveLayers, StatusValue::Layers(_))
                | (Self::Color(_), StatusValue::Rgb(_))
                | (Self::Effect(_), StatusValue::Identifier(_))
                | (Self::Firmware, StatusValue::Firmware(_))
                | (Self::Reachability, StatusValue::Reachability(_))
        )
    }
    pub(crate) fn encoded_size(&self) -> usize {
        match self {
            Self::Indicator(DeviceIndicator::Named(id)) => id.encoded_size(),
            Self::LightingEnabled(LightingZone::Named(id))
            | Self::Brightness(LightingZone::Named(id))
            | Self::Color(LightingZone::Named(id))
            | Self::Effect(LightingZone::Named(id)) => id.encoded_size(),
            _ => 1,
        }
    }
}
impl StatusValue {
    pub(crate) fn encoded_size(&self) -> usize {
        match self {
            Self::Layers(ids) => ids
                .iter()
                .fold(0usize, |size, id| size.saturating_add(id.encoded_size())),
            Self::Identifier(id) => id.encoded_size(),
            Self::Firmware(info) => info.versions().iter().fold(0usize, |size, v| {
                size.saturating_add(v.namespace().len())
                    .saturating_add(v.value().len())
            }),
            _ => 3,
        }
    }
}

/// Producer clock evidence is opaque; ticks in different domains are not compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MeasurementTime {
    pub(crate) clock: NamespacedId,
    pub(crate) ticks: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeafUpdate {
    pub(crate) value: Observation<StatusValue>,
    pub(crate) measured_at: Option<MeasurementTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DevicePatch {
    fields: BTreeMap<DeviceField, LeafUpdate>,
}
fn map_observation<T>(
    value: Observation<T>,
    map: impl FnOnce(T) -> StatusValue,
) -> Observation<StatusValue> {
    match value {
        Observation::Known(value) => Observation::Known(map(value)),
        Observation::Unknown => Observation::Unknown,
        Observation::Unavailable(reason) => Observation::Unavailable(reason),
    }
}
impl DevicePatch {
    fn one(field: DeviceField, value: Observation<StatusValue>) -> Self {
        Self {
            fields: BTreeMap::from([(
                field,
                LeafUpdate {
                    value,
                    measured_at: None,
                },
            )]),
        }
    }
    pub(crate) fn battery(
        level: Option<Observation<Percentage>>,
        charging: Option<Observation<ChargingState>>,
    ) -> Self {
        let mut patch = Self::default();
        if let Some(value) = level {
            patch.fields.insert(
                DeviceField::BatteryLevel,
                LeafUpdate {
                    value: map_observation(value, StatusValue::Percent),
                    measured_at: None,
                },
            );
        }
        if let Some(value) = charging {
            patch.fields.insert(
                DeviceField::BatteryCharging,
                LeafUpdate {
                    value: map_observation(value, StatusValue::Charging),
                    measured_at: None,
                },
            );
        }
        patch
    }
    pub(crate) fn indicator(id: DeviceIndicator, value: Observation<bool>) -> Self {
        Self::one(
            DeviceField::Indicator(id),
            map_observation(value, StatusValue::Bool),
        )
    }
    pub(crate) fn layers(ids: Observation<Vec<NamespacedId>>) -> Result<Self> {
        if let Observation::Known(ids) = &ids
            && ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
        {
            return Err(ModelError::InvalidStatus(
                "active layer set contains duplicates",
            ));
        }
        Ok(Self::one(
            DeviceField::ActiveLayers,
            map_observation(ids, |ids| StatusValue::Layers(ids.into_iter().collect())),
        ))
    }
    pub(crate) fn lighting_enabled(zone: LightingZone, value: Observation<bool>) -> Self {
        Self::one(
            DeviceField::LightingEnabled(zone),
            map_observation(value, StatusValue::Bool),
        )
    }
    pub(crate) fn brightness(zone: LightingZone, value: Observation<Percentage>) -> Self {
        Self::one(
            DeviceField::Brightness(zone),
            map_observation(value, StatusValue::Percent),
        )
    }
    pub(crate) fn color(zone: LightingZone, value: Observation<Rgb>) -> Self {
        Self::one(
            DeviceField::Color(zone),
            map_observation(value, StatusValue::Rgb),
        )
    }
    pub(crate) fn effect(zone: LightingZone, value: Observation<NamespacedId>) -> Self {
        Self::one(
            DeviceField::Effect(zone),
            map_observation(value, StatusValue::Identifier),
        )
    }
    pub(crate) fn firmware(value: Observation<FirmwareInfo>) -> Self {
        Self::one(
            DeviceField::Firmware,
            map_observation(value, StatusValue::Firmware),
        )
    }
    pub(crate) fn connection(value: Observation<Reachability>) -> Self {
        Self::one(
            DeviceField::Reachability,
            map_observation(value, StatusValue::Reachability),
        )
    }
    pub(crate) fn measured_at(mut self, time: MeasurementTime) -> Self {
        for leaf in self.fields.values_mut() {
            leaf.measured_at = Some(time.clone());
        }
        self
    }
    pub(crate) fn merge(mut self, other: Self) -> Result<Self> {
        if other
            .fields
            .keys()
            .any(|field| self.fields.contains_key(field))
        {
            return Err(ModelError::InvalidStatus(
                "patch supplies the same leaf twice",
            ));
        }
        self.fields.extend(other.fields);
        Ok(self)
    }
    pub(crate) fn fields(&self) -> &BTreeMap<DeviceField, LeafUpdate> {
        &self.fields
    }
    pub(crate) fn encoded_size(&self) -> usize {
        self.fields.iter().fold(0usize, |size, (field, leaf)| {
            let value = match &leaf.value {
                Observation::Known(value) => value.encoded_size(),
                _ => 1,
            };
            size.saturating_add(field.encoded_size())
                .saturating_add(value)
                .saturating_add(
                    leaf.measured_at
                        .as_ref()
                        .map_or(0, |time| time.clock.encoded_size().saturating_add(8)),
                )
        })
    }
}
impl DeviceObservation {
    pub(crate) fn patch(&self) -> DevicePatch {
        match self {
            Self::Battery(value) => {
                DevicePatch::battery(Some(value.level.clone()), Some(value.charging.clone()))
            }
            Self::Firmware(value) => DevicePatch::firmware(value.clone()),
            Self::Patch(patch) => patch.clone(),
        }
    }
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
