# ADR 0002: Separate topology, transport, protocol and capabilities

Status: Accepted design; topology, identity and connection primitives implemented for issue #5, and capability
metadata/cache for issue #6. Issue #7 implements [backend contracts and a fake-backed session lifecycle](../backends.md).
Real hardware backends and application lifecycle integration remain pending.

## Context

The [three target devices](../devices.md) range from one keyboard body to halves, controllers and removable modules.
USB interface counts do not describe that topology. Firmware and connection mode determine what can be observed;
a vendor/model label alone does not establish a common feature set.

## Decision

Use the following vocabulary throughout the core and adapters:

| Concept | Meaning |
|---|---|
| Logical keyboard (Device in the planned D-Bus API) | A keyboard, possibly partially observed, that a user addresses as one unit |
| Physical device | One hardware unit independently discovered by the host, owning interfaces and endpoints; one keyboard may aggregate several |
| Interface | A host-visible interface of a physical device, not a keyboard or an inferred internal part |
| Component | An independently meaningful part: main body, half, controller or module; optionally a child of another component |
| Transport endpoint | A host-accessible channel and its attachment, such as a serial port or a HID interface |
| Backend | Protocol-specific recognition, safe query selection, decoding and mapping into domain observations |
| Capability | A named, typed observation or future operation that a device/component can support |

Each physical device belongs to one logical keyboard and may have zero or more interfaces/endpoints/components.
An endpoint optionally references an interface of the same physical device. Components form a parent/child forest
within a physical owner; independently discovered units are associated through keyboard membership. Hidden parts
remain components unless independently discovered by the host. An endpoint may serve several components; a keyboard
may use several endpoints. Components need not have an endpoint of their own. Keep routing in the backend session;
do not expose transport handles or arbitrary vendor packets through the domain API. Aggregation is explicit and
caller-directed, never inferred from interface counts, side labels or matching display names.

Linux integration enumerates candidates and their metadata. Backends match supported interface/firmware evidence
and use transports for bounded I/O. Matching a vendor ID is insufficient permission to send arbitrary probes.
Protocol details, polling constraints and normalization belong to backends; the core decides how accepted observations
affect identity, availability and lifecycle. HID and USB serial are both required architectural possibilities.

### Identity

Assign distinct opaque runtime IDs to keyboards, physical devices, interfaces, endpoints and components. Never
reuse them within the process lifetime, even across independent topology instances; rediscovery of retired instances
allocates fresh IDs. The future service epoch scopes them externally. Public object paths derive from runtime IDs,
never raw device nodes or serial numbers. Keep attachment generations separate so revalidation also rejects delayed
work from an old handle without needing to retire hardware.

Persistently matching a physical keyboard is optional and evidence-based: use verified device/protocol identifiers
when available, with an explicit confidence/ambiguity indication. VID/PID identifies a family, not an individual;
`/dev/hidrawN`, `/dev/ttyACMN` and USB port paths are not durable identity. Do not merge two halves or devices based
only on side names, discovery order or similar descriptors. Ambiguous associations remain partial keyboards.
Missing identity evidence is explicit. Verified unit identity is distinct from advertised identifiers and group/pairing
claims; collisions preserve separate runtime records and produce ambiguous candidates. Hardware evidence may remain
the same across reconnects while runtime IDs change. There is no on-disk identity persistence yet.

### Capabilities and observations

The base record contains identity evidence, display metadata and discovery/access/backend condition. Optional
capabilities initially include firmware information, battery and runtime state only where safely observable.
Layer/profile identifiers, if available, are backend-reported identifiers, not a shared layout-engine model.

Keep these dimensions separate:

- **Support:** unknown, supported or unsupported for this firmware/transport combination.
- **Availability:** whether a supported capability can currently be queried, with a reason such as inaccessible
  endpoint, missing component or busy transport.
- **Observation quality:** unknown, fresh, stale or failed, together with last-success time and relevant error.

A retained stale value is explicitly marked; a missing battery is never encoded as `0%`. Report battery per component
with percentage/charging fields only when known. A wired component does not gain a battery capability for symmetry.
Do not average component batteries into a fictitious keyboard battery. Access/backend condition describes what KBCTL
knows about communication, not a hardware self-test or whether normal typing works.

Adapters expose only implemented, supported capability interfaces. Unknown/unsupported entries and unavailability
reasons remain discoverable in base metadata; temporary failures preserve supported interfaces and change status.
Future controls require explicit typed capabilities and shared validation; there is no generic vendor-command escape.

The implemented [capability model](../capabilities.md) reports support per typed operation, with read-only/writable
classification derived from the operation. Sparse reports are explicitly scoped to a keyboard or component; they
do not inherit or union. Reporters resolve their own firmware/protocol constraints and supply namespaced opaque
version/constraint metadata. The generic core does not evaluate SemVer or assume a universal version ordering.

The topology-owned cache fully replaces a scope's report on refresh and rejects stale results using unique refresh
tokens. Structural, attachment or known context changes conservatively invalidate the keyboard's evidence. Temporary
permission/busy failures preserve established support while exposing source unavailability; revalidation requires
rediscovery. This cache stores support evidence, not observed state or executable authorization.

## Alternatives and trade-offs

- **One device per USB endpoint:** easy enumeration, but duplicates composite keyboards and loses internal components.
- **One vendor-specific object per model:** accurately mirrors each implementation but forces every client to interpret
  protocol quirks and prevents generic battery/status consumers.
- **All devices implement one large feature interface:** easy client code until absent fields become misleading sentinel
  values. Sparse capabilities require clients to discover support but preserve truthfulness.
- **General component graph/plugin framework:** potentially flexible, but adds identity, cycle and compatibility problems
  before a hierarchy and private backend modules have proved insufficient.

## Consequences and follow-up

Partial availability and firmware variation are ordinary domain states. The implemented
[domain model and invariants](../model.md) include synthetic split/module tests without actual USB access.
Backend work must establish pairing evidence and safe capability detection per firmware; this ADR deliberately
leaves backend traits, crate selection and polling intervals to that work.
