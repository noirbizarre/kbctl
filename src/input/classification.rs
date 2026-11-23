//! Pure capability decisions shared by discovery, grouping and collection, never product-name rules.
use crate::model::{identity::InputDeviceId, input::*};
use std::collections::BTreeMap;

pub(crate) fn typing_keys() -> impl Iterator<Item = u16> {
    // Linux codes denote key positions, not a locale: these sets cover all three ordinary letter rows.
    (16..=25)
        .chain(30..=38)
        .chain(44..=50)
        .chain(2..=11)
        .chain([14, 15, 28, 57, 42, 29, 56])
}
pub(crate) fn classify(
    properties: &InputProperties,
    capabilities: &Capabilities,
) -> InputClassification {
    let decision = |kind, reason, coverage| InputClassification {
        kind,
        reason,
        coverage,
    };
    if properties.values().any(|v| match v {
        Metadata::Known(v) => !["0", "1"].contains(&v.as_str()),
        Metadata::Failed(problem) => problem.kind == ProblemKind::Malformed,
        Metadata::Missing => false,
    }) {
        return decision(
            InputClass::Unknown,
            ClassificationReason::ConflictingMetadata,
            None,
        );
    }
    let Some(events) = capabilities.events.known() else {
        return decision(
            InputClass::Unknown,
            ClassificationReason::MissingCapabilities,
            None,
        );
    };
    let property = |key| {
        properties
            .get(key)
            .and_then(Metadata::known)
            .map(String::as_str)
    };
    let claimed = property("ID_INPUT_KEYBOARD") == Some("1");
    if !events.contains(&1) {
        return if claimed {
            decision(
                InputClass::Unknown,
                ClassificationReason::ConflictingMetadata,
                None,
            )
        } else {
            decision(InputClass::Other, ClassificationReason::NoTypingKeys, None)
        };
    }
    let Some(keys) = capabilities.codes.get(&1).and_then(Metadata::known) else {
        return decision(
            InputClass::Unknown,
            ClassificationReason::MissingCapabilities,
            None,
        );
    };
    let coverage = TypingCoverage {
        letters: (16..=25)
            .chain(30..=38)
            .chain(44..=50)
            .filter(|k| keys.contains(k))
            .count(),
        digits: (2..=11).filter(|k| keys.contains(k)).count(),
        editing: [14, 15, 28, 57].iter().filter(|k| keys.contains(k)).count(),
        modifiers: [[42, 54], [29, 97], [56, 100]]
            .iter()
            .filter(|pair| pair.iter().any(|k| keys.contains(k)))
            .count(),
    };
    let complete = coverage.letters == 26
        && coverage.digits == 10
        && coverage.editing == 4
        && coverage.modifiers == 3;
    let non_keyboard = [
        "ID_INPUT_MOUSE",
        "ID_INPUT_TOUCHPAD",
        "ID_INPUT_TOUCHSCREEN",
        "ID_INPUT_TABLET",
        "ID_INPUT_JOYSTICK",
        "ID_INPUT_SWITCH",
    ]
    .iter()
    .any(|key| property(key) == Some("1"))
        || events.contains(&5)
        || (0x110..=0x13f).any(|key| keys.contains(&key))
        || (events.contains(&3) && [0x140, 0x145, 0x14a].iter().any(|key| keys.contains(key)));
    if complete {
        // Explicit negative enrichment conflicts with a full bitmap; absence is not a negative tag.
        return if property("ID_INPUT_KEYBOARD") == Some("0") {
            decision(
                InputClass::Unknown,
                ClassificationReason::ConflictingMetadata,
                Some(coverage),
            )
        } else {
            decision(
                InputClass::Keyboard,
                ClassificationReason::TypingProfile,
                Some(coverage),
            )
        };
    }
    if claimed {
        return decision(
            InputClass::Unknown,
            ClassificationReason::ConflictingMetadata,
            Some(coverage),
        );
    }
    if non_keyboard {
        return decision(
            InputClass::Other,
            ClassificationReason::NonKeyboardRole,
            Some(coverage),
        );
    }
    // Consumer/system keys are intentionally absent: a power button or headset is not a macro pad.
    let partial = typing_keys()
        .chain(59..=68)
        .chain([87, 88, 96, 98, 117, 121])
        .chain(71..=83)
        .chain(183..=194)
        .chain(656..=685)
        .any(|k| keys.contains(&k));
    decision(
        if partial {
            InputClass::Partial
        } else {
            InputClass::Other
        },
        if partial {
            ClassificationReason::TypingSubset
        } else {
            ClassificationReason::NoTypingKeys
        },
        Some(coverage),
    )
}

pub(crate) fn group_class<'a>(
    endpoints: impl Iterator<Item = &'a EndpointDescriptor>,
) -> InputClass {
    let classes: Vec<_> = endpoints.map(|e| e.classification.kind).collect();
    if classes.is_empty() {
        return InputClass::Unknown;
    }
    // Never union partial endpoints into a typing interface, or relabel the group's auxiliary endpoints.
    [
        InputClass::Keyboard,
        InputClass::Partial,
        InputClass::Unknown,
    ]
    .into_iter()
    .find(|kind| classes.contains(kind))
    .unwrap_or(InputClass::Other)
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum CollectionScope {
    #[default]
    Keyboards,
    Device(InputDeviceId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Allow,
    Uncertain,
    Deny,
}

pub(crate) fn admissions<'a>(
    endpoints: impl Iterator<Item = &'a InputEndpoint>,
    scope: CollectionScope,
) -> BTreeMap<crate::model::identity::InputEndpointId, Admission> {
    let endpoints: Vec<_> = endpoints.collect();
    endpoints
        .iter()
        .map(|endpoint| {
            let d = &endpoint.descriptor;
            let peers: Vec<_> = endpoints
                .iter()
                .filter(|e| e.device == endpoint.device)
                .map(|e| &e.descriptor)
                .collect();
            let group = group_class(peers.iter().copied());
            let in_scope = match scope {
                CollectionScope::Keyboards => group == InputClass::Keyboard,
                CollectionScope::Device(id) => {
                    id == endpoint.device
                        && matches!(group, InputClass::Keyboard | InputClass::Partial)
                }
            };
            let explicitly_other_role =
                d.classification.reason == ClassificationReason::NonKeyboardRole;
            let standard_led = d
                .capabilities
                .codes
                .get(&17)
                .and_then(Metadata::known)
                .is_some_and(|codes| [0, 1, 2].iter().any(|code| codes.contains(code)));
            let typing = d.classification.kind == InputClass::Keyboard
                || (matches!(scope, CollectionScope::Device(_))
                    && d.classification.kind == InputClass::Partial);
            let auxiliary = d.group_confirmed
                && !explicitly_other_role
                && standard_led
                && d.classification.kind != InputClass::Unknown;
            let scope_permits_uncertainty = match scope {
                CollectionScope::Keyboards => true,
                CollectionScope::Device(id) => {
                    id == endpoint.device
                        && matches!(group, InputClass::Keyboard | InputClass::Partial)
                }
            };
            let uncertain = scope_permits_uncertainty
                && !explicitly_other_role
                && d.classification.reason != ClassificationReason::ConflictingMetadata
                && (d.classification.reason == ClassificationReason::MissingCapabilities
                    || (group == InputClass::Unknown
                        && peers.iter().any(|e| {
                            e.classification.reason == ClassificationReason::MissingCapabilities
                        })));
            (
                endpoint.id,
                if in_scope && (typing || auxiliary) {
                    Admission::Allow
                } else if uncertain {
                    Admission::Uncertain
                } else {
                    Admission::Deny
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
