use super::*;

#[test]
fn every_registered_fixture_is_versioned_documented_and_explicitly_synthetic() {
    for (label, json) in CORPUS {
        let fixture = Fixture::parse(label, json).unwrap();
        fixture
            .require_protocol("kbctl.synthetic.serial", "1")
            .unwrap();
        assert!(matches!(fixture.origin(), Origin::Synthetic { .. }));
        assert_eq!(fixture.device().firmware, "synthetic-r4");
        fixture.bind("main").unwrap();
    }
}

#[test]
fn every_checked_in_synthetic_v1_json_file_is_registered_for_validation() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/backends/v1/synthetic");
    let files: std::collections::BTreeSet<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect();
    let registered = CORPUS.iter().map(|(name, _)| name.to_string()).collect();
    assert_eq!(
        files, registered,
        "register every fixture with include_str! so regular CI validates it"
    );
}

#[test]
fn unknown_versions_fields_tags_aliases_and_gate_references_are_rejected() {
    let base: serde_json::Value = serde_json::from_str(CORPUS[0].1).unwrap();
    for case in 0..9 {
        let mut value = base.clone();
        match case {
            0 => value["schema_version"] = 2.into(),
            1 => value["surprise"] = true.into(),
            2 => value["device"]["unexpected"] = "ignored?".into(),
            3 => value["endpoints"][0]["steps"][0]["op"] = "guess".into(),
            4 => value["endpoints"][0]["steps"][0]["outcome"]["extra"] = 1.into(),
            5 => {
                let repeated = value["endpoints"][0].clone();
                value["endpoints"].as_array_mut().unwrap().push(repeated);
            }
            6 => value["endpoints"][0]["steps"][0]["after"] = "missing-gate".into(),
            7 => value["device"]["firmware"] = "unknown".into(),
            _ => value["origin"]["sanitization"] = " ".into(),
        }
        assert!(
            Fixture::parse("invalid-fixture", &value.to_string()).is_err(),
            "case {case}"
        );
    }
}

#[test]
fn captured_metadata_requires_provenance_without_pretending_to_validate_capture_sanitization() {
    let mut value: serde_json::Value = serde_json::from_str(CORPUS[0].1).unwrap();
    value["origin"]["kind"] = "captured".into();
    assert!(Fixture::parse("capture-schema-test", &value.to_string()).is_err());
    value["origin"]["source"] = "synthetic metadata exercise, not a real capture".into();
    let fixture = Fixture::parse("capture-schema-test", &value.to_string()).unwrap();
    assert!(matches!(fixture.origin(), Origin::Captured { .. }));
    assert!(fixture.require_protocol("another-protocol", "1").is_err());
}

#[test]
fn malformed_protocol_bytes_remain_loadable_while_invalid_script_counts_do_not() {
    let malformed = load("malformed");
    malformed.bind("main").unwrap();
    let battery = load("battery");
    assert!(
        battery
            .short_write("main", "read-request", vec![3])
            .is_err()
    );
    assert!(
        battery
            .replace_byte("main", "read-reply", usize::MAX, 1)
            .is_err()
    );
    assert!(battery.truncate("main", "read-reply", 999).is_err());
}

#[test]
fn fixture_mutations_preserve_base_identity_and_keep_failure_descriptions_specific() {
    let base = load("firmware");
    let fragmented = base
        .fragment("main", 1)
        .unwrap()
        .short_write("main", "read-request", vec![1, 1])
        .unwrap();
    assert_eq!(base.id(), fragmented.id());
    assert!(fragmented.label().contains("fragment"));
    assert!(fragmented.label().contains("short write"));
    assert!(base.mutations.is_empty());
    fragmented.bind("main").unwrap();
    let truncated = base
        .truncate("main", "read-reply", 3)
        .unwrap()
        .end_after("main", "read-reply", None)
        .unwrap();
    assert!(matches!(
        truncated.document.endpoints[0]
            .steps
            .last()
            .unwrap()
            .outcome,
        Outcome::Eof
    ));
    let changed = base.replace_byte("main", "read-reply", 2, 0xff).unwrap();
    changed.bind("main").unwrap();
    let disconnected = base
        .end_after("main", "read-reply", Some(Failure::Disconnected))
        .unwrap();
    disconnected.bind("main").unwrap();
    let empty_reply = base.truncate("main", "read-reply", 0).unwrap();
    empty_reply.bind("main").unwrap();
}

#[test]
fn coalescing_does_not_cross_gate_or_operation_boundaries() {
    let mut fixture = load("firmware");
    let endpoint = &mut fixture.document.endpoints[0];
    let index = endpoint
        .steps
        .iter()
        .position(|step| step.id == "read-reply")
        .unwrap();
    let mut tail = endpoint.steps[index].clone();
    tail.id = "reply-tail".into();
    if let Outcome::Input { bytes, .. } = &mut tail.outcome {
        *bytes = vec![114, 52];
    }
    if let Outcome::Input { bytes, .. } = &mut endpoint.steps[index].outcome {
        bytes.truncate(3);
    }
    endpoint.steps.insert(index + 1, tail);
    fixture.validate().unwrap();
    let combined = fixture.coalesce("main").unwrap();
    assert_eq!(
        combined.document.endpoints[0].steps.len() + 1,
        fixture.document.endpoints[0].steps.len()
    );
    let Outcome::Input { bytes, .. } = &combined.document.endpoints[0].steps[index].outcome else {
        unreachable!();
    };
    assert_eq!(bytes, &[3, 0, 145, 114, 52]);
}
