# Testing a backend without hardware

Issue [#10](https://github.com/noirbizarre/kbctl/issues/10) provides reusable infrastructure under
[`src/test_support/`](../src/test_support/mod.rs). The entire module is `#[cfg(test)]`, private to the existing binary.
No library crate, executor, hardware access, production backend or transport adapter is introduced.

Use the actual `Topology`, `ConnectContext`, transport leases, shared `Session` and normalized runtime in tests.
The helpers supply controlled inputs to those implementations rather than substituting a second lifecycle model.

## Choose the right layer

| Test | Tool |
|---|---|
| Metadata identification and ambiguity | Registry fixtures; no device I/O |
| Framing/parser and command encoding | Scripted serial/HID controller plus the protocol driver |
| Lifecycle/capability contract | Shared conformance assertions plus a backend-specific recipe |
| Observation ordering, freshness and snapshots | Fake protocol → shared session → normalized runtime |
| Broken adapter contract | A small local adversarial double, such as the existing transport `ProbeIo` |
| Actual device interoperability | Supplementary, [opt-in hardware tests](hardware-testing.md) |

Synthetic protocol behavior is not evidence that GMMK Pro, Defy or Naya Create supports an operation. Real identifier
evidence remains in the [hardware matrix](architecture/devices.md).

## Devices, clocks and polling

`DeviceBuilder` builds real model specifications with explicit physical/group aliases and interface/endpoint/component
aliases. Group membership and routes are declared, never inferred from names or serial values. It supports split
physical members, controller children, multiple HID collections and discovery-only units with no endpoints.
`build()` rejects duplicate/unknown aliases, mismatched specification counts and cross-keyboard routes.

`DeviceFixture::revalidate()` explicitly establishes ready attachment generations. Its `context(topology, primary,
selected)` resolves aliases and calls the actual context constructor, including when the topology has moved into a
runtime. Opening leases likewise takes the current topology; stale or missing generations do not get fabricated.

`ManualClock` implements the shared monotonic `Clock`. `time(seconds)` returns an origin-relative instant;
`at(seconds)`, `advance(duration)` and negative-test `rewind(duration)` change it. Time changes do not wake tasks.
Deadline tests advance time and explicitly repoll, preserving the host scheduling contract.

`poll_once` performs one forced poll. For wake assertions, poll with your own `WakeCounter`/`Context`. `ready` drives
only immediately-woken work with a bounded budget; `ready_with_budget` can set a larger explicit limit. Neither sleeps
or spins blindly through `Pending`. A stalled future fails with instructions to release scripted data/gates or repoll
after clock advancement, exposing missing-wakeup bugs instead of hiding them.

## Scripted transport

`Controller` owns script state; an owned, non-cloneable adapter implements the real `TransportIo`. `pair` returns an
adapter/controller for direct tests. `LeaseFactory` opens selected registered controllers through real `TransportLease`
and `LeaseSet` values, retaining a shared cooperative reservation. It rejects competing opens through that factory and
records typed open/close/drop/release order. This is not OS/process exclusion or a substitute for the future host lock.

### Operations and lanes

`Script` contains named steps, declared gates and explicitly idle operation tails. Supported operations are serial
read/write, HID input/output and HID get/set feature. Each operation class has an ordered lane; read pending must not
block a write, or vice versa. Named gates express cross-lane dependencies. For example, an expected write can release
a gate required by its response. Gate release is idempotent, can happen before polling, and wakes registered affected
waiters after unlocking. Independently pending operations retain independent latest wakers.

Input injection, gate release and direction-specific faults are explicit controller actions. Scripts do not contain
code, expressions, timers or a hidden scheduler. Declaring gates validates references, not eventual progress: a test
must release its manual gates. Rust-only toy adapters can install reactions to fully accepted known commands; the
generic transport itself knows no toy opcodes, and JSON cannot install callbacks.

### Serial streams

- Reads preserve unread bytes, can coalesce adjacent ready chunks and return no more than buffer/chunk limits.
- Input gates, faults and EOF are boundaries. Truncated bytes without EOF remain pending; they are not magically a frame.
- Expected output tracks a cursor. The writer may offer a matching prefix of the remaining logical command, and a
  short acceptance advances only by the accepted count. Every offered byte is checked, including unexpected trailing data.
- An explicit acceptance count cannot exceed offered/remaining data. `Pending` accepts nothing. A scripted zero write
  is zero progress, not completion; the toy driver fails instead of retrying forever.
- Explicit read EOF persists for that channel; terminal disconnect affects later operations too. One-shot faults are
  separately declared. Reconnect normally installs fresh controllers and attachment generations.

An input step's chunk limit or `set_read_limit` forces fragmentation. `set_write_limit` can limit progress; explicit
per-step acceptance counts give precise replay. Trace entries distinguish offered data from actual accepted bytes.

### HID reports

HID input reports are atomic, not serial chunks. Do not coalesce them or return a tail as a second report. The fake
rejects undersized buffers and partial-report script outcomes. Explicit output/feature report arguments are preserved
separately; HID input contains exactly the supplied bytes. These conventions do not establish a universal OS adapter
report-prefix or padding rule. Match the eventual adapter contract when adding real protocol tests.

### Strictness and completion

An empty declared idle tail returns `Pending`; exhausting a finite strict lane unexpectedly is a sticky script failure.
Unexpected operation/report/output errors retain their original diagnostic and recent bounded transcript across later
polls. Malformed protocol bytes, by contrast, are valid transport input that the driver must reject.

Use `assert_complete()` after a successful exchange. It catches unconsumed steps and any sticky mismatch. Deliberate
cancellation uses `assert_cancelled_at(checkpoint)` after close; static fixture step IDs are stable checkpoints.
Dynamically appended steps have generated suffixes. Never assert script completion by panicking from `Drop`: a
cancelled operation legitimately leaves work and must retain its original failure. Lease cleanup is a separate
`LeaseFactory::assert_released()` assertion, checking every I/O owner drops before its matching reservation.

The transcript records operations, report IDs, offered/transferred bytes, progress, gates, errors and close/drop.
Repeated pending noise is coalesced and the recent transcript is bounded. Fixture labels and mutation descriptions
travel into failures so a corpus variant can be reproduced.

## Versioned fixtures

The [v1 corpus and schema](../tests/fixtures/backends/v1/README.md) use strict Serde DTOs with unknown-field rejection.
All checked-in fixtures are explicitly synthetic. The loader also defines captured-trace provenance requirements,
but metadata validation does not sanitize byte content or prove hardware support.

For an existing fixture in a private unit test:

```rust,ignore
let fixture = crate::test_support::fixture::load("firmware")
    .fragment("main", 1)?
    .short_write("main", "read-request", vec![1, 1])?;
fixture.require_protocol("kbctl.synthetic.serial", "1")?;
let controller = fixture.bind("main")?;
// Supply this controller to the real lease factory and backend-specific case.
```

Mutation helpers return a copy, preserve the base fixture ID and annotate failures. They support fragmentation,
safe input coalescing, short writes, byte replacement, truncation and EOF/disconnect after a checkpoint. Coalescing
does not cross gates/other operations. Removing all bytes from a serial-input step models silence, not EOF.

Add a fixture by recording honest device/revision/firmware/protocol labels, origin and purpose, defining explicit
endpoint/gate aliases, and registering its `include_str!` entry in `fixture::CORPUS`. The corpus test parses all entries
and checks that every synthetic v1 JSON file is registered. Keep old schema/protocol versions explicit when evolving
the format; do not silently reinterpret historical bytes.

## Shared conformance groups

[`test_support::conformance`](../src/test_support/conformance.rs) supplies assertions; the backend supplies recipes:

| Contract | Recipe responsibility | Shared groups |
|---|---|---|
| `BackendFactory` | Fresh scenario case, expected support/reporter, fresh replacement channels | `lifecycle`, `capabilities` |
| `ReadFactory` | Chosen operation, explicit-deadline execution, expected normalized result/interfaces | `reads`, `cancellation_and_timeouts`, `failures`, `reconnect` |
| `EventFactory` | Deterministic event injection and read completion recipes, expected event | `events` |

`Case` holds the actual topology/backend/candidate, explicit primary/endpoints/routes, optional component and explicit
capability scope, controllers, clock, lease factory and configurable bounds. Keyboard-only metadata backends can use
lifecycle/capability groups without inventing a component or implementing a read adapter. Execution groups require
their real operation recipe; missing scenarios must fail, not quietly return success.

Shared assertions contain no toy opcodes or mandatory battery method. Both battery-only and firmware-only adapters
register the same groups in [`backends/conformance_tests.rs`](../src/backends/conformance_tests.rs), preserving the
absence of unrelated interfaces. Unknown and explicit unsupported operation cases are separate. Deadline checks include
expired-before-admission/no-I/O and the exact expiry boundary after admission. Reconnect uses fresh generations and
rejects old deliveries. Success cases consume their scripts; cancellation cases have explicit remaining checkpoints.

Use those registrations as the complete adapter example. Keep protocol recipes outside shared assertions and retain
specialized normalized-state tests for field ordering, lifecycle, expiry and subscription recovery.

## Running checks

For helper/conformance development:

```sh
mise run test -p kbctl -E 'test(test_support::) | test(backends::conformance_tests)'
```

Before handing off changes, select the affected backend/model/runtime regressions too, and run formatting, Clippy,
Markdown and spelling checks per [CONTRIBUTING](../CONTRIBUTING.md#before-you-push). `lint` checks both normal and test
targets; root helper registration is `#[cfg(test)]`, so support code and corpus are absent from production builds.
Hardware tests are optional supplementary evidence, never a replacement for this deterministic suite.
