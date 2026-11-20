# ADR 0003: Versioned session-bus object API

Status: Accepted design; implementation pending.

## Context

Desktop consumers need discovery and live status without owning hardware. D-Bus provides per-user service discovery,
typed interfaces, signals and owner-change notification. Its standard ObjectManager does not by itself define an
atomic, globally revisioned application event stream.

## Decision

Use the session bus with well-known name `io.github.noirbizarre.Kbctl1` and root path
`/io/github/noirbizarre/Kbctl1`. The initial daemon is explicitly started; calls do not rely on automatic activation.
The following is the intended contract shape, not a shipped API or complete introspection schema:

```text
/io/github/noirbizarre/Kbctl1                         Manager + ObjectManager
/io/github/noirbizarre/Kbctl1/devices/d1              Device
/io/github/noirbizarre/Kbctl1/devices/d1/components/c1 Component [+ Battery, Firmware]
/io/github/noirbizarre/Kbctl1/devices/d1/components/c2 Component [+ Battery]
```

All application interfaces use prefix `io.github.noirbizarre.Kbctl1`, for example `.Manager`, `.Device`, `.Component`,
`.Battery` and `.Firmware`. Export standard `org.freedesktop.DBus.Introspectable` and `.Properties` as applicable,
and `org.freedesktop.DBus.ObjectManager` at the root. Base status properties are read-only.

| Interface | Initial responsibility |
|---|---|
| Manager | Service epoch/API metadata; `GetSnapshot` and the coherent `StateChanged` signal |
| Device | Runtime ID, display identity, identity confidence, availability and capability-support metadata |
| Component | Runtime ID, owning device/optional parent component, role, availability and capability metadata |
| Battery | Known percentage/charging observations, quality, last-success time and failure reason |
| Firmware | Verified version observations and their quality/availability |

Use typed fields with explicit validity/quality for optional values; D-Bus has no general nullable basic type.
Do not encode missing percentages with zero or pass opaque JSON instead of defining types. The implementation must
write introspection signatures before exposing API v1, following this vocabulary and the consistency rules below.

### Discovery and coherent observation

ObjectManager's `GetManagedObjects`, `InterfacesAdded` and `InterfacesRemoved` describe the exported objects and
capability interfaces. `PropertiesChanged` describes existing-property changes, including invalidated values that
must be fetched again. Permanent instance removal retires the object; temporary inaccessibility changes status.

For standard object discovery, install signal matches before `GetManagedObjects` and reconcile buffered updates.
Resolve and pin the service's unique bus owner, monitor `NameOwnerChanged`, and discard cached objects/buffers if
that owner changes during bootstrap or normal operation. Follow the [D-Bus ObjectManager specification][objectmanager].

Clients needing one consistent application view use `GetSnapshot` and `StateChanged` as specified in
[ADR 0004](0004-state-and-events.md). Snapshot records use the same object paths and property meanings as the standard
interfaces. Both exports derive from the same committed state; adapters do not poll hardware independently.
Clients choose the coherent stream or standard property caching for a view, rather than applying both as independent
updates. Standard interface signals are not a cross-object transaction boundary.

### Versioning and failures

The `1` is the major API version in the service name, interface names and root path. Compatible additions may add
properties, methods or optional capability interfaces. Changing existing types, meanings or removing members requires
a new major version; clients tolerate unknown additive fields/interfaces and extensible status values.
The package version is not the API version. Runtime IDs and cached paths do not survive an epoch/owner change.

Use stable application error names under `io.github.noirbizarre.Kbctl1.Error`, such as `.Busy`, `.Unavailable` and
`.ResyncRequired`, with actionable descriptions. Use standard D-Bus errors for standard failures such as unknown
objects or invalid arguments. Distinguish a failed request from an individual device's failed observation: one
inaccessible keyboard does not fail a snapshot of all devices. CLI mapping preserves the cause and recovery action
using typed `kbctl::<module>::<kind>` diagnostics; do not expose raw packets or privileged handles.

## Alternatives and trade-offs

- **Custom Unix socket/RPC:** permits arbitrary framing, but requires bespoke discovery, authorization, introspection
  and desktop client tooling. D-Bus's existing ecosystem better matches local desktop consumers.
- **CLI subprocesses:** convenient for one-shot scripts, but live clients would repeatedly poll and parse presentation
  output. Structured CLI output can remain useful without becoming the daemon transport.
- **System-bus service:** a system-wide owner could coordinate users, but adds privilege/access policy beyond this
  per-user application. The session API does not grant hardware privileges.
- **Only standard property signals:** familiar and sufficient for simple clients, but no coherent global baseline or
  revision-gap detection. A small revisioned contract serves clients that need those guarantees.

## Consequences and follow-up

Clients can discover supported capabilities and reconnect without vendor knowledge. A coherent stream adds a small
application contract beyond standard interfaces; implementation tests must ensure both exports describe the same
commits. Exact signatures, selected Rust bindings and service activation packaging are deferred until implementation.

[objectmanager]: https://dbus.freedesktop.org/doc/dbus-specification.html#standard-interfaces-objectmanager
