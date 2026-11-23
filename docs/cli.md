# CLI guide

The read-only CLI runs directly on Linux, with no daemon required. Its application operations compose the existing
input discovery, registry and normalized indicator services. Other operating systems report an unsupported-platform
diagnostic. Management protocol backends and D-Bus integration are separate work.

## Commands

```sh
kbctl device list
kbctl device list --all
kbctl device inspect gmmk-pro
kbctl indicator status
kbctl indicator status --device dygma-defy
kbctl --verbose indicator status
kbctl indicator watch
kbctl status
kbctl --json device list
kbctl indicator watch --json
```

The examples are illustrative; copy a selector actually printed by `device list`. Reviewed model labels may include a
brand or variant, such as `glorious-gmmk-pro` or `dygma-defy-wired`.

`device list` shows a compact NAME / SELECTOR / PATH table, grouping sibling endpoints rather than repeating kernel nodes.
The path is a current locator; `(+N)` counts additional endpoints. Defaults include positively recognized typing keyboards,
including virtual typing keyboards, and hide partial/other/unknown devices. `device list --all` shows every input group
with explicit classification and placement labels; inspect can still address any group. Narrow terminals use stacked
rows, keeping selectors complete for copying. For example:

```text
NAME        SELECTOR     PATH
GMMK Pro    gmmk-pro     /dev/input/event0 (+1)
Dygma Defy  dygma-defy   /dev/input/event2 (+1)
```

`--verbose` (or `-v`) expands list/status/watch with endpoint and metadata diagnostics. `device inspect` is always detailed,
with identity evidence, grouping/runtime identity, endpoint locators, classification, kernel identifiers and capabilities
in named sections. It accepts readable handles, qualified aliases and legacy technical selectors. Exact sysfs/event-node
paths remain accepted for transient inspection, for example `kbctl device inspect /dev/input/event0`. They are not
persistent selectors. Full display names and process-local runtime IDs are not accepted as selectors.
Missing or ambiguous selection is an actionable error rather than selecting the first match.

### Keyboard classification and placement

Classification is deterministic discovery/domain logic, independent of names and vendor lists:

| Class | Evidence and inclusion policy |
|---|---|
| `keyboard` | Complete ordinary typing capability profile; included by default |
| `partial_keyboard` | Positive limited typing, keypad, function or macro-key subset; visible under `--all` and inspect |
| `other` | Known non-keyboard role or no typing-key evidence; diagnostic inventory only |
| `unknown` | Missing required capabilities, malformed or contradictory evidence; diagnostic inventory only |

