# Architecture

KBCTL is a unified control plane for programmable keyboards. Its initial focus is discovery and status:
which keyboards and components are present, whether they are accessible, and what state their existing firmware
can safely report. Firmware, battery and active runtime state are optional observations, not universal promises.

**Implementation status:** these decisions define the target architecture for [issue #4][issue]. The current Rust
binary parses a placeholder command and performs no device operations. No backend, daemon or D-Bus API exists yet.
Examples and module names below describe planned behavior.

## Goals and boundaries

- Offer standalone CLI operation and daemon-backed access to the same application behavior and state model.
- Support simple, split and modular devices without equating a physical component with a USB interface.
- Normalize reliable observations while retaining uncertainty and partial failures.
- Begin with Linux discovery, device permissions and a session-bus API; keep domain logic independent of Linux.
- Work with existing firmware and coexist operationally with vendor configuration tools.

Firmware flashing, configuration-tool replacement, layout editing, the future layout engine and graphical overlay
are outside this design. The initial interface is observational: runtime controls may be added later as explicit,
capability-gated operations. There is no raw protocol passthrough or keystroke collection.

Linux is the initial runtime target. Portable core boundaries leave room for other platforms without promising
device or daemon support there. Existing cross-platform build/test scaffolding is not a support claim.

## Responsibilities and dependency direction

```text
CLI rendering and mode selection           Desktop clients
       | direct            | daemon              |
       |                   +---- session D-Bus ---+
       |                                |
       |                      D-Bus adapter / user daemon
       |                                |
       +---------- shared application service ---+
                              |
                 domain state, identity, lifecycle
                              |
                   protocol backend boundary
                              |
                 transport endpoints (HID, serial)
                              |
                    Linux discovery and access
```

| Boundary | Responsibility | Must not own |
|---|---|---|
| CLI | Argument interpretation, mode selection, human/structured rendering, exit status | Protocol decoding or duplicate lifecycle rules |
| User daemon | Foreground process lifetime, ownership, discovery tasks, session-bus export | Additional device privileges or desktop policy |
| Application service | Operations, committed snapshots, event ordering, lifecycle and capability policy | CLI formatting or D-Bus wire types |
| Domain model | Devices, components, identities, capabilities and observation quality | OS handles and vendor packet formats |
| Protocol backend | Recognize supported firmware, select safe queries, decode and normalize observations | Presentation, bus export or global ownership policy |
| Transport | Bounded I/O, endpoint opening/closing, connection errors | Meaning of battery or layers |
| Linux integration | Enumeration/hotplug, node metadata, permissions and attachment handles | Vendor protocol semantics |
| Desktop integration | Consume public status, apply desktop-specific presentation/policy | Direct hardware access through the daemon API |

Dependencies point toward shared behavior, with platform/backend implementations supplied at application startup.
The daemon and direct CLI instantiate the same service. A daemon-backed CLI is a client and opens no hardware.
The core serializes state commits; unrelated hardware I/O can proceed concurrently.

## Single binary, private modules

Remain one Cargo package and one executable. Introduce private modules only as implementation needs them:

```text
src/main.rs           startup, dependency wiring, diagnostic rendering
src/cli.rs            argument types only (also consumed by build.rs)
src/error.rs          typed actionable diagnostics
src/app/              application operations, snapshots and lifecycle
src/model/            device/component/capability vocabulary
src/backends/         protocol-specific recognition and queries
src/transport/        HID and serial I/O boundaries
src/platform/linux/   discovery, permissions and ownership locking
src/daemon/           foreground lifetime and D-Bus adaptation
```

This is a boundary sketch, not a requirement to create empty modules. No `lib.rs`, workspace split, external
plugin ABI, async runtime or specific device/D-Bus crate is selected here. Diagnostic codes retain the repository's
`kbctl::<module>::<kind>` convention; errors identify the failed operation and a concrete recovery action.

## Decisions

| ADR | Decision |
|---|---|
| [0001: Execution model](adrs/0001-execution-model.md) | Shared service in a single binary; explicit modes and per-user ownership |
| [0002: Device model](adrs/0002-device-model.md) | Separate devices, components, endpoints, backends and capabilities |
| [0003: Session D-Bus API](adrs/0003-session-dbus-api.md) | Versioned object model and typed capability interfaces |
| [0004: State and events](adrs/0004-state-and-events.md) | Revisioned committed state, bounded live delivery and snapshot resynchronization |
| [0005: Linux device access](adrs/0005-linux-device-access.md) | Unprivileged processes, narrowly scoped access and safe status queries |

The [hardware evidence matrix](devices.md) applies these boundaries to GMMK Pro, Defy and Naya Create.
It distinguishes source evidence from proposed mappings and unresolved implementation work.

## Implementation sequence and verification targets

1. Implement domain snapshots and lifecycle transitions with simulated backends; verify unknown/stale values,
   attachment generations, partial components and snapshot/event ordering.
2. Implement Linux discovery and direct status for one verified firmware/transport combination. Test permission
   failures, bounded safe probing and reconnects on hardware before advertising that combination as supported.
3. Host the same service in the user daemon, add the versioned session API, and compare direct and remote snapshots.
   Verify startup races, bus-owner changes, slow consumers and restart resynchronization.
4. Extend verified device/firmware coverage; add desktop consumers independently of protocol backends.

These are future implementation checks. This documentation change does not demonstrate hardware compatibility.
The existing systemd user-unit file is a template placeholder; its current command does not start a daemon.

[issue]: https://github.com/noirbizarre/kbctl/issues/4
