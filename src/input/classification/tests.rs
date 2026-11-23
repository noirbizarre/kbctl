use super::*;
use crate::{
    model::identity::InputEndpointId,
    test_support::input::{failure, scan},
};

fn classify_keys(
    keys: impl IntoIterator<Item = u16>,
    properties: InputProperties,
) -> InputClassification {
    let mut capabilities = scan().endpoints.remove(0).capabilities;
    capabilities
        .codes
        .insert(1, Metadata::Known(keys.into_iter().collect()));
    classify(&properties, &capabilities)
}

#[test]
fn complete_typing_requires_all_letter_rows_digits_editing_and_modifier_groups() {
    let keys: Vec<_> = typing_keys().collect();
    assert_eq!(
        classify_keys(keys.clone(), InputProperties::new()).kind,
        InputClass::Keyboard
    );
    for missing in [2, 14, 15, 16, 28, 29, 30, 42, 44, 56, 57] {
        assert_eq!(
            classify_keys(
                keys.iter().copied().filter(|key| *key != missing),
                InputProperties::new()
            )
            .kind,
            InputClass::Partial
        );
    }
    let right_modifiers = keys
        .into_iter()
        .filter(|k| ![29, 42, 56].contains(k))
        .chain([54, 97, 100]);
    assert_eq!(
        classify_keys(right_modifiers, InputProperties::new()).kind,
        InputClass::Keyboard
    );
}

#[test]
fn keypads_macro_pads_and_partial_typing_rows_are_not_full_keyboards() {
    for keys in [
        (71..=83).collect::<Vec<_>>(),
        vec![30, 31, 32],
        vec![656],
        (183..=194).collect(),
        (16..=25).collect(),
    ] {
        assert_eq!(
            classify_keys(keys, InputProperties::new()).kind,
            InputClass::Partial
        );
    }
}

#[test]
fn power_video_audio_lid_pointer_and_controller_capabilities_are_not_keyboards() {
    // Standard system/consumer and BTN codes: labels and USB vendor IDs do not participate.
    for keys in [
        vec![116],
        vec![142, 143],
        vec![224, 225, 227],
        vec![113, 114, 115, 164],
        vec![0x110, 0x111],
        vec![0x130, 0x131],
        vec![],
    ] {
        assert_eq!(
            classify_keys(keys, InputProperties::new()).kind,
            InputClass::Other
        );
    }
    let mut capabilities = scan().endpoints.remove(0).capabilities;
    capabilities.events = Metadata::Known([0, 5].into());
    assert_eq!(
        classify(&InputProperties::new(), &capabilities).kind,
        InputClass::Other
    );
    for role in ["ID_INPUT_MOUSE", "ID_INPUT_TOUCHPAD", "ID_INPUT_JOYSTICK"] {
        let properties = InputProperties::from([(role, Metadata::Known("1".into()))]);
        assert_eq!(classify_keys([30], properties).kind, InputClass::Other);
    }
}

#[test]
fn missing_or_conflicting_evidence_never_becomes_a_full_keyboard() {
    let positive = InputProperties::from([("ID_INPUT_KEYBOARD", Metadata::Known("1".into()))]);
    let mut capabilities = scan().endpoints.remove(0).capabilities;
    capabilities.codes.insert(1, Metadata::Failed(failure()));
    assert_eq!(classify(&positive, &capabilities).kind, InputClass::Unknown);
    assert_eq!(
        classify_keys([116], positive).reason,
        ClassificationReason::ConflictingMetadata
    );
    for property in [
        Metadata::Known("0".into()),
        Metadata::Known("invalid".into()),
    ] {
        assert_eq!(
            classify_keys(
                typing_keys(),
                InputProperties::from([("ID_INPUT_KEYBOARD", property)])
            )
            .kind,
            InputClass::Unknown
        );
    }
    capabilities.events = Metadata::Missing;
    assert_eq!(
        classify(&InputProperties::new(), &capabilities).kind,
        InputClass::Unknown
    );
}

#[test]
fn grouping_preserves_a_typing_interface_but_does_not_union_partial_endpoints() {
    assert_eq!(group_class(std::iter::empty()), InputClass::Unknown);
    let mut fixture = scan();
    let mut media = fixture.endpoints[0].clone();
    media
        .capabilities
        .codes
        .insert(1, Metadata::Known([113, 114, 115].into()));
    media.classification = classify(&media.input_properties, &media.capabilities);
    fixture.endpoints.push(media);
    assert_eq!(group_class(fixture.endpoints.iter()), InputClass::Keyboard);
    for endpoint in &mut fixture.endpoints {
        endpoint
            .capabilities
            .codes
            .insert(1, Metadata::Known([30, 31].into()));
        endpoint.classification = classify(&endpoint.input_properties, &endpoint.capabilities);
    }
    assert_eq!(group_class(fixture.endpoints.iter()), InputClass::Partial);
}

#[test]
fn a_lock_led_does_not_admit_an_unrelated_input_but_confirmed_siblings_can_report() {
    let descriptor = scan().endpoints.remove(0);
    let id = crate::model::identity::InputDeviceId::new().unwrap();
    let keyboard = InputEndpoint {
        id: InputEndpointId::new().unwrap(),
        device: id,
        descriptor: descriptor.clone(),
    };
    let mut media = InputEndpoint {
        id: InputEndpointId::new().unwrap(),
        device: id,
        descriptor,
    };
    media
        .descriptor
        .capabilities
        .codes
        .insert(1, Metadata::Known([113].into()));
    media.descriptor.classification = classify(
        &media.descriptor.input_properties,
        &media.descriptor.capabilities,
    );
    assert_eq!(
        admissions([&media].into_iter(), CollectionScope::Keyboards)[&media.id],
        Admission::Deny
    );
    assert_eq!(
        admissions([&keyboard, &media].into_iter(), CollectionScope::Keyboards)[&media.id],
        Admission::Allow
    );
    media.descriptor.group_confirmed = false;
    assert_eq!(
        admissions([&keyboard, &media].into_iter(), CollectionScope::Keyboards)[&media.id],
        Admission::Deny
    );
}
