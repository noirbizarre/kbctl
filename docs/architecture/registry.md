# Device registry and identification

The private [`src/registry/`](../../src/registry/mod.rs) module implements the metadata-only registry for
[issue #8](https://github.com/noirbizarre/kbctl/issues/8). It identifies one host-discovered physical unit at a time,
assesses its channels and returns proposed component/association plans and justified backend routes. It performs
no enumeration, opening, probing, connection, topology mutation or capability publication.

The [built-in identifier table](devices.md#source-backed-identification-definitions) includes source-backed GMMK Pro,
Defy and Naya Create definitions. These recognize reviewed identifiers, not hardware-validated KBCTL compatibility.
The binary still has no CLI device command or live discovery host.
Issue #11 reuses physical predicates through `Registry::identify_metadata` for the independent
[input inventory](../input-discovery.md), without manufacturing physical devices, association plans or management routes.

## Definitions and provenance

Definitions are typed Rust data in [`builtin.rs`](../../src/registry/builtin.rs), separate from generic matching and
protocol implementations. No runtime file format or dynamic plugin ABI is introduced.

| Record | Meaning |
|---|---|
| `DeviceDefinition` | Stable declaration ID, model/family/variant labels, runtime/bootloader mode, physical selector, expected components and protocol references |
| `PhysicalSelector` | Exact VID/PID pair, optionally narrowed by exact advertised serial |
| `EndpointRole` | Named required role with transport/interface/HID collection predicates |
| `ProtocolReference` | Backend/configuration identity, primary role, required roles, sources and research-only/enabled status |
| `ComponentTemplate` | Stable local key, component kind/side and optional parent key; expected structure, not observed presence |
| `SourceRecord` | Stable source ID, vendor/upstream/community class, pinned reference and scoped claim/limitations |

Source-backed means the identifier was reviewed in the cited revision. It does not mean a locally captured descriptor,
verified firmware range, safely opened management endpoint or implemented backend. The registry does not use a blanket
verified-device flag. Identifier and protocol-selector sources are recorded separately where they differ.

`Registry::new` validates declaration IDs, source references, unique template/role/protocol keys, acyclic component
parents, transport-compatible selectors and a declared primary role. Bootloader definitions cannot expose runtime
protocol mappings. Invalid declarations produce typed actionable `kbctl::registry::<kind>` diagnostics before matching.
Declarations can share source references, but duplicate definition/source identities are errors.

## Discovery facts and predicates

Physical identity is evaluated before endpoints. A model match remains valid even if its management channel is absent
or unknown. Physical selectors always require the full USB VID/PID pair; names, fuzzy text, side labels, transient
paths and runtime instance IDs are not selector inputs.

`AdvertisedSerial` is optional, nonblank, exact and case-sensitive. Nonblank leading/trailing whitespace is preserved,
not normalized into another device's descriptor. An advertised serial can narrow a declaration but does not become
a verified `HardwareIdentity`, establish uniqueness or pair split units. Missing serial is ordinary discovery data.

Endpoint selectors require an explicit HID/serial transport and may constrain USB interface number, HID usage page
and usage. Every constraint must hold on the **same endpoint and its associated interface**. HID metadata describes
that addressable collection; another collection on the same interface cannot supply a missing usage. Unknown fields
are `None`, not zero or wildcards. Partial HID knowledge is preserved, and HID facts on serial channels are rejected
by the model. Interface numbers need not exist for every transport.

Every tested predicate retains typed expected and observed facts with one of these verdicts:

- `Satisfied`: observed exactly as required; its evidence can contribute to a route.
- `Missing`: the required fact is not established.
- `Mismatch`: an observed value contradicts the required value.

These are matching outcomes, not exceptions. Channel assessment distinguishes no endpoints observed, incomplete
metadata and rejected descriptors. No protocol references means management selection is unspecified, not a fabricated
unsupported capability. A known model with no management endpoint remains identified.

## Inspection results

`Registry::inspect(topology, physical_id, implementations)` returns an owned `Inspection` describing:

1. `Identification`: unknown, a single definition ID, or canonically ordered ambiguous definition IDs.
2. Physical predicate assessments, including missing/rejected evidence for declarations that did not qualify.
3. Matching definitions with per-role endpoint assessments and protocol-reference status.
4. Complete backend routes with explicit primary/role bindings and merged definition/reference/source provenance.
5. Association plans referencing existing physical/logical ownership and proposed component templates.

An unknown device stays in topology with its original metadata and runtime identity. It is not sent to guessed
vendor matchers. Registry inspection takes an immutable topology reference and neither erases unknowns nor changes
existing names, hardware evidence, component presence, associations or capability-cache state.

`Inspection::actionable` returns a route only when classification is unique, exactly one complete route exists, and
there is no unresolved alternative involving a registered enabled implementation. A rejected or research-only
reference remains diagnostic context, not a competing executable implementation.

## Determinism, endpoint assignment and ambiguity

Declarations, roles, endpoint matches, candidates, evidence and provenance use canonical ordering. The same facts and
runtime IDs produce the same result regardless of declaration/registration/vector order. New process-local runtime
IDs in an independently constructed topology are not expected to compare byte-for-byte.

Distinct qualifying definitions remain ambiguous, even when one rule includes more descriptors than another.
There is no registration-order winner, priority score, fuzzy-name fallback or inferred specificity rule.

A mapping can require multiple named endpoint roles. The registry builds a sorted eligible endpoint set per role and
searches for **distinct** endpoint assignments. One channel can serve several component routes through one shared
role; the same endpoint cannot be counted twice to satisfy two distinct roles. The primary role is explicitly named.

The deterministic search stops after proving two distinct assignments. It returns unique, no match, ambiguous, or
indeterminate when its work budget prevents a proof. Finding one solution before exhausting the budget is not proof
of uniqueness. No Cartesian product is materialized or silently truncated, and no first endpoint is selected as a
fallback. Declarations allow 1..=16 roles to bound recursion; search budgets are validated in 1..=100000 endpoint-choice
checks, with a built-in budget of 4096 per protocol reference.

Exact registry routes are keyed by backend/configuration, explicit primary endpoint and named role assignment.
Evidence and source/reference provenance merge for that key. Different assignments remain distinct even if their
endpoint ownership sets coincide. Identification ambiguity is not erased when two definitions converge onto a common
protocol route.

At the lower backend layer, `BackendCandidate` endpoints are an unordered ownership set. `MatchResolution::from_hints`
canonicalizes backend/configuration/endpoint-set routes and merges evidence for duplicates. Primary and named role
semantics are additional registry constraints, not vector-order conventions or information that the lower helper can
discard on the registry's behalf.

## Backend admission boundary

An executable candidate requires all of these:

- A qualifying physical definition.
- An explicitly enabled protocol reference.
- A registered implementation with the referenced backend identity.
- A uniquely resolved required-role assignment.
- The implementation's metadata matcher accepting the selected configuration and channels.

`DiscoveryView` exposes read-only accessors. Its restricted-view constructor accepts only distinct endpoint IDs owned
by that physical view and exposes only their associated interfaces. The registry calls a registered matcher with
this coherent restricted view. A candidate returning a foreign backend/configuration, foreign/missing endpoint set
or evidence absent from the view is a backend-contract error, not an alternative to select silently. A backend may
legitimately decline the metadata match.

The implementation catalog rejects duplicate backend identities, so catalog order cannot choose an implementation.
Matching calls neither `connect` nor transport I/O. Candidates are metadata proposals: the host must still establish
ownership, validate current attachments, perform safe negotiation and discover operations through the capability model.
No model definition creates a `CapabilityReport`.

All current real-device protocol references are **research-only**, even if an implementation with a matching name is
supplied. GMMK has no verified management selector at all. Synthetic enabled definitions and test backends demonstrate
actual candidate generation without pretending that a production Defy/Naya protocol already exists.

## Association and component plans

Host discovery owns physical grouping. All interfaces/endpoints of one physical unit retain its current logical
keyboard; the registry does not create one keyboard per interface. `AssociationPlan` records that existing ownership
and the declaration's expected component forest. Templates are ordered by key, not by observed presence or execution
order; an accepting host must resolve parent keys when constructing components.

A plan is not live mutation authority. The host explicitly accepts it, revalidates current discovery and uses existing
topology methods under its serialized application boundary. A cached plan must not rename hardware, overwrite identity,
create supposedly present modules or merge keyboards automatically.

There is no verified built-in split-pairing key. Naya left/right/dongle labels, common family names, VID/PID, discovery
order and equal advertised serials do not merge independently discovered units. Two attached pairs remain separate
partial keyboards until relationship evidence is established. `Topology::associate_physical_device` remains the
explicit application operation; this issue adds no speculative cross-physical grouping engine.

## Verification

[`registry/tests.rs`](../../src/registry/tests.rs) covers source-backed identification paths, runtime/bootloader
separation, unknown/missing metadata, serial semantics, collection-local HID constraints, endpoint assignment proofs
and budget exhaustion, canonical deduplication, backend contract failures and preserved topology/cache state.
Backend tests additionally cover canonical ownership sets and restricted discovery views. Synthetic descriptors in
these tests do not verify unrecorded interface numbers/usages on real keyboards.

See [hardware evidence](devices.md), [backend contracts](backends.md) and the [domain model](model.md) for the remaining
safe-probe, actual transport-opening, firmware-support and application-integration work.