The typing profile requires readable EV_KEY and all three alphabetic rows, number-row digits, Space/Enter/Backspace/Tab,
and Shift/Ctrl/Alt modifier groups (either side). It does not require a physical full-size layout, numpad, function row
or LED support. Power/sleep/brightness/volume/media keys and pointer/controller buttons alone do not qualify.
Udev role tags corroborate the bitmaps; a positive keyboard tag cannot override unreadable or contradictory capabilities.
Missing tags are not negative evidence. Inspect explains the decision and its coverage counts. See the
[discovery guide](input-discovery.md#bitmap-abi-and-keyboard-classification) for exact policy and limitations.

One positively recognized typing endpoint establishes a keyboard attachment, preserving media/pointer siblings as their
own endpoint classes. Separate partial interfaces are not unioned into a fictional complete typing interface. This
classification does not create management topology or prove that a shared receiver represents one keyboard chassis.

Origin (`physical`, `virtual`, `unknown`) is separate from placement (`internal`, `external`, `unknown`). Virtual ancestry
or the virtual input bus is labeled virtual even if other metadata advertises USB; physical location remains unknown.
AT/i8042 ancestry on a portable chassis can support an internal label. USB removability must account for the upstream hub
chain: a fixed child of a removable dock is external, while an entirely fixed chain to the root supports internal placement.
USB bus type, an AT product name or missing placement metadata alone never decides built-in/external location.

Default list, indicator status/watch and global status use the recognized-keyboard scope in both human and JSON output.
Partials are not silently mixed in. Copy their selector from `device list --all` to explicitly query supported observations
with `indicator status --device <selector>`. Other/unknown selections remain inspectable but report `not collected` for
indicators. Healthy discovery with zero recognized keyboards is distinct from an empty raw input inventory; use `--all`
for diagnostic visibility. Filtering never removes discovery records or hides selector collisions with excluded inputs.

### Selector evidence and persistence

- **Display name** is a label for reading; it can change and need not be unique.
- **User selector** is an inventory-aware handle for selecting a current grouped attachment. It is not hardware identity.
- **Hardware evidence** includes model metadata, USB IDs, optional/nonunique advertised serial and attachment topology.
- **Technical/runtime identifiers** include the existing evidence selectors, process-local IDs and current event/sysfs paths.

Readable model bases prefer a reviewed registry model label, then USB product metadata. A positively internal physical
typing keyboard without model metadata gets `internal-keyboard`. Otherwise a deterministic raw kernel-name fallback is
used, preferring typing then partial endpoints and ordering names independently of event numbers. Kernel-name fallback
is explicitly weak; changing that label can change the handle. Rendered display labels are never used as identity input.

Normalization lowercases ASCII letters/digits, collapses other characters to one hyphen, trims separators, and limits the
slug to 40 ASCII characters. It does not transliterate Unicode; original names remain visible. Punctuation/Unicode-only
or missing text falls through to another source, then a generic class/origin basis such as `keyboard`, `virtual-keyboard`,
`keypad` or `input-device`. Generic fallbacks always use a qualified selector. Double hyphens are reserved for generated
qualifiers, and the old full `usb-*`/`attachment-*` evidence-ID namespaces cannot become ordinary model aliases.

- A unique model gets a bare handle such as `gmmk-pro`. This is **inventory-relative**: adding a second identical model
  makes the bare handle ambiguous. Matching considers the full inventory, including hidden input groups.
- Every group also has a qualified readable alias, available under `--verbose`, inspect and JSON even while its bare handle
  is unique. Duplicates use that qualified form in the default SELECTOR column.
- `gmmk-pro--a1b2c3d4` uses the model basis plus uniquely observed namespaced advertised serial evidence. The token reuses
  the legacy USB digest described below. It stays usable across reconnects/port moves/renumbering while model and evidence
  remain unchanged and the suffix remains unambiguous; this is not verified unit identity or proof of global uniqueness.
- `gmmk-pro--at-a1b2c3d4` explicitly uses current attachment topology when serial evidence is missing, blank, failed or
  duplicated. It is **not** a persistence guarantee; the same port can later contain another device. Internal laptop
  keyboards frequently have this weaker qualified alias even when the unique `internal-keyboard` role handle is convenient.
- Tokens start at eight hex characters and extend through 12, 16, 24 and 32 characters when needed to avoid prefix
  collisions. Full-token collisions remain explicit ambiguity, never first-match selection or runtime-ID/order suffixes.
  Adding a colliding device can extend a qualified alias; model/registry-label changes can also change readable handles.
- Querying an ambiguous base fails with names, qualified readable candidates, stability and transient inspection locators.
  There is no fuzzy display-name matching, persistent configuration or user-managed alias database.

### Legacy selectors and script migration

Existing technical selectors retain their values and continue to resolve through the same service:

- `usb-<32 hex digits>` uses the USB vendor/product IDs and a nonblank **advertised** serial. It stays the same across
  disconnect/reconnect, port changes and event-node renumbering while that evidence remains the same. It is not verified
  unit identity, evidence of ownership, or permission to pair devices. Case and spaces in the serial are significant.
- `attachment-<32 hex digits>` is the explicit fallback when USB/serial evidence is absent, blank or failed. It derives
  from the current group sysfs locator and has **no reconnect-stability or hardware-uniqueness guarantee**. The same path
  can later refer to another device. A fallback is also available for every advertised selector via inspect/verbose output.
- Duplicate advertised identities remain separate attachments with an ambiguous evidence ID. Use the qualified readable
  topology alias or technical attachment fallback for a current query. Duplicate IDs never silently resolve to one device.
  Ambiguity is relative to the current inventory; incomplete discovery cannot establish global uniqueness.

Legacy selector derivation v1 is unchanged: SHA-256 over `kbctl/selector/v1/usb` followed by a zero byte, then three fields,
each prefixed with its unsigned 64-bit big-endian byte length. Fields are the two-byte big-endian vendor ID, two-byte
big-endian product ID, and exact UTF-8 serial. The first 16 digest bytes become lowercase hex after `usb-`. Fallbacks use
the namespace `kbctl/selector/v1/attachment` and one field containing the current locator's OS-encoded bytes, prefixed
in the same way. Runtime IDs, names, event-node paths and enumeration order never enter the advertised selector.

Scripts using `selector.id` need no field migration. New integrations should use `user_selector.id` for the displayed
handle or `user_selector.qualified` for evidence-qualified selection, observing its stability/ambiguity metadata. Keep the
legacy USB alias when selection must remain independent of changing model descriptions; its advertised evidence can still
be absent or nonunique. Schema-v1 `id` remains a process-local runtime ID, not the readable selector.

Static shell completions and help describe SELECTOR arguments with non-path value hints. They do not enumerate connected
devices or run discovery; completion support follows each shell generator. Transient inspection paths still parse normally.

Device commands use metadata only: event-stream access is **not checked** and no indicator node is opened. Indicator
commands collect from positive typing endpoints and justified LED-reporting siblings in a confirmed keyboard attachment;
LED capability alone on an unrelated device is insufficient. Explicit partial selection uses the selected group's scope.
Global `status` combines discovery, devices, grouped indicators and an explicit
management summary; it does not start management-backend sessions or claim that device discovery creates logical
programmable keyboards.

## Indicator scope and ownership

Each endpoint independently reports Caps Lock, Num Lock and Scroll Lock. Endpoints can disagree. Kernel-maintained LED
state does not establish desktop-global lock state, physical LED illumination or firmware state. There are no writes.

Default human output gives a separate view for each eligible input attachment, with `on`/`off` only when all applicable
endpoints have fresh, known and agreeing values. Conflicting known values show `disagreement`. Known readings alongside
unknown, unavailable, uncollected, stale or unsupported siblings show `mixed`, with contributing states. Uniform uncertain
or failed states retain their names. `--verbose` shows the endpoint values, availability and freshness behind each summary.
An explicitly selected device without eligible sources reports `not collected`, not `off` or `unsupported`.

Groups are the existing discovery attachments, **not verified keyboard chassis**: a USB receiver can expose several
devices, and unreadable grouping evidence remains uncertain. KBCTL labels this scope and never merges independent
attachments by names or serials. `--device` on indicator status selects the same canonical/fallback ID shown by device
discovery, restricting both collection admission and human/JSON results to the selected eligible group; current paths and
names are not accepted for this option. Selection resolves against the full inventory before any indicator-node open.

Known values, unknown values, unsupported locks, unavailable sources, permission denials, errors and stale observations
are explicit. Read the lock's `state`, `quality`, source availability, `failure` and `source_failure` alongside `value`.
An unavailable source can have no first report at all; its failure is still reflected in each lock's state. `last_known`
is historical evidence, never a replacement for a current unknown or unavailable value. A stale report can retain its
boolean `value`, with explicit stale validity and cause. A permission failure is never encoded as `false`.

Indicator commands acquire the shared per-user ownership lock before opening input endpoints. If another KBCTL process
owns it, the command fails with `kbctl::ownership::busy`; it never opens competing hardware or bypasses ownership.
`XDG_RUNTIME_DIR` must satisfy the [ownership storage contract](host-indicators.md#ownership-and-checked-opens).
The CLI does not yet route requests to an owner over D-Bus.

Watch processes hotplug, collection readiness, retries and freshness deadlines. It prints an initial full indicator view
and subsequent changed views, including timestamp/freshness renewals. It closes leases and retires sources on Ctrl-C,
SIGTERM or output failure. Snapshot fallback samples periodically and can miss changes between samples.

## Output contract (schema version 1)

Human output uses concise command-specific views, restrained terminal color and explicit state words; diagnostics remain
understandable without color. `--verbose` adds detail; `RUST_BACKTRACE` expands errors without changing successful output.
`--json` is global and may appear before or after a subcommand. It retains full metadata for the query's visible devices,
regardless of verbosity. Use `device list --all --json` for the complete raw input inventory.
JSON rendering uses
[`console-json`](https://github.com/noirbizarre/console-json); that library owns terminal detection and color policy.

- Terminal: pretty JSON, with colors when the library detects support.
- Pipe or redirected stdout: compact uncolored JSON, regardless of forced-color environment settings.
- One-shot command: one JSON document and a trailing newline.
- Watch: one initial `snapshot` document, then `update` documents; `resync` means a subscription was rebuilt after
  overflow or an epoch change. Every document contains a full replacement view. When piped, this is NDJSON, flushed
  after each record. Interactive pretty documents span multiple lines.

Every document contains `schema_version: 1`, `command` (for example `device.list`), and `event`. Successful documents
contain `cursor`, `time_domain` and `data`. Cursors contain an opaque hex epoch and revision; they order state within a
running service, not independent CLI invocations. All `*_ms` timestamps are monotonic milliseconds from runtime startup
in the named `time_domain`, not Unix timestamps. Collections of devices/endpoints/sources are ordered by sysfs path.
Treat runtime IDs, selectors and epochs as separate opaque strings. Unknown/missing metadata is a tagged object, never an
unexplained null.
Nullable report, value and last-known members indicate absence within an explicitly tagged lock/source state.

Command data members:

| Command | Data |
|---|---|
| `device.list` | `inventory`, `discovery`, `devices`, `problems` |
| `device.inspect` | `inventory`, `discovery`, `device`, `selected_endpoint_id`, `problems` |
| `indicator.status`, `indicator.watch` | `inventory`, `discovery`, `indicators`, `problems` |
| `status` | `inventory`, `execution`, `discovery`, `devices`, `indicators`, `management`, `problems` |

`inventory` describes query visibility (`keyboards`, `all_inputs` or `selected_device`), total raw input groups, recognized
keyboard groups and shown groups. An empty default list can coexist with many non-keyboard inputs. `discovery` separates
inventory and monitor health; degraded discovery is not evidence of an empty host. `indicators.scope` is
`per_endpoint_kernel_state`; `collection` is `observed` or `no_sources`. An empty source list is interpreted together
with discovery health. Each source has endpoint locators, scope, availability and three named entries under `locks`:
`caps_lock`, `num_lock`, `scroll_lock`. Field reports retain provenance, sequence, validity and freshness deadlines.
`problems` preserves collector diagnostics, including initialization/fallback warnings; it describes the most recent
host turn (all initial collection turns for a one-shot query). Continuing source/metadata failures also live in state.

Device records include separate `display_name`, readable `user_selector`, and legacy `selector` metadata (`id`, `kind`,
`verification`, `ambiguous`, `fallback`, `fallback_ambiguous`, `reason`) and `grouping`.
`usb_serial` means advertised evidence; `attachment` means locator-scoped fallback.
`grouping` is `usb_attachment`, `input_attachment` or `uncertain_attachment`,
never a claim of chassis identity. Existing numeric-string `id` fields remain process-local.

`user_selector` contains primary `id`, base `slug`, always-present `qualified` alias, nullable `bare_alias`, `bare_ambiguous`,
`name_source`, `disambiguation`, `stability`, `qualified_stability`, `ambiguous` and `qualified_ambiguous`. Generic or
reserved aliases may have no bare form. Ambiguity flags refer to each form; exact resolution still checks every accepted
alias. Stability is `inventory_relative`, `model_and_advertised_serial`, `label_and_advertised_serial` or `attachment_scoped`.
Naming sources are `registry_model`, `usb_product`, `internal_placement`, `kernel_name` or `generic`.
`model_label` and tagged raw `product` metadata preserve the naming inputs without overloading display/runtime fields.

Group records and grouped indicator summaries add `classification.kind`, `origin` and `location`. Endpoint records retain
the schema-v1 `keyboard` boolean/unknown projection and `classification_property`; richer `classification` provides `kind`,
`reason`, confidence and `typing_coverage`, while `input_properties` and `placement.evidence` retain raw tagged metadata.
Placement includes its qualified reason. These additions preserve field shapes and selector derivation; the default query
scope is now keyboard-first, with the complete previous inventory accessible explicitly through `--all`.

`indicators.devices` adds per-attachment summaries, referencing `device_id` and contributing `endpoint_ids`, plus the same
name/user-selector/technical-selector/grouping fields. Each lock has `state`, nullable `value` and distinct
`contributing` state/value pairs.
Only unanimously fresh known observations produce a non-null summary boolean. Mixed, disagreement and other uncertain
summaries have null values; raw endpoint reports and history remain unchanged in `indicators.sources`. Existing schema-v1
fields and endpoint scope are preserved. Grouped summaries retain legacy technical-selector order, and existing device/
endpoint/source arrays retain sysfs-path ordering. Human rows follow readable-selector order.

Fatal failures produce `event: "error"` and an `error` object with stable diagnostic `code`, `message` and actionable
`help`, rather than `data`. For example, with unset runtime storage:

```json
{"schema_version":1,"command":"indicator.status","event":"error","error":{"code":"kbctl::ownership::storage","message":"unsafe ownership storage at XDG_RUNTIME_DIR: runtime directory is unset","help":"Set XDG_RUNTIME_DIR to an absolute, user-owned private directory (0700). Check the kbctl directory (0700) and regular, single-link owner.lock (0600); do not delete a live lockfile or use symlinks."}}
```

Future changes may add fields or enum cases; scripts should select named fields and handle unfamiliar explicit states.
Incompatible shape or semantic changes require a new schema version.

## Exit status and diagnostics

| Code | Meaning |
|---|---|
| 0 | Query completed, including faithfully reported degraded/unsupported/permission-denied observations; or clean watch/pipe shutdown |
| 1 | Fatal initialization, ownership, selection, runtime or output failure |
| 2 | Invalid command syntax, as reported by Clap |

Human fatal diagnostics go to stderr. In JSON mode, application/runtime error documents go to stdout with nonzero exit
status; stderr remains available for a failure to render the error itself. Clap's syntax errors/help retain their normal
text behavior. Watch can emit an error after earlier snapshots. Serialization or I/O failure can leave a partial last
document; consumers must also inspect the process exit status. A closed downstream pipe is a normal consumer shutdown.

## Verification

Command tests inject synthetic discovery, indicator leases, clocks, cancellation and output. Executable-edge tests
exercise help, syntax, structured setup errors, pipe presentation and ownership contention before any real discovery or
indicator access. No regular test needs a keyboard, root, `/dev/input` permissions or a live udev monitor.

```sh
mise run test -p kbctl -E 'test(cli::) | test(application::) | test(output::)'
mise run test -p kbctl --test cli
```
