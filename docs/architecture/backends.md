# Backend contracts and lifecycle

The private [`src/backends/`](../../src/backends/mod.rs) and [`src/transport/`](../../src/transport/mod.rs) modules
implement the hardware-free contract for [issue #7](https://github.com/noirbizarre/kbctl/issues/7). Shared session
machinery enforces ownership lifetime, request cancellation, bounded delivery and result binding. Synthetic protocols
exercise that machinery. Issue #8 adds the separate [metadata registry](registry.md); no real keyboard protocol,
OS opener, daemon or CLI device command is implemented.
Issue #9 adds the [normalized runtime/state service](state.md) and source receipt/liveness integration described below.
Issue #10 supplies [shared test support and conformance](../backend-testing.md), with versioned synthetic fixtures.

## Boundaries

| Layer | Responsibility |
|---|---|
| Application host | Enumerate hardware, resolve registry candidates, acquire process ownership, open safe channels, schedule progress/timers and commit accepted state |
| Registry/vendor mapping | Source-backed identification, endpoint-role selection and proposed topology; real protocol references are research-only |
| `Backend` | Describe identity/update strategy, match metadata without I/O, and connect a selected candidate using transferred leases |
| Shared `Session` | Own channels, enforce lifecycle, serialize requests, validate replies/routes/bounds and publish backend observations |
| `ProtocolDriver` | Recognize known firmware, frame/correlate requests, decode replies and unsolicited reports, and interpret capability support |
| `TransportIo` | Nonblocking serial or HID operations on an already-opened handle, with local close/release |
| Domain model | Typed observations, capability reports and freshness tokens; no vendor packets or OS handles |

Vendor configuration and generic protocol parsing are separate. The same protocol can use a different mapping or
transport adapter without importing CLI formatting, D-Bus interfaces or desktop policy. The current binary only
registers these private modules; its placeholder command still performs no device operations.

## Discovery and selection

`DiscoveryView` exposes one host-discovered physical unit, its interfaces and endpoints through read-only accessors.
Its validated restricted view rejects duplicate/foreign endpoints and exposes only their associated interfaces.
A backend's `match_metadata` returns `NoMatch` or a `BackendCandidate` with backend/configuration identity, distinct
endpoint IDs and typed evidence. Evidence can describe USB model IDs, exact advertised serial, interface numbers,
transport kind and collection-local HID usage page/usage.

`MatchResolution` canonically sorts/deduplicates identical backend/configuration/endpoint ownership sets and merges
their evidence, while retaining distinct alternatives as ambiguous. Endpoint vectors are unordered ownership sets;
primary and named role bindings are explicit registry/context data. The [registry](registry.md) preserves role and
classification ambiguity beyond this lower-level aggregation and calls matchers only for justified enabled mappings.
No confidence score or registration-order choice is used. Backend selection is distinct from stable hardware identity;
unknown or ambiguous devices remain discovery facts rather than being sent exploratory packets.

Metadata matching performs no I/O. Active recognition is a bounded, allowlisted handshake after lease transfer.
Discovery is not a continuing connectivity guarantee: connection can fail because a device disappeared, access was
lost, the selected configuration was wrong or firmware cannot be safely recognized.

## Async contract and capability interfaces

`BackendFuture<'a, T>` is a boxed `Send` future using only standard-library futures/task types. `Backend`,
`ProtocolDriver`, `BatteryRead` and `FirmwareRead` are object-safe. Drivers/channels are `Send`; sessions are mutable
owners and need not be `Sync`. No runtime, worker thread or timer service is selected or spawned implicitly.

The shared `Session` is a concrete lifecycle host around a dynamic protocol driver. Its optional `battery()` and
`firmware()` accessors expose read interfaces. A battery-only protocol need not implement `FirmwareProtocol`, and a
firmware-only protocol need not implement `BatteryProtocol`. Common capability discovery is separate from those reads.
There is no monolithic keyboard interface with methods for every capability family.

Interface presence describes implementation, not support on every target. Before admitting a typed read, the session
validates its explicit component route and the driver's current scope-specific `CapabilityReport`:

- Unknown support produces `SupportUnknown`.
- Explicitly unsupported produces `Unsupported`.
- An unauthorized target produces `InvalidTarget`.
- Supported reads dispatch through the corresponding protocol interface.

The driver maintains its current interpreted support evidence; the topology cache is the application's published view.
If firmware/protocol context becomes unsafe or unrecognized, the driver must stop claiming support, fail/reconnect
as appropriate, and the host must invalidate published evidence. Merely having a write-capable serial handle does
not authorize configuration writes. Only battery/firmware read execution is implemented in this contract.

## Leases and ownership

The application opens channels only after acquiring the process ownership described in
[ADR 0001](adrs/0001-execution-model.md). It transfers move-only `TransportLease` values into a `LeaseSet`, which also
retains an application-supplied `OwnershipReservation` guard. Every I/O owner closes and drops before that reservation
is released, including partial negotiation failure or cancellation of a never-polled connect future.

A lease contains an endpoint ID, attachment generation, transfer bound, prevalidated access grant and owned adapter.
The set rejects duplicate endpoints. Connection admission checks the selected candidate, exact endpoint set, kinds,
attachment generations and required exclusivity. `AccessGrant::TransportExclusive` is an assertion by the opener about
transport-specific access; it is not proof that unrelated processes or vendor tools are excluded. The reservation
trait retains a guard's lifetime but does not acquire an OS lock. Actual opening, safe serial settings, process-lock
implementation and device permissions remain host/platform work.

`IoRequest` distinguishes serial reads/writes from HID input/output/feature operations and preserves report IDs.
Adapters register wakers when pending and do not wait synchronously for hardware. Leases reject mismatched operations,
oversized transfers and impossible returned lengths. Driver framing must also bound accumulated decoder state and
handle partial serial operations correctly; neither HID report boundaries nor serial framing can be guessed by the core.

## Connection state and cleanup

`ConnectContext` captures a process-unique session ID, selected ready attachments, an explicit primary source for
keyboard capability reports, and authorized endpoint-to-component routes. It captures topology tokens before I/O;
it is not cloneable. One session can coordinate several required endpoints or many components behind one channel.

| State/input | Result |
|---|---|
| Connecting + successful handshake | Publish an active session |
| Connecting + failure/deadline/drop | Close all transferred leases; no partial active session escapes |
| Active + completed read | Deliver typed result; remain active |
| Active + rejected pre-admission request | Send nothing; remain active |
| Active + admitted request failure/cancellation/deadline | Fail the session and release every lease |
| Active + required endpoint failure | Fail the coordinated session, without declaring every physical member removed |
| Active + disconnect | Enter terminal disconnected state and release leases |
| Active + shutdown/drop | Enter terminal shutdown state and release leases |
| Any terminal state + shutdown/disconnect | Remain terminal; release is idempotent |

Initial disconnect/shutdown is local-only and requires no device reply or goodbye. A future graceful protocol shutdown
must fit an explicit budget and must not prevent local cleanup. `Session::close_with_topology` is a host-side helper
that releases resources and maps an unavailable reason into existing endpoints, preserving physical discovery records.
Busy/permission reasons retain established capability support according to #6; other context loss invalidates it.

### Reconnect

Reconnect creates a new session instead of reviving a terminal instance:

1. Disconnect/release the old session **before opening replacements**; the injected opener must reject competing access.
2. Reconcile topology, safely reopen channels and revalidate attachments, which allocates fresh generations and clears
   old capability evidence.
3. Capture a new context and transfer fresh leases. `reconnect` rejects an active old session or reused generations
   for overlapping endpoint IDs, then invokes the selected backend's connection contract.
4. Refresh capabilities for the new session. Old session objects remain terminal, and old deliveries cannot belong
   to the replacement session even when endpoint runtime IDs survive.

Automatic retries, backoff and degraded multi-endpoint dependency policies remain application/backend work. Initially,
all admitted endpoints are required; losing one ends that session rather than silently retaining uncertain routing.

## Request progress, deadlines and cancellation

The session serializes admitted requests using Rust's exclusive mutable borrow. Each has a checked request ID.
Typed protocol interfaces queue a request without blocking; a single driver pump handles pending output, matching
replies and unsolicited input. Replies must match request ID, source and payload kind. Unmatched replies are protocol
failures, never arbitrary events. A driver may implement stronger wire correlation, but cannot bypass shared checks.

Request futures drive I/O while pending. A dropped **unpolled** read has not admitted work and leaves the session
alone. Dropping an admitted request closes the session: the initial policy does not guess how to drain late replies
or safely reuse a protocol channel after cancellation. Event-wait cancellation has different semantics below.

`SessionOptions` supplies a monotonic `Clock`, connect deadline, message/endpoint limits, per-poll work budget and
event capacity. Each read receives its deadline. The core checks time before admission/I/O and before accepting a
completion. Expired-before-admission requests send nothing; expiration after admission terminates uncertain work.

**The host must schedule deadline wakeups.** An `Instant` does not wake a future. The host knows the supplied deadline
and must race/poll it using its eventual executor/timer. Tests advance a manual clock and explicitly repoll; the
implementation promises no autonomous timeout thread. Polling/hybrid `UpdateStrategy` intervals likewise tell the
host when to schedule supported reads; time passing alone causes no I/O.

Work budgets bound messages processed per poll; exhausting a budget requests another poll and yields. Message bounds
limit decoded text/fact content as well as transfer buffers; they are not a promise about exact Rust heap overhead.
Each driver's individual poll must also remain bounded and nonblocking.

## Unsolicited events and delivery continuity

`take_events()` returns one independent `EventReceiver` per session. Waiting for `next()` holds the receiver, not a
mutable session borrow, so a command can proceed while a consumer waits. The host must still drive idle push input
with `poll_drive`; active requests drive the same input path. Taking a receiver does not itself enable a device-side
keyscan/event mode or prove support for an arbitrary event family.

An unsolicited observation received during a firmware request goes into the bounded event queue while the matching
firmware reply completes the request. Polling reads and push reports share `DeviceObservation`; these are backend
inputs, not the public committed event stream from ADR 0004. No global revision, historical replay or snapshot API
is implied.

Receiver waits register/update wakers and dequeue only when returning an item. Dropping a pending wait loses no data;
wakers are invoked outside queue locks. Dropping the receiver ends that subscription and discards buffered data.

If the queue overflows, it discards incomplete buffered observations, records `ResyncRequired` outside the full queue,
wakes the receiver, returns that error once, then ends. No later event is presented as contiguous. The session can
still answer supported reads, but reconnect is required to obtain a new receiver. Fresh reads/capability refresh can
recover current information meanwhile. Session failure also has an out-of-band terminal error, while orderly close
ends the receiver without queued stale observations. An already-ended/overflowed receiver remains ended.

## Normalized observations, ingress and acceptance

[`src/model/observation.rs`](../../src/model/observation.rs) provides the shared successful-report vocabulary:

- `Observation<T>` distinguishes known, unknown and explicitly reported temporary unavailability.
- `Percentage` validates 0..=100; battery level and charging state are independently partial.
- `FirmwareInfo` reuses existing namespaced version facts and duplicate-key validation.
- `DeviceObservation` carries existing battery/firmware reports or a typed `DevicePatch` for partial indicator, layer,
  lighting, battery, firmware and reachability status. Only existing battery/firmware request interfaces execute reads;
  additional status producers can use unsolicited normalized patches without adding configuration commands.

Unknown is not a zero battery. A successful device report of sleeping/missing/link-unavailable status differs from
a failed transport query. Failed queries remain errors; unsupported remains a capability/dispatch result.
The [normalized state service](state.md) stores these reports with source provenance, timestamps, retained knowledge,
freshness and commit semantics. Desired state and broader configuration payloads remain separate.

`ObservationDelivery` records session, source, target and typed value, plus a checked session-local ingress sequence,
host receipt instant and event/reply origin. Stamping happens inside the validated protocol pump, before queued events
and returned replies diverge. Reversed consumption order cannot make an older event overwrite a newer same-field reply.
Receipt-clock regression or sequence exhaustion terminates the session without emitting invalid observation metadata.

`SessionObserver` shares immutable captured binding and out-of-band liveness, not handles or ownership authority.
It can validate a delivery while another request holds `&mut Session`. Existing `accept_observation` delegates to it;
the runtime combines observer validation with normalized publication. Never create a fresh token on receipt of an old
queued event. Move-away/back and keyboard-only attachment bindings remain checked without inventing a component.

Terminal liveness is recorded before teardown notification, including failure, cancellation, shutdown and drop.
Input overflow also records its ingress boundary outside the queue. Runtime reconciliation stales affected evidence
but does not treat a newer successful read as predating an old gap. The host must drive reconciliation and explicit
expiry; observer changes do not secretly mutate published snapshots or introduce a scheduler.
Further discarded input advances the loss boundary, including after receiver termination, so read recovery is not
silently preserved across newer lost reports.

For capabilities, the host uses `prepare_refresh`, performs async `query_capabilities` without borrowing topology,
then calls `accept_capabilities` to validate and publish with #6's refresh token. On failure or cancellation it retains
the prepared handle and calls `PreparedRefresh::abandon`, unless invalidation/supersession already removed that work.
Keyboard-scoped capability reports use the explicit primary source and require no component placeholder.

## Errors and verification

`BackendError` normalizes unsupported/unknown support, invalid target/input, closed state, contention/access,
cancellation/deadline, disconnection, protocol/resource limits, resync and stale-result failures. It retains backend,
phase, operation, endpoint and original source chains. Stable codes are `kbctl::backend::<kind>` and
`kbctl::transport::<kind>`, with actionable diagnostic help. A decoded result rejected as stale is different from
a protocol failing to decode a reply.

[`backends/fake.rs`](../../src/backends/fake.rs) layers synthetic mappings and independent battery/firmware adapters over
the shared scripted transport/device/lease helpers. Its toy serial v1 codec has length-prefixed inbound frames and
two-byte commands, with bounded incremental decode state and partial-write offsets; read chunks are not frame boundaries.
The payload-oriented `Wire::push` façade keeps specialized state tests readable; raw input is explicit.

[`backends/conformance_tests.rs`](../../src/backends/conformance_tests.rs) registers both feature adapters against the
same protocol-independent lifecycle/capability/read/event assertions. The [versioned corpus](../../tests/fixtures/backends/v1/README.md)
exercises fragmentation, short writes, gated events, malformed responses and pending truncation. Other backend/state
tests retain their targeted ordering, ownership and freshness checks; the adversarial transport probe stays local.
See [backend testing](../backend-testing.md) for APIs, fixtures and contribution steps, and
[hardware testing](../hardware-testing.md) for ignored opt-in test conventions. No sleeps, physical devices or executor
dependency are required by the regular suite. Synthetic replay does not establish target-keyboard compatibility.
