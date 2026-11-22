# Normalized observed state

Issue [#9](https://github.com/noirbizarre/kbctl/issues/9) implements a private, backend-independent observed-state
model and commit service. [`runtime::Runtime`](../../src/runtime/mod.rs) owns topology and is the exclusive normalized
writer. [`state::StateReader`](../../src/state/mod.rs) supplies immutable snapshots and atomic live subscriptions.
The CLI is not wired to this service yet; actual Linux collection, hardware protocols, scheduling, D-Bus and persistence
remain host/integration work.

## Status vocabulary

[`model::observation`](../../src/model/observation.rs) defines successful reports and validated partial patches.
[`model::state`](../../src/model/state.rs) wraps reports in stored provenance, quality, source and lifecycle records.

| Observed family | Representation |
|---|---|
| Host indicators | Separate `HostLock::{Caps, Num, Scroll}` entries for an explicitly registered host source/scope |
| Device indicators | A distinct `DeviceIndicator` type, including standard lock LEDs and namespaced custom indicators |
| Active layers | An explicitly observed complete set of namespaced layer IDs; known empty is not unknown or layer zero |
| Lighting | Independently partial enabled, percentage brightness, RGB color and namespaced effect per component/named zone |
| Battery | Independently partial percentage and charging state, scoped to the reporting component and endpoint |
| Firmware | Validated namespaced version facts, with no universal SemVer ordering |
| Device connection | Explicitly reported reachability, including sleeping; distinct from management endpoint and session state |

Namespaced IDs preserve producer-specific labels without embedding protocol types, raw packets or arbitrary vendor
configuration maps. A layer set does not assert activation priority. This issue models observed status only: it adds
no desired state, configuration writes, keymap/macro contents or inferred aggregation.

### Patches versus records

`DevicePatch` has typed constructors for each leaf. Merging patches rejects duplicate fields. A missing leaf means
**unchanged**, including its report stamp, deadline, failure and ordering watermark. Explicit `Known`, `Unknown` or
device-reported `Unavailable` updates the supplied leaf. Layer sets and firmware are whole-field replacements.
Existing full `BatteryState` reports adapt into two explicit leaves; partial battery reports need not refresh charging.

`HostPatch` has separate keys/value types and a host registration handle. It cannot address device LED state.
Unsupported host collection, permission denial and inaccessible sources are explicit host availability transitions,
never false lock values. Renewing an unavailable host source creates a fresh generation before accepting reports again.

Snapshots and commits contain **complete replacement records**, not these input patches. Replacing a record means
replacing its whole field map. An absent field is unobserved, not a default false/zero and not permission to keep an
old client-side value absent from the replacement. A component without source records has no established observations.

## Source scope and retained knowledge

Host sources have a process-unique source ID, explicit opaque scope and generation. The model does not assume a
singleton global lock state or assign a host scope to every keyboard. A future collector must document whether its
scope describes a seat, input context, device or another authority.

Device source slots are keyed by component and endpoint. Current provenance records reporter identity, session ID,
captured attachment and original target/membership context. A controller may report for another physical member's
component through an explicit authorized route. Multiple endpoints reporting for one component remain separate:
there is no latest-source winner, averaged battery, unioned layer set or host-to-device indicator copying.

A stored `FieldState<T>` contains:

- Its latest successful report, if any: value, original report stamp and explicit fresh/stale validity.
- At most one last-known sample, retaining its original value and stamp even after unknown/unavailable/failure.
- An optional typed, bounded failure fact. Backend diagnostic chains remain in the backend error path, not snapshots.

`FieldQuality` describes report evidence: unknown means no report, fresh/stale describe its validity, and failed means
an applicable failure was recorded. A **fresh report of an unknown value is still unknown**; consumers must inspect
the value as well as quality. Last-known data is historical, never silently substituted as the current fresh value.

Expiry or lifecycle loss does not invent a new device reading. Explicit unknown withdraws current confidence while
preserving useful last-known data. A successful device report that a component sleeps is different from a request
failing to communicate with it. Successful replacement evidence clears that leaf's failure.

Reconnect replaces the current generation in a source slot rather than creating per-session status history. Untouched
old reports remain stale and retain their original provenance; new reports can restart their session-local sequence.
Retiring a source/target removes its current records and watermarks. Historical IDs inside retained provenance are
original context, not a claim that those topology instances are still live.

## Receipt order and provenance

The shared backend pump stamps every validated normalized observation with a checked session-local sequence,
monotonic host receipt instant and reply/event origin **before** splitting queued unsolicited input from read replies.
Sequences and receipt-clock regression fail closed instead of wrapping or emitting misleading input. Capability
replies are not device status observations.

The runtime applies ordering per source generation, target and leaf:

1. Validate the entire delivery, captured binding and patch/deadlines before mutation.
2. Apply supplied leaves newer than their own watermark.
3. Ignore older/duplicate supplied leaves and report their count; omitted leaves do not advance.
4. Publish the accepted subset as one atomic commit if it changed visible records.

For example, event A arrives before reply B but is consumed afterward. A's old percentage cannot overwrite B's newer
percentage. A charging leaf omitted by B may still apply. An earlier firmware observation also remains applicable
after a later battery reply. Equal receipt timestamps do not erase this distinction: sequences decide ordering.

This is software ingress order, not proof of hardware measurement order. A producer may report cached values.
Optional `MeasurementTime` retains a namespaced clock and opaque ticks without comparing firmware clocks or inferring
clock synchronization. Stored stamps include receipt time, caller-supplied freshness deadline, optional measurement
evidence, origin and original provenance. Host producers supply explicit sequence/receipt evidence; contradictory
ordering against retained source/field evidence is rejected.

## Time and freshness

`Timestamp` is a checked duration from runtime startup, not wall time. Snapshot headers identify that time domain.
The domain stays stable if the commit epoch rolls over, so existing sample timestamps do not change origin.
Backend receipt instants and runtime actions must use the same host monotonic domain. Runtime actions are nondecreasing;
delayed receipt times may legitimately precede the latest action time. Future/pre-domain stamps and deadlines before
receipt are rejected without partial mutation.

The caller supplies finite freshness deadlines for included leaves, either uniformly or per field. No keyboard-wide
battery interval or lighting timeout is invented. A sample accepted after its deadline is immediately stale; acceptance
does not restart its freshness duration. Updating percentage alone leaves charging's original deadline intact.

`next_deadline()` tells the host when to schedule `advance_time(now)`. At `now >= fresh_until`, all due evidence
transitions in one commit; repeated ticks without transitions produce no event. Snapshot reads and subscription polls
do not run expiry or reconcile lifecycle. There is no hidden thread: a host that stops driving time/lifecycle actions
cannot expect autonomous updates, and an old snapshot is always evidence from its recorded commit.

## Runtime mutation and lifecycle

The runtime exposes immutable `topology()` and a reader, with controlled methods for add/associate/remove/retire,
rename, endpoint revalidation/invalidation, sources, observations and expiry. It offers no mutable-topology or public
arbitrary-closure escape hatch. Each action validates time/input, reserves publication resources, applies one validated
topology operation or field batch, reconciles affected sources and publishes one commit. No topology/store borrow is
held across protocol I/O or `.await`. Multi-endpoint host orchestration must not mistake several fallible actions for
one atomic transaction.

| Lifecycle fact | Normalized effect |
|---|---|
| Busy/access denial | Keep discovery, publish unavailable access and stale values; capability support retains #6 semantics |
| Suspend/revalidation | Invalidate old attachments/evidence; revalidation never resurrects freshness |
| Component removal | Remove its subtree/status; preserve unrelated topology |
| Physical retirement | Remove owned projection/source records; reconcile surviving routes whose captured session became invalid |
| Reassociation | Publish grouping changes; original membership validation prevents old evidence becoming current again |
| Session failure/cancel/shutdown/drop | Publish terminal source availability and stale/failed evidence when the runtime reconciles |
| Backend input overflow | Mark continuity lost and stale evidence at/before the gap; a newer read can recover its included leaves |
| Capability-cache bookkeeping | Does not renew observation stamps, freshness or normalized revisions |

`SessionObserver` is a cloneable read-only binding/liveness handle, not an I/O owner or reusable connection constructor.
It lets the runtime validate/commit an event while a request still holds `&mut Session`. Terminal state is recorded
outside the backend input queue, including drop and admitted-request cancellation. Continuity loss carries its ingress
boundary; reconciling an old gap cannot make a later successful read stale.
Further decoded input discarded by an ended/dropped receiver advances that boundary, so an earlier recovery read
cannot remain fresh despite a newer known loss.

`synchronize(now)` and other controlled actions reconcile captured lifecycle facts. Endpoint-ready alone does not prove
session health. Because #7 validates the whole captured binding, removing one required half/route can also stale the
surviving routes of that session. Rebinding is explicit. The host still owns stopping actual I/O and reopening leases;
normalization never substitutes a new token when an old event arrives.
An actual firmware/protocol change that invalidates a producer's interpretation requires ending/rebinding that source
and invalidating capabilities. Clearing support-cache bookkeeping alone is not a source-validity transition.

## Snapshots, commits and subscriptions

A snapshot contains `(epoch, revision)`, time-domain identity and canonical immutable records for keyboards, physical
membership, interfaces, components, endpoints, sessions and source-scoped status. Current topology references resolve
within the snapshot; original binding/provenance IDs are explicitly historical where appropriate. No live `Topology`,
transport handle, registry match object or mutable capability cache is copied into the snapshot.

Each visible action increments revision once. A commit contains complete replacement-record upserts and removals,
never the same key in both. Fully identical resulting records produce no commit. Equal values with changed receipt
stamp, sequence, deadline, source, quality or retained evidence are changes. Merely reading or advancing an unrelated
private action clock does not manufacture record changes. Canonical keys make batches deterministic. Global software
commit order promises no simultaneous measurement order across unrelated hardware.

`subscribe_and_snapshot(capacity)` registers a bounded subscription and captures revision R under the same store
serialization lock. Its first possible event is after R; there is no bootstrap gap or duplicate baseline event.
Healthy subscribers see the same consecutive, unfiltered commits. Applying full-record replacements/removals recovers
the next snapshot; consumers do not need an earlier event history to interpret current records.

Overflow clears only that subscriber's incomplete queue, exposes terminal `ResyncRequired(Overflow)` outside capacity,
and ends it. A consumer must discard the partial view and subscribe with a new snapshot. Dropped/invalid subscriptions
are removed rather than retained as history. Waits are cancellation-safe, and user waker callbacks run outside internal
locks. A slow consumer never makes publication wait for queue space. Writer closure ends subscriptions and causes
future snapshot/bootstrap requests on that reader to fail as closed.

Opaque 128-bit epochs come from OS entropy through `getrandom`; runtime IDs or PIDs/wall time are not restart identity.
Before a revision increment could overflow, a replacement epoch is generated before mutation. Old subscriptions receive
epoch-change resync, changed state begins at revision 1 without wrapping, and the new epoch's initial commit contains
its complete record set. Epoch-generation failure or repetition leaves topology/store unchanged. Test injection covers
rollover without approaching real counter exhaustion. There is no durable replay or persistence.

## Verification and remaining integration

`state/tests.rs` verifies atomic bootstrap, complete-record reconstruction, slow-subscriber isolation, cleanup,
reentrant wakers and epoch handling. `runtime/tests.rs` covers host separation, partial fields, freshness, clocks,
projection and mutation atomicity. `backends/state_tests.rs` exercises actual fake protocol ingress through the runtime,
including reverse-consumed event/reply ordering, liveness, overflow, reconnect and source separation.

This service is hardware-free. Linux host-source discovery/collection, real status queries, retry/scheduling policy,
CLI/daemon hosting and D-Bus export remain separate integrations. Broader configuration payloads, desired state and
cross-source winner/aggregation policies are not inferred by this model.
