//! Inventory-relative readable handles, separate from display names and version-one evidence aliases.
use super::devices::DeviceView;
use crate::model::input::{InputClass, InputLocation, InputOrigin};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const SLUG_LIMIT: usize = 40;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct UserSelector {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) qualified: String,
    pub(crate) bare_alias: Option<String>,
    pub(crate) bare_ambiguous: bool,
    pub(crate) name_source: &'static str,
    pub(crate) disambiguation: &'static str,
    pub(crate) stability: &'static str,
    pub(crate) qualified_stability: &'static str,
    pub(crate) ambiguous: bool,
    pub(crate) qualified_ambiguous: bool,
}
impl UserSelector {
    pub(crate) fn matches(&self, query: &str) -> bool {
        query == self.id || query == self.qualified || self.bare_alias.as_deref() == Some(query)
    }
}

struct Basis {
    slug: String,
    source: &'static str,
    generic: bool,
    token: String,
    attachment: bool,
}

pub(crate) fn assign(devices: &[DeviceView]) -> Vec<UserSelector> {
    let bases: Vec<_> = devices.iter().map(basis).collect();
    let mut slugs = BTreeMap::<&str, usize>::new();
    let mut legacy = BTreeMap::<&str, BTreeSet<usize>>::new();
    for (index, device) in devices.iter().enumerate() {
        if !bases[index].generic {
            *slugs.entry(&bases[index].slug).or_default() += 1;
        }
        for alias in [&device.selector.id, &device.selector.fallback] {
            legacy.entry(alias).or_default().insert(index);
        }
    }
    bases
        .iter()
        .enumerate()
        .map(|(index, basis)| {
            let mut qualified = String::new();
            let mut ambiguous = true;
            for width in [8, 12, 16, 24, 32] {
                qualified = qualified_at(basis, width);
                ambiguous = bases
                    .iter()
                    .enumerate()
                    .any(|(other, b)| other != index && qualified_at(b, width) == qualified);
                if !ambiguous {
                    break;
                }
            }
            // A model slug must not steal a compatibility alias belonging to another attachment.
            let reserved_alias = legacy
                .get(basis.slug.as_str())
                .is_some_and(|targets| targets.iter().any(|target| *target != index));
            let bare_available = !basis.generic && !reserved_alias;
            let bare_ambiguous =
                bare_available && slugs.get(basis.slug.as_str()).copied().unwrap_or(0) > 1;
            let bare = bare_available && !bare_ambiguous;
            let qualified_stability = if basis.attachment {
                "attachment_scoped"
            } else if ["registry_model", "usb_product"].contains(&basis.source) {
                "model_and_advertised_serial"
            } else {
                "label_and_advertised_serial"
            };
            UserSelector {
                id: if bare {
                    basis.slug.clone()
                } else {
                    qualified.clone()
                },
                slug: basis.slug.clone(),
                qualified,
                bare_alias: bare_available.then(|| basis.slug.clone()),
                bare_ambiguous,
                name_source: basis.source,
                disambiguation: if basis.attachment {
                    "attachment"
                } else {
                    "advertised_serial"
                },
                stability: if bare {
                    "inventory_relative"
                } else {
                    qualified_stability
                },
                qualified_stability,
                ambiguous: !bare && ambiguous,
                qualified_ambiguous: ambiguous,
            }
        })
        .collect()
}

fn qualified_at(basis: &Basis, width: usize) -> String {
    // Slugs collapse separators, so a double hyphen is reserved for generated aliases, never an ordinary model slug.
    let token: String = basis.token.chars().take(width).collect();
    format!(
        "{}--{}{token}",
        basis.slug,
        if basis.attachment { "at-" } else { "" }
    )
}

fn basis(device: &DeviceView) -> Basis {
    let model = device
        .group
        .descriptor
        .model_label
        .as_deref()
        .and_then(usable)
        .map(|s| (s, "registry_model"))
        .or_else(|| {
            device
                .group
                .descriptor
                .product
                .known()
                .and_then(|s| usable(s))
                .map(|s| (s, "usb_product"))
        });
    let (slug, source, generic) = if let Some((slug, source)) = model {
        (slug, source, false)
    } else if device.class == InputClass::Keyboard
        && device.origin == InputOrigin::Physical
        && device.location == InputLocation::Internal
    {
        ("internal-keyboard".into(), "internal_placement", false)
    } else {
        // Kernel-name fallback is explicit and weak. Renumbering endpoints must not change which sibling supplies it.
        let mut names: Vec<_> = device
            .endpoints
            .iter()
            .filter_map(|e| {
                e.endpoint
                    .descriptor
                    .name
                    .known()
                    .and_then(|name| usable(name))
                    .map(|slug| {
                        let rank = match e.endpoint.descriptor.classification.kind {
                            InputClass::Keyboard => 0,
                            InputClass::Partial => 1,
                            _ => 2,
                        };
                        (rank, slug)
                    })
            })
            .collect();
        names.sort();
        if let Some((_, name)) = names.into_iter().next() {
            (name, "kernel_name", false)
        } else {
            (
                match (device.class, device.origin) {
                    (InputClass::Keyboard, InputOrigin::Virtual) => "virtual-keyboard",
                    (InputClass::Keyboard, _) => "keyboard",
                    (InputClass::Partial, _) => "keypad",
                    _ => "input-device",
                }
                .into(),
                "generic",
                true,
            )
        }
    };
    let attachment = device.selector.kind != "usb_serial" || device.selector.ambiguous;
    let evidence = if attachment {
        &device.selector.fallback
    } else {
        &device.selector.id
    };
    let token = evidence
        .strip_prefix(if attachment { "attachment-" } else { "usb-" })
        .unwrap_or(evidence)
        .to_owned();
    Basis {
        slug,
        source,
        generic,
        token,
        attachment,
    }
}

fn usable(text: &str) -> Option<String> {
    let mut slug = String::new();
    let mut separator = false;
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(char::from(byte.to_ascii_lowercase()));
            separator = false;
        } else {
            separator = true;
        }
    }
    let mut slug = slug.trim_end_matches('-').to_owned();
    // Reserve the legacy evidence namespace even when its owning device is disconnected.
    let legacy_shape = ["usb-", "attachment-"].iter().any(|prefix| {
        slug.strip_prefix(prefix)
            .is_some_and(|token| token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()))
    });
    if legacy_shape {
        return None;
    }
    slug.truncate(SLUG_LIMIT);
    let slug = slug.trim_end_matches('-').to_owned();
    (!slug.is_empty()).then_some(slug)
}

#[cfg(test)]
mod tests;
