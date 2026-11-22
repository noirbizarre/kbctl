# Implemented device and keyboard model

The private [`src/model/`](../../src/model/mod.rs) modules implement the in-memory domain layer for
[issue #5](https://github.com/noirbizarre/kbctl/issues/5), extended with [capability reports and cache](capabilities.md)
for issue #6. They perform no enumeration, transport I/O or firmware queries.
The [CLI](../cli.md) consumes normalized input/indicator snapshots. No identifiers or topology are persisted to disk.

## Records and ownership

| Type | Meaning and invariant |
|---|---|
| `LogicalKeyboard` | A user-addressable keyboard with one or more physical members in a published topology |
| `PhysicalDevice` | One hardware unit discovered independently by the host; belongs to exactly one keyboard |
| `Interface` | Host-visible interface belonging to exactly one physical device; optional USB interface number |
| `Endpoint` | Host-accessible management channel belonging to exactly one physical device; optional interface association |
| `Component` | Meaningful body, half, controller, receiver, dongle or module represented through one physical device |

Issue #8 adds optional exact `AdvertisedSerial` metadata to physical units and partial `HidMetadata` usage page/usage
facts to addressable HID endpoints. Advertised serial is independent from verified identity evidence; missing serial
is valid. Usage facts belong to that endpoint's collection, not every collection sharing its USB interface. HID facts
on serial transports are rejected. Display names remain independent from discovery matching.

A physical device can have zero or more interfaces, endpoints and recognized components. An interface can have
multiple endpoints. Here, endpoint means a communication channel, not necessarily a literal USB endpoint descriptor.
An endpoint's interface, when supplied, must belong to its physical owner. Serial channels need not invent an
interface record. Neither an interface nor an endpoint automatically creates a keyboard.

Components form a forest. Each parent belongs to the same physical device and therefore the same logical keyboard
as its child; missing parents, self-parenting and cycles are rejected. Independently host-discovered hardware is
connected through keyboard membership, not component ancestry. Hidden parts behind a controller stay components.

Components do not own channels: one endpoint can serve several components, and a component need not have its own
endpoint. Actual routing is backend policy, not implied by component parentage. Components can share a keyboard
while being represented through different physical devices.

## Validated construction and mutation

`Topology` owns private record maps and exposes immutable views. `PhysicalDeviceSpec` assembles one physical unit's
interfaces, channels and component hierarchy. Interface/parent references are local indexes in that specification,
preventing references into another physical unit. Forward parent references are allowed if the result is acyclic.
USB interface numbers, when present, cannot repeat within one physical device.

- `add_physical_device(None, spec)` creates a separate keyboard and physical unit atomically.
- `add_physical_device(Some(keyboard), spec)` explicitly adds a unit to a live keyboard.
- `associate_physical_device` explicitly moves a unit and its components to another keyboard, retiring an empty
  source keyboard. The caller must establish association evidence first; the model does not guess pairings.
- `add_component` accepts only an existing parent represented through the same physical device. Allocating a new
  component instance prevents insertion from creating cycles.
- `remove_component` retires only the component and its descendants, preserving siblings and physical hardware.
- `retire_physical_device` removes its interfaces, endpoints and components. Other keyboard members survive; the
  logical keyboard retires when its last physical member leaves.

Fallible validation/allocation completes before mutation. Errors leave existing topology unchanged. Allocation gaps
on a failed assembly are harmless. The aggregate is not cloneable: snapshots will be separate immutable records,
not independently mutable copies of live instances. Errors identify offending references and include recovery help.

An unavailable endpoint is not proof of missing hardware. Future application/discovery code decides when there is
enough presence evidence to invoke retirement; the domain model does not infer it from a failed query.

The [registry](registry.md) proposes component/association plans without mutating these records or capability state.
Multiple interfaces retain their physical unit's existing keyboard. Templates are not evidence of component presence,
and common family names, side labels or equal advertised serials do not pair independent physical units. The host
must explicitly accept/revalidate plans before applying existing topology operations.

## Identity and names

### Runtime instances

`KeyboardId`, `PhysicalDeviceId`, `InterfaceId`, `EndpointId` and `ComponentId` are distinct opaque types. A shared,
checked process-global allocator never reuses a value, even across independent `Topology` instances or concurrent
allocations. Exhaustion returns an error permanently rather than wrapping. Runtime IDs are not hardware identifiers
or cross-process identifiers; the [state service epoch](state.md) scopes committed snapshots externally.

Rediscovering a retired unit creates new runtime IDs, even if its hardware evidence is identical. A still-present
keyboard can retain its ID when one physical member reconnects. If its last member was retired, rediscovery creates
a new keyboard instance too. Runtime identifiers must not be saved as durable device identity.

### Hardware evidence

`HardwareIdentity` explicitly represents either missing evidence (`NotReported` or `NotYetEstablished`) or a nonempty
set of `IdentityClaim` values. Each claim has an issuer namespace, exact identifier value, unit/group scope, and
advertised/verified strength. Duplicate namespace/scope pairs and blank identifiers are rejected.

The discovery/backend caller is responsible for verification: creating a verified claim is an assertion about its
source, not something this generic model can prove. Namespaces must include any model/transport scope necessary
to make values comparable. A serial number is not assumed to exist, be unique, or identify an entire split keyboard.
Group/pairing identifiers describe a relationship, not the identity of one unit.

`HardwareIdentity::compare` returns `Matching`, `Conflicting` or `Insufficient`. Only verified unit claims with compatible
namespaces participate. Any conflicting shared namespace wins over a matching one. Missing evidence, advertised
strings and group identifiers cannot establish a unit match. Exact values are preserved rather than trimmed into
accidentally equal identifiers.

`Topology::identity_candidates` reports insufficient evidence, no match, one candidate or ambiguous candidates.
Conflicting unit evidence excludes a candidate; duplicate matching claims leave multiple records and yield ambiguity.
Even a single candidate does not cause a merge: this is evidence for caller-directed reconciliation, not a persistent
identity database. Association/storage policies can build on these records later.

USB VID/PID, names, left/right labels, enumeration order, USB port paths and transient device nodes are not individual
hardware identities. Identical metadata never automatically groups units. Keep unstable locators in `EndpointLocator`,
not verified identity claims.

The application catalog adds [CLI selectors](../cli.md#selector-evidence-and-persistence), separate from runtime and
verified hardware identity. USB VID/PID plus advertised serial can provide an evidence-qualified persistent selector;
missing evidence uses an explicit attachment-locator fallback. Neither grants reconciliation/pairing authority. Duplicate
claims remain ambiguous and do not merge attachment groups; digest collisions are also rejected during resolution.

### Human-readable names

`DisplayName` rejects blank text. Optional discovery names receive deterministic fallbacks such as `Unknown keyboard`,
`Unknown physical device`, `Left half` or `Module`. Names need not be unique and never participate in matching.
Renaming a keyboard or physical unit changes neither runtime identity nor hardware evidence.

## Connection and operation freshness

`TransportKind` currently distinguishes HID and serial channels. `ConnectionState` is independently unvalidated, ready
with an `AttachmentGeneration`, or unavailable with a typed reason such as permission denial, busy channel,
disconnection, suspend or protocol failure. Ready means the adapter validated the channel, not that every capability
works or that normal typing is healthy. Capability support is tracked separately from channel availability.
Issue #7 adds typed backend observations, extended by #9's [normalized state and freshness model](state.md).

`revalidate_endpoint` issues a fresh generation, even for a previously ready endpoint. `invalidate_endpoint` makes
all work from the previous ready attachment stale without removing physical hardware. Restored access requires
revalidation, not simply changing a boolean back to connected.

`begin_operation` requires a ready endpoint and a live target component in the same keyboard. Its opaque token records
endpoint, generation, target instance and original keyboard. `validate_completion` rejects old work after invalidation,
revalidation, removal/recreation or changed keyboard membership. Membership generations prevent moving a unit away
and back from reviving old tokens, including work from an unchanged endpoint targeting that unit's components.
A token validates freshness, not backend authorization
to route a query to that component. The application must serialize validation with applying an observation; this
standalone check is not a concurrency transaction or a per-operation cancellation mechanism.
`begin_attachment`/`validate_attachment` also capture endpoint generation, physical membership and keyboard without a
component target, allowing keyboard-only backend bindings to reject stale attachment contexts.

The [backend/session contract](backends.md) implements admitted-request cancellation, resource release and bounded
backend event delivery. Issue #9's runtime now owns coherent normalized updates, expiry and committed subscriptions.
Scheduling, retries, actual access recovery and discovery collection remain host work; see
[ADR 0004](adrs/0004-state-and-events.md).

## Observed state and commits

`model::observation` defines successful reports and typed partial device patches. `model::state` wraps source-scoped
fields in provenance, monotonic stamps, freshness, last-known data and failure facts. Host lock keys/entry points are
distinct from device LEDs. Active layers, lighting, battery, firmware and reachability preserve unknown/unavailable
values rather than filling defaults.

The [runtime/state service](state.md) owns topology plus normalized writes and exposes immutable snapshot readers.
Controlled actions reconcile lifecycle/removals and publish complete replacement records under epoch/revision cursors.
Snapshots are self-contained; atomic bootstrap and bounded subscriptions permit explicit recovery after missed commits.

## Capability evidence

The implemented [capability model](capabilities.md) stores sparse operation-level reports explicitly scoped to a
keyboard or component. Unknown support is distinct from unsupported, and reading support does not imply writing.
Version/constraint metadata is opaque to the core; reporters resolve their own firmware/protocol restrictions.

`Topology` owns accepted reports and pending refreshes. Its mutation methods enforce cache invalidation for structural
or attachment/context changes. Unique refresh tokens reject superseded results even on the same attachment.
Permission/busy access loss preserves established support while separately exposing source unavailability; successful
revalidation requires rediscovery. No capability history, on-disk storage or automatic feature aggregation is added.

## Representative tests

[`src/model/tests.rs`](../../src/model/tests.rs) contains synthetic scenarios, not backend/hardware support claims:

| Scenario | Representation and assertion |
|---|---|
| GMMK Pro | One keyboard, one physical unit, multiple HID interfaces/channels and a body; no keyboard per interface |
| Dygma Defy | A discovered Neuron physical unit, controller and hidden half components; one serial channel can target both halves |
| Naya Create | Two explicitly associated discovered halves with module children; module retirement preserves other components and both halves |
| Multiple similar pairs | Identical labels/model IDs or duplicate hardware claims do not silently combine keyboards |
| Reconnect | Matching verified evidence survives while retired runtime instances and operation tokens do not |

Additional tests cover invalid ownership/cycles, missing identity, partial physical retirement, failed assembly,
name changes, connection revalidation, allocator concurrency and exhaustion. See the
[hardware evidence matrix](devices.md) for firmware/topology uncertainties not resolved by these tests.
