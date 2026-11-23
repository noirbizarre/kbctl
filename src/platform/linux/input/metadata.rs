use super::*;
use crate::{
    input::{bitmap, classification::classify},
    model::{identity::Identification, topology::UsbIds},
};
use std::{collections::BTreeMap, ffi::OsStr, fs, io::Read, os::unix::fs::MetadataExt};
mod placement;

/// Constraining reads to metadata makes permission/race tests independent of real keyboards and test UID.
pub(crate) trait Reader {
    fn entries(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn incarnation(&self, path: &Path) -> io::Result<Incarnation>;
    fn input_properties(&self, path: &Path) -> InputProperties;
}
pub(crate) struct SystemMetadata;
impl Reader for SystemMetadata {
    fn entries(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        // Collect fallibly: silently skipped directory entries would turn errors into false removals.
        fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        fs::File::open(path)?.take(4097).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    fn incarnation(&self, path: &Path) -> io::Result<Incarnation> {
        let metadata = fs::metadata(path)?;
        Ok(Incarnation {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    fn input_properties(&self, path: &Path) -> InputProperties {
        // A single udev lookup preserves role combinations without querying the same record repeatedly.
        let device = udev::Device::from_syspath(path);
        INPUT_PROPERTIES
            .iter()
            .map(|&key| {
                (
                    key,
                    match &device {
                        Ok(device) => match device.property_value(key) {
                            Some(value) if matches!(value.to_str(), Some("0" | "1")) => {
                                Metadata::Known(value.to_string_lossy().into_owned())
                            }
                            Some(_) => Metadata::Failed(invalid(
                                path,
                                &format!("decode {key}"),
                                ProblemKind::Malformed,
                            )),
                            None => Metadata::Missing,
                        },
                        Err(error) => {
                            Metadata::Failed(problem(path, "read udev input role metadata", error))
                        }
                    },
                )
            })
            .collect()
    }
}

pub(crate) struct Collector<R> {
    reader: R,
    sys: PathBuf,
    word_bits: Option<u32>,
}
impl<R: Reader> Collector<R> {
    pub(crate) fn new(reader: R, sys: PathBuf, word_bits: Option<u32>) -> Self {
        Self {
            reader,
            sys,
            word_bits,
        }
    }
    fn text(&self, path: &Path) -> Metadata<String> {
        match self.reader.read(path) {
            Ok(bytes) if bytes.len() > 4096 => Metadata::Failed(invalid(
                path,
                "read bounded metadata",
                ProblemKind::Malformed,
            )),
            Ok(bytes) if !bytes.ends_with(b"\n") => Metadata::Failed(invalid(
                path,
                "read complete newline-terminated sysfs metadata",
                ProblemKind::Malformed,
            )),
            Ok(bytes) => match String::from_utf8(bytes) {
                // Strip only the sysfs terminator: serial matching is exact, not whitespace/case normalized.
                Ok(text) => Metadata::Known(text.strip_suffix('\n').unwrap_or(&text).to_owned()),
                Err(_) => Metadata::Failed(invalid(
                    path,
                    "decode UTF-8 metadata",
                    ProblemKind::Malformed,
                )),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Metadata::Missing,
            Err(error) => Metadata::Failed(problem(path, "read metadata", &error)),
        }
    }
    fn hex(&self, path: &Path) -> Metadata<u16> {
        match self.text(path) {
            Metadata::Known(text) => u16::from_str_radix(text.trim(), 16)
                .map(Metadata::Known)
                .unwrap_or_else(|_| {
                    Metadata::Failed(invalid(
                        path,
                        "decode hexadecimal identity",
                        ProblemKind::Malformed,
                    ))
                }),
            Metadata::Failed(problem) => Metadata::Failed(problem),
            Metadata::Missing => Metadata::Missing,
        }
    }
    fn bits(&self, path: &Path) -> Metadata<std::collections::BTreeSet<u16>> {
        let Some(width) = self.word_bits else {
            return Metadata::Failed(invalid(
                path,
                "determine supported kernel bitmap ABI",
                ProblemKind::Unsupported,
            ));
        };
        match self.text(path) {
            Metadata::Known(text) => bitmap::parse(&text, width)
                .map(Metadata::Known)
                .unwrap_or_else(|_| {
                    Metadata::Failed(invalid(path, "decode sysfs bitmap", ProblemKind::Malformed))
                }),
            Metadata::Failed(problem) => Metadata::Failed(problem),
            Metadata::Missing => Metadata::Missing,
        }
    }
    fn usb_parent(&self, input: &Path) -> Result<Option<PathBuf>, Problem> {
        for parent in input
            .ancestors()
            .skip(1)
            .take_while(|parent| *parent != self.sys)
        {
            let subsystem = match self.reader.canonicalize(&parent.join("subsystem")) {
                Ok(path) => path,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(problem(parent, "resolve attachment subsystem", &error)),
            };
            if subsystem.file_name() != Some(OsStr::new("usb")) {
                continue;
            }
            match self.text(&parent.join("uevent")) {
                Metadata::Known(text) if text.lines().any(|line| line == "DEVTYPE=usb_device") => {
                    return Ok(Some(parent.into()));
                }
                Metadata::Failed(problem) => return Err(problem),
                _ => {}
            }
        }
        Ok(None)
    }
    fn endpoint(
        &self,
        path: &Path,
    ) -> Result<(GroupDescriptor, EndpointDescriptor, Vec<Problem>), Problem> {
        let path = self
            .reader
            .canonicalize(path)
            .map_err(|error| problem(path, "resolve input endpoint", &error))?;
        if !path.starts_with(&self.sys) {
            return Err(invalid(
                &path,
                "resolve endpoint inside sysfs",
                ProblemKind::Malformed,
            ));
        }
        let before = self
            .reader
            .incarnation(&path)
            .map_err(|error| problem(&path, "inspect live endpoint", &error))?;
        let input = path
            .parent()
            .ok_or_else(|| invalid(&path, "find input device parent", ProblemKind::Malformed))?;
        let identity = InputIdentity {
            bus: self.hex(&input.join("id/bustype")),
            vendor: self.hex(&input.join("id/vendor")),
            product: self.hex(&input.join("id/product")),
            version: self.hex(&input.join("id/version")),
        };
        // A Bluetooth keyboard may sit below a USB radio; only BUS_USB justifies a USB keyboard model predicate.
        let (usb, grouping_problem, group_confirmed) = match &identity.bus {
            Metadata::Known(3) => match self.usb_parent(input) {
                Ok(parent) => {
                    let confirmed = parent.is_some();
                    (parent, None, confirmed)
                }
                Err(problem) => (None, Some(problem), false),
            },
            Metadata::Known(_) => (None, None, true),
            _ => (None, None, false),
        };
        let group_path = usb.as_deref().unwrap_or(input);
        let group_before = self
            .reader
            .incarnation(group_path)
            .map_err(|error| problem(group_path, "inspect live attachment", &error))?;
        let mut group = GroupDescriptor {
            key: DiscoveryKey(group_path.into()),
            incarnation: Some(group_before.clone()),
            usb: Metadata::Missing,
            serial: Metadata::Missing,
            identification: Identification::Unknown,
        };
        if usb.is_some() {
            group.usb = match (
                self.hex(&group_path.join("idVendor")),
                self.hex(&group_path.join("idProduct")),
            ) {
                (Metadata::Known(vendor), Metadata::Known(product)) => {
                    Metadata::Known(UsbIds { vendor, product })
                }
                (Metadata::Failed(problem), _) | (_, Metadata::Failed(problem)) => {
                    Metadata::Failed(problem)
                }
                _ => Metadata::Missing,
            };
            group.serial = self.text(&group_path.join("serial"));
        }
        let capabilities = Capabilities {
            events: self.bits(&input.join("capabilities/ev")),
            codes: [
                (1, "key"),
                (2, "rel"),
                (3, "abs"),
                (4, "msc"),
                (5, "sw"),
                (17, "led"),
                (18, "snd"),
                (21, "ff"),
            ]
            .into_iter()
            .map(|(code, name)| (code, self.bits(&input.join("capabilities").join(name))))
            .collect(),
            properties: self.bits(&input.join("properties")),
        };
        let properties = self.reader.input_properties(&path);
        let placement = self.placement(input, &identity, usb.as_deref());
        let endpoint = EndpointDescriptor {
            key: DiscoveryKey(path.clone()),
            incarnation: Some(before.clone()),
            group: group.key.clone(),
            group_confirmed,
            devnode: match self.text(&path.join("uevent")) {
                Metadata::Known(text) => text
                    .lines()
                    .find_map(|line| line.strip_prefix("DEVNAME="))
                    .map(|name| {
                        let name = Path::new(name);
                        // Kernel metadata should be relative; malformed enrichment must not escape the device tree.
                        if !name.as_os_str().is_empty()
                            && name
                                .components()
                                .all(|part| matches!(part, std::path::Component::Normal(_)))
                        {
                            Metadata::Known(Path::new("/dev").join(name))
                        } else {
                            Metadata::Failed(invalid(
                                &path,
                                "decode advertised device locator",
                                ProblemKind::Malformed,
                            ))
                        }
                    })
                    .unwrap_or(Metadata::Missing),
                Metadata::Failed(problem) => Metadata::Failed(problem),
                Metadata::Missing => Metadata::Missing,
            },
            name: self.text(&input.join("name")),
            physical_path: self.text(&input.join("phys")),
            unique_name: self.text(&input.join("uniq")),
            classification: classify(&properties, &capabilities),
            input_properties: properties,
            placement,
            identity,
            capabilities,
            stream_access: StreamAccess::NotChecked,
        };
        if self.reader.incarnation(&path).ok().as_ref() != Some(&before)
            || self.reader.incarnation(group_path).ok().as_ref() != Some(&group_before)
        {
            return Err(invalid(
                &path,
                "reconcile replacement during metadata collection",
                ProblemKind::Race,
            ));
        }
        let mut problems = Vec::new();
        problems.extend(grouping_problem);
        fn collect<T>(value: &Metadata<T>, problems: &mut Vec<Problem>) {
            if let Metadata::Failed(problem) = value {
                problems.push(problem.clone());
            }
        }
        collect(&group.usb, &mut problems);
        collect(&group.serial, &mut problems);
        collect(&endpoint.devnode, &mut problems);
        for value in [
            &endpoint.name,
            &endpoint.physical_path,
            &endpoint.unique_name,
        ] {
            collect(value, &mut problems);
        }
        for value in endpoint.input_properties.values() {
            collect(value, &mut problems);
        }
        for fact in &endpoint.placement.facts {
            collect(&fact.value, &mut problems);
        }
        for value in [
            &endpoint.identity.bus,
            &endpoint.identity.vendor,
            &endpoint.identity.product,
            &endpoint.identity.version,
        ] {
            collect(value, &mut problems);
        }
        collect(&endpoint.capabilities.events, &mut problems);
        collect(&endpoint.capabilities.properties, &mut problems);
        for value in endpoint.capabilities.codes.values() {
            collect(value, &mut problems);
        }
        Ok((group, endpoint, problems))
    }
    pub(crate) fn scan(&self) -> Scan {
        let root = self.sys.join("class/input");
        let mut scan = Scan {
            groups: Vec::new(),
            endpoints: Vec::new(),
            complete: true,
            problems: Vec::new(),
        };
        let entries = match self.reader.entries(&root) {
            Ok(entries) => entries,
            Err(error) => {
                scan.complete = false;
                scan.problems.push(problem(
                    &root,
                    "enumerate registered input endpoints",
                    &error,
                ));
                return scan;
            }
        };
        let mut groups = BTreeMap::new();
        for entry in entries {
            let Some(name) = entry
                .file_name()
                .and_then(OsStr::to_str)
                .and_then(|name| name.strip_prefix("event"))
            else {
                continue;
            };
            if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            match self.endpoint(&entry) {
                Ok((group, endpoint, problems)) => {
                    if groups.get(&group.key).is_some_and(|old| *old != group) {
                        scan.complete = false;
                        scan.problems.push(invalid(
                            &entry,
                            "reconcile inconsistent attachment metadata",
                            ProblemKind::Race,
                        ));
                    }
                    groups.insert(group.key.clone(), group);
                    scan.endpoints.push(endpoint);
                    scan.problems.extend(problems);
                }
                Err(problem) => {
                    scan.complete = false;
                    scan.problems.push(problem);
                }
            }
        }
        scan.groups = groups.into_values().collect();
        scan.problems
            .sort_by(|a, b| (&a.path, &a.operation).cmp(&(&b.path, &b.operation)));
        scan.problems.dedup();
        scan
    }
}

#[cfg(test)]
mod tests;
