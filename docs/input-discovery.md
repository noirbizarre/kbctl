# Linux input discovery

Issue [#11](https://github.com/noirbizarre/kbctl/issues/11) implements an internal, metadata-only input discovery service.
It inventories Linux input endpoints and supported events, classifies ordinary keyboard input, identifies USB models
through the registry, and reconciles hotplug through the existing runtime/state service. The [CLI](cli.md) starts the
direct discovery host; daemon hosting remains future work.

## Inventory and identity

[`model::input`](../src/model/input.rs) represents input attachment groups and endpoints separately from programmable
keyboard topology. Input discovery creates **no logical keyboards, management endpoints, components or backend routes**.
Non-keyboard and consumer-control sibling endpoints remain in the inventory.

Each group/endpoint has a process-local opaque ID. Syspaths, inode/device fingerprints and event-node locators describe
live attachments, not persistent hardware identity. A replacement gets fresh IDs; identical names, USB identifiers and
advertised serials do not merge independent attachments. These fingerprints are continuity evidence, not an infallible
history through arbitrary monitoring outages. Uncertain instances are renewed after a coverage gap.

The CLI derives [readable handles](cli.md#selector-evidence-and-persistence) separately from display/runtime identity, with
evidence-qualified aliases for duplicates. Naming metadata includes fallible USB product text and the resolved registry
model label; names never affect capability classification. Existing USB/serial and attachment-digest selectors remain
technical compatibility aliases. These do not change runtime identity,
verify serial claims, establish chassis boundaries or merge groups. Grouped indicator summaries preserve disagreement
and uncertainty across applicable sibling endpoints; raw observations remain endpoint-scoped.

For known `BUS_USB` input devices, the nearest actual USB device ancestor groups sibling interfaces. The group describes
a **shared host attachment**: a receiver is not proof that all its input nodes belong to one keyboard chassis. Other
transports and virtual devices conservatively use their individual `inputN` parent; the common virtual-input root and
USB Bluetooth radio never become a keyboard-grouping shortcut. Failed grouping metadata is uncertain, not proof that a
confirmed live endpoint moved elsewhere.

Only USB input devices with actual USB ancestor identifiers feed USB registry predicates. Linux input vendor/product
numbers on Bluetooth or other buses do not substitute for USB identifiers. Advertised serial matching preserves case
and spaces; neither serials nor registry model recognition establish individual identity or pairing. The pure
`Registry::identify_metadata` entry point shares physical predicates with topology inspection and preserves ambiguity.

An application can explicitly accept an association between a current input group and an existing physical-device ID
through `Runtime::associate_input_device`. It must justify that relationship separately. Association grants no management
authority. Input removal preserves management topology; physical retirement clears its association atomically. Logical
membership follows current topology rather than a copied keyboard ID.

## Metadata and access

The Linux adapter enumerates actual entries in `/sys/class/input`, resolves their sysfs device metadata, and uses udev
properties for enrichment. It does not guess numbered device paths. Direct fallible metadata reads preserve permission,
I/O and parse failures that optional udev attribute getters would erase.

Metadata distinguishes known values, missing data and typed failures. Known empty bitmaps/strings and numeric zero are
not unknown. Supported event types, input properties and available key/relative/absolute/miscellaneous/switch/LED/sound/
force-feedback code sets preserve numeric codes, including unfamiliar ones. Discovery does not query axis parameters,
keymaps or live input values. `EV_LED` support does not establish current host locks, physical illumination or programmable
lighting capabilities.

Every endpoint's event-stream access is **`NotChecked`**. Discovery never opens `/dev/input/event*`, reads key events,
grabs input, changes LED state, installs access rules or acquires hardware-management ownership. Metadata readability is
not evidence that an eventual event-stream open would succeed. Actual collectors must apply their own access/lifetime
contract; [host indicator observation](host-indicators.md) is implemented separately in #12, while control remains #13.

Failures carry the affected metadata path, operation, category and available OS error code, with an actionable
`kbctl::input::discovery` diagnostic. Check the named path, sysfs/udev visibility, container mounts and specific access
policy. Granting broad input access or running the application as root is not the discovery recovery procedure.
Periodic reconciliation retries metadata after access improves. Unique descriptors stay out of routine logs.

### Bitmap ABI and keyboard classification

Sysfs capability bitmaps contain most-significant-first **kernel unsigned-long words**. Short hex tokens do not shorten
word boundaries. The pure parser accepts an explicit 32/64-bit width and rejects malformed/overflowing input; the Linux
adapter selects 64 for the currently supported x86-64/AArch64 kernel environments. Other unestablished kernel ABIs report
unsupported capability metadata. Process pointer width or the longest printed token is not a kernel-bitness detector.
Reads are bounded to 4096 bytes and require the complete sysfs newline terminator.

`input::classification` defines four classes: typing keyboard, partial keyboard/keypad, other input and unknown. Discovery
recomputes classification from evidence before publication, rather than trusting a stale cached label. Required positive
typing evidence is EV_KEY plus all ordinary letter rows (`16..25`, `30..38`, `44..50`), digits (`2..11`), editing keys
Backspace/Tab/Enter/Space, and Shift/Ctrl/Alt groups (either side). This describes advertised typing capabilities, not the
number of physical switches: compact/split keyboards can satisfy it without a numpad or function row.

Positive but incomplete typing, numeric-keypad, function or macro-key subsets are partial keyboards. Consumer/system keys
(power, sleep, brightness, volume, media), switches and pointer/controller buttons do not alone qualify. Positive pointer,
touch, controller and switch roles prevent their limited shortcut keys from becoming partial typing devices; a complete
typing bitmap can still describe a combined keyboard/pointer interface. Names and vendor/model tables never decide class.

Relevant ID_INPUT_* roles are retained with individual known/missing/failed status. A keyboard tag alone cannot establish
typing capability. Missing tags are not negative evidence; explicit contradictory or malformed role metadata and unreadable
required bitmaps produce unknown classification. Raw capabilities, coverage counts and reasons remain inspectable.
One typing endpoint establishes its attachment's keyboard classification without relabeling auxiliary endpoints. Partial
endpoints are never unioned to fabricate typing coverage. Defaults exclude partial/other/unknown groups; `--all` and
unrestricted inspect retain them. Explicit indicator selection can query a positively classified partial group.

### Origin and physical placement

Origin and location are independent. Canonical virtual ancestry or BUS_VIRTUAL gives virtual origin with unknown physical
location, even when a virtual interface advertises USB. Non-virtual sysfs device ancestry supplies physical-origin evidence.
For i8042 ancestry with BUS_I8042, readable DMI chassis types 8, 9, 10, 14, 30, 31 or 32 support qualified internal placement;
desktop PS/2 bus/name alone does not. USB `removable` evidence is followed through actual device ancestors: any removable
hop supports external placement; internal placement requires every device hop fixed through a verified USB root. This
avoids mistaking a fixed child of an external hub for a built-in keyboard. Missing/conflicting/malformed/denied evidence
stays unknown with its raw facts and diagnostics. No product-name or USB/I2C bus-only location inference is used.

Placement is metadata-derived evidence, not hardware attestation, and never changes grouping or selector identity.
Tests use synthetic typing and system-control profiles rather than claiming captured-device interoperability.

## Runtime and hosting contract

[`input::Service`](../src/input/mod.rs) accepts an injected `Source`, registry and scheduling configuration. The source
starts/reopens monitoring, drains a bounded notification batch and returns complete-or-incomplete membership scans.
[`platform::linux::input::LinuxSource`](../src/platform/linux/input.rs) is the real adapter.

The direct CLI host (and a future daemon host):

1. Constructs the real source, registry, service and shared `Runtime` with the same monotonic time domain.
2. Calls `step(now, &mut runtime)` to enable monitoring before the initial scan and publish the inventory.
3. Schedules subsequent steps on monitor readiness and `next_deadline()`, including immediate pending-drain work.
4. Re-registers the borrowed monitor FD when its registration generation changes; ignores stale generation tokens.
5. Consumes normalized snapshots/commits through the ordinary `StateReader`, and renders returned problems as needed.

There is no hidden thread, executor or async-runtime dependency. Steps use synchronous metadata I/O: notification count
and retries are bounded, but filesystem operations are not claimed to have strict wall-clock deadlines.

Notifications only invalidate enumeration; even a delayed removal requests a fresh scan rather than deleting an ID.
Monitoring uses processed udev input-subsystem events. A small receive adapter preserves errno because udev's convenient
iterator hides failures. Would-block is idle; interrupted/error/ambiguous reception is handled explicitly. Filtered event
sequence gaps do not imply lost input events. Periodic scans repair missed notifications regardless of monitor health.

Defaults are a 30-second reconciliation period, 64 notifications per turn, and 1–30-second bounded retry backoff.
Monitor startup failure still allows degraded enumeration. Recovery replaces the monitor registration and triggers a
scan. Pending floods return control without starving scans. Failed runtime publication retains pending scan work.

`Runtime::apply_input_scan` stages/validates replacement and reserves publication before mutating live inventory. Input
records share the existing state cursor, snapshot/bootstrap, bounded subscriptions and removal commits. Identical scans
do not publish timestamp-only churn. Complete membership with partial metadata updates known endpoints; incomplete
membership retains the previous authoritative catalog and publishes degraded discovery status. Readers must inspect
that status rather than treat retained records as a newly verified complete scan. A complete empty scan removes inventory.

## Build and tests

Linux builds require `pkg-config` and libudev development files (`libudev-dev` on Debian/Ubuntu, provided by `systemd` on
Arch). `Cross.toml` installs target-architecture libraries inside each existing GNU cross image and selects its pkg-config
directory; runner libraries cannot substitute for AArch64 libraries. Windows/macOS compile the portable model/service and
mock tests without Linux bindings. When linked by a host, the adapter requires the system libudev runtime library.

Tests use synthetic metadata, injected permission/race failures, temporary sysfs-like trees, a mocked notification source,
the real runtime/state store and `ManualClock`. They require no physical devices, input access, udev rules or sleeps:

```sh
mise run test -p kbctl -E 'test(input::) | test(platform::) | test(runtime::input::) | test(metadata_identification_and_topology)'
```

These tests establish metadata and lifecycle behavior, not interoperability with a physical keyboard. Hardware status
protocols and USB/HID management discovery remain separate work. [Host indicators](host-indicators.md) and the
[read-only CLI host](cli.md) compose this service without changing its metadata-only boundary.
