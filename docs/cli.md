# CLI guide

The read-only CLI runs directly on Linux, with no daemon required. Its application operations compose the existing
input discovery, registry and normalized indicator services. Other operating systems report an unsupported-platform
diagnostic. Management protocol backends and D-Bus integration are separate work.

## Commands

```sh
kbctl device list
kbctl device inspect /dev/input/event0
kbctl device inspect /sys/devices/synthetic-usb
kbctl indicator status
kbctl indicator watch
kbctl status
kbctl --json device list
kbctl indicator watch --json
```

The sysfs example is illustrative; use a path actually printed by `device list`.

`device list` includes every discovered input group, its endpoints, classification evidence, registry identification,
metadata and discovery health. Non-keyboard groups and unknown classifications remain visible. `device inspect` accepts
an exact group sysfs path, endpoint sysfs path or known event-node path and returns the group context and, when an
endpoint was selected, its ID. Missing or ambiguous matches are errors. These paths are current OS locators and can be
reused after unplugging. IDs are process-local correlation values, not persistent selectors or hardware identities.

Device commands use metadata only: event-stream access is **not checked** and no indicator node is opened. Indicator
commands collect from known keyboard endpoints or endpoints advertising standard lock LEDs, following the existing
collector's candidate policy. Global `status` combines discovery, devices, per-endpoint indicators and an explicit
management summary; it does not start management-backend sessions or claim that device discovery creates logical
programmable keyboards.

## Indicator scope and ownership

Each endpoint independently reports Caps Lock, Num Lock and Scroll Lock. Endpoints can disagree. Kernel-maintained LED
state does not establish desktop-global lock state, physical LED illumination or firmware state. There are no writes.

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

Human output is an indented, labeled view of the same presentation facts used by JSON, including metadata and failure
details. `--json` is global and may appear before or after a subcommand. JSON rendering uses
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
Treat IDs and epochs as opaque strings. Unknown/missing metadata is a tagged object, never an unexplained null.
Nullable report, value and last-known members indicate absence within an explicitly tagged lock/source state.

Command data members:

| Command | Data |
|---|---|
| `device.list` | `discovery`, `devices`, `problems` |
| `device.inspect` | `discovery`, `device`, `selected_endpoint_id`, `problems` |
| `indicator.status`, `indicator.watch` | `discovery`, `indicators`, `problems` |
| `status` | `execution`, `discovery`, `devices`, `indicators`, `management`, `problems` |

`discovery` separates inventory and monitor health. An empty array with current inventory means no devices were found;
an empty array with degraded discovery is not evidence of an empty host. `indicators.scope` is
`per_endpoint_kernel_state`; `collection` is `observed` or `no_sources`. An empty source list is interpreted together
with discovery health. Each source has endpoint locators, scope, availability and three named entries under `locks`:
`caps_lock`, `num_lock`, `scroll_lock`. Field reports retain provenance, sequence, validity and freshness deadlines.
`problems` preserves collector diagnostics, including initialization/fallback warnings; it describes the most recent
host turn (all initial collection turns for a one-shot query). Continuing source/metadata failures also live in state.

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
