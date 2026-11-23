//! Placement is qualified platform evidence, never a product-name or USB-bus shortcut.
use super::*;

impl<R: Reader> Collector<R> {
    pub(super) fn placement(
        &self,
        input: &Path,
        identity: &InputIdentity,
        usb: Option<&Path>,
    ) -> Placement {
        if input.starts_with(self.sys.join("devices/virtual")) || identity.bus.known() == Some(&6) {
            // Uinput can advertise BUS_USB. Canonical virtual ancestry still wins over that advertised value.
            return Placement {
                origin: InputOrigin::Virtual,
                location: InputLocation::Unknown,
                reason: "virtual_input_ancestry_or_bus",
                facts: vec![PlacementFact {
                    path: input.into(),
                    value: Metadata::Known("virtual_input".into()),
                }],
            };
        }
        let origin = if input.starts_with(self.sys.join("devices")) {
            InputOrigin::Physical
        } else {
            InputOrigin::Unknown
        };
        let mut placement = Placement {
            origin,
            ..Placement::default()
        };
        if identity.bus.known() == Some(&0x11)
            && input.starts_with(self.sys.join("devices/platform/i8042"))
        {
            let path = self.sys.join("class/dmi/id/chassis_type");
            let value = match self.text(&path) {
                Metadata::Known(value) if value.parse::<u16>().is_err() => Metadata::Failed(
                    invalid(&path, "decode chassis type", ProblemKind::Malformed),
                ),
                value => value,
            };
            let portable = value
                .known()
                .and_then(|v| v.parse::<u16>().ok())
                .is_some_and(|v| [8, 9, 10, 14, 30, 31, 32].contains(&v));
            placement.facts.push(PlacementFact {
                path: input.into(),
                value: Metadata::Known("platform_i8042".into()),
            });
            placement.facts.push(PlacementFact { path, value });
            if portable {
                placement.location = InputLocation::Internal;
                placement.reason = "i8042_on_portable_chassis";
            }
        }
        if let Some(usb) = usb {
            let mut root_seen = false;
            for ancestor in usb.ancestors().take_while(|p| *p != self.sys) {
                match self.reader.canonicalize(&ancestor.join("subsystem")) {
                    Ok(subsystem) if subsystem.file_name() == Some(OsStr::new("usb")) => {}
                    Ok(_) => continue,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        placement.facts.push(PlacementFact {
                            path: ancestor.into(),
                            value: Metadata::Failed(problem(
                                ancestor,
                                "resolve USB placement ancestry",
                                &error,
                            )),
                        });
                        break;
                    }
                }
                match self.text(&ancestor.join("uevent")) {
                    Metadata::Known(value)
                        if value.lines().any(|line| line == "DEVTYPE=usb_device") => {}
                    Metadata::Failed(error) => {
                        placement.facts.push(PlacementFact {
                            path: ancestor.into(),
                            value: Metadata::Failed(error),
                        });
                        break;
                    }
                    _ => continue,
                }
                // Verified USB-device roots use the kernel's usbN sysname; their own removability is not a port hop.
                if ancestor
                    .file_name()
                    .and_then(OsStr::to_str)
                    .and_then(|s| s.strip_prefix("usb"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                {
                    root_seen = true;
                    break;
                }
                let path = ancestor.join("removable");
                let value = match self.text(&path) {
                    Metadata::Known(value)
                        if !["fixed", "removable", "unknown"].contains(&value.as_str()) =>
                    {
                        Metadata::Failed(invalid(
                            &path,
                            "decode USB removability",
                            ProblemKind::Malformed,
                        ))
                    }
                    value => value,
                };
                placement.facts.push(PlacementFact { path, value });
            }
            if placement
                .facts
                .iter()
                .any(|f| f.value.known().is_some_and(|v| v == "removable"))
            {
                placement.location = InputLocation::External;
                placement.reason = "removable_usb_ancestry";
            } else if root_seen
                && !placement.facts.is_empty()
                && placement
                    .facts
                    .iter()
                    .all(|f| f.value.known().is_some_and(|v| v == "fixed"))
            {
                // A fixed child on a removable dock is external, so every hop to the root must positively be fixed.
                placement.location = InputLocation::Internal;
                placement.reason = "fixed_usb_chain_to_root";
            }
        }
        placement
    }
}
