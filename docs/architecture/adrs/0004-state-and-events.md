# ADR 0004: Committed snapshots and bounded live events

Status: Accepted design; implementation pending.

The [domain model](../model.md) implements topology retirement and attachment/target freshness checks for issue #5.
The application service, observation application, committed snapshots and event delivery below remain planned work.
Issue #6 also implements a topology-owned [capability cache](../capabilities.md) with refresh supersession and
structural/attachment/context invalidation. That cache does not implement normalized-state snapshots or events.
Issue #7 implements a separate [backend observation channel](../backends.md) with session-binding checks and bounded
delivery. It is an input to the future application commit stream, not an implementation of its epoch/revision contract.

## Context

Discovery, protocol replies and removal can race. A CLI or desktop client must not miss an update between its initial
read and subscription, display a stale battery as current, or apply an old reply to a newly attached module.
We need current state, not durable event sourcing.

## Decision

The application service maintains the authoritative in-memory normalized state. Serialize commits of accepted changes,
while allowing unrelated endpoint I/O concurrently. Give each service lifetime an opaque epoch and each commit a
strictly increasing unsigned 64-bit revision. Start a new epoch before revision exhaustion; never wrap within an epoch.
A snapshot contains `(epoch, revision, records)` representing one committed view. Each event contains
`(epoch, revision, upserts, removals)` for one atomic commit; upserts contain complete replacement records.
Observation timestamps are metadata, not ordering keys. Revisions establish software consistency, not simultaneous
physical measurements across components.

### Bootstrap and resynchronization

The direct host exposes an atomic subscribe-and-snapshot operation internally: register a bounded queue and capture
its baseline snapshot in the same serialized step. Deliver only subsequent revisions to that subscription.

The D-Bus adapter exposes the same commits through `GetSnapshot` and a root `StateChanged` signal:

1. Resolve the unique owner and install/confirm the state and owner-change signal matches before requesting a snapshot.
2. Buffer state signals from that owner while `GetSnapshot` captures the current committed view.
3. Install the returned snapshot at revision R; discard buffered signals of the same epoch at revisions at or below R.
4. Apply subsequent commits in order, starting at R+1. Buffering and publication must preserve application commit order.
5. On owner/epoch change, revision gap, local buffer overflow or bus disconnection, discard the live view and bootstrap
   again. Do not continue presenting a partial cache as fresh.

For example, a client subscribes at revision 20; commit 21 arrives while its snapshot is being read. A snapshot at 20
is followed by buffered event 21. A snapshot at 21 supersedes that buffered event. Neither ordering loses a change.

Initial v1 events are unfiltered, so consecutive revisions are meaningful for every coherent subscriber. There is no
per-client replay log. Internal subscriber queues are bounded: overflow invalidates the subscription and requires a
new snapshot rather than silently dropping an event. A D-Bus adapter unable to publish its ordered stream must close
its bus connection and stop the host, making invalidation visible through owner loss. Slow clients bound their own
bootstrap buffers and resynchronize on overflow or reconnection. No consumer blocks hardware processing indefinitely.

Application revisions detect gaps when a subsequent event arrives; they are not a liveness heartbeat. If a consumer
cannot establish connection health it marks its view stale and obtains a new snapshot before trusting it again.
Snapshots can always recover current state; no delivery or history is promised across a disconnection.

### Device and component lifecycle

| Input | Committed behavior |
|---|---|
| Candidate discovery | Add metadata with unknown protocol/capabilities until safe recognition succeeds. |
| Supported endpoint accessible | Establish its attachment generation; publish verified capabilities and observations. |
| Permission denied/busy | Keep discoverable identity, mark access unavailable and provide recovery context; other devices continue. |
| Query timeout/protocol error | Record failure, retain last known values as stale, retry with bounded backend-specific backoff. |
| Component unreachable | Preserve the logical device and known component with unavailable/unknown reachability as evidence permits. |
| Confirmed module removal | Remove that component instance and descendants; retain unaffected components. |
| Host endpoint unplugged | Cancel its work and close its handle; update affected components and retire devices with no remaining presence evidence. |
| Reconnection | Create a new attachment generation; retired instances get new runtime IDs, with optional evidence-based persistent matching. |
| Suspend/resume | Stop polling, mark affected observations stale, then reconcile discovery and revalidate handles before refreshing. |
| Access revoked | Stop queries/close affected handles when detected; mark unavailable and require successful revalidation before fresh observations. |
| Service restart | New epoch; clients discard old paths, snapshots and buffered events. |

Every in-flight operation carries its endpoint generation and target instance. Reject completions after cancellation,
generation change or target retirement. A removed component cannot reappear because an earlier request completed late.
Reconcile discovery after monitor interruption; enumeration and queued hotplug events must not create duplicate devices.

The implemented primitives also check the target's original keyboard membership. Physical-device retirement removes
its owned interfaces, endpoints and represented components, preserving other physical members of the keyboard.
Endpoint unavailability alone does not retire the physical unit; the application must determine loss of presence.
The application must serialize freshness validation with observation application and add per-operation cancellation
guards when it introduces cancellable work. Domain tokens alone do not implement the full cancellation policy.

Retirement removes records from current snapshots and emits removals in the coherent stream and corresponding standard
D-Bus interface-removal signals. Retain no unbounded disconnected-device history. Loss of one endpoint is not proof
that every component of a multi-endpoint keyboard disappeared.

### Lifecycle walkthrough

A two-half keyboard is present at revision 8. Its right endpoint disappears: revision 9 marks affected observations
stale and retires the right instance if presence is lost, retaining the left and logical device. The right half
reappears under a new attachment generation at revision 10. A reply issued against revision 8's old handle is rejected.
A client that sees revision 10 after revision 8 requests a fresh snapshot; it never guesses revision 9's contents.

## Alternatives and trade-offs

- **Polling-only clients:** easier subscription logic, but adds repeated work and misses useful lifecycle transitions.
  Backends may still poll hardware; clients observe the resulting commits.
- **Unversioned signals:** minimal wire data, but cannot reliably reconcile bootstrap or detect missed commits.
- **Durable event log/replay:** enables historical queries but adds storage, retention and schema migration concerns
  unrelated to current status. Rebuild from snapshots instead.

## Consequences and follow-up

Direct and daemon hosts share lifecycle semantics and can use the same scenario tests. Bounded queues may force a slow
consumer to resynchronize, a deliberate resource trade-off. Implementation must test bootstrap races, overflow,
removal during I/O and epoch changes; exact queue sizes, freshness deadlines and polling rates require backend evidence.
