# Implemented capability model

The private [`src/model/capability.rs`](../../src/model/capability.rs) module implements support evidence and an
in-memory discovery cache for [issue #6](https://github.com/noirbizarre/kbctl/issues/6). Backends can describe a small
subset of operations without implementing unrelated features. These types do not implement device commands,
transport probing or a normalized state store. Issue #7 supplies the separate [backend contract](backends.md),
minimal typed status values and backend event delivery.

## Operations and access

`CapabilityOperation` is a closed operation catalog, not an arbitrary feature/action cross-product. Access is derived
from the operation, so metadata cannot accidentally describe reading a battery as a configuration write.

| Domain | Read-only operations | Writable operations |
|---|---|---|
| Indicators | `ReadIndicators` | `WriteIndicators` |
| Layers | `ReadLayers` | `ActivateLayer` |
| Lighting | `ReadLighting` | `WriteLighting` |
| Battery | `ReadBattery` | None |
| Wireless state | `ReadWirelessState` | None |
| Keymaps | `ReadKeymaps` | `WriteKeymaps` |
| Macros | `ReadMacros` | `WriteMacros` |
| Runtime events | `SubscribeRuntimeEvents` | None |
| Firmware information | `ReadFirmware` | None |

Indicators mean keyboard-reported logical indicators, not a desktop's interpretation of host lock state. Lighting
describes general keyboard illumination. Layer activation is distinct from rewriting keymaps. Names classify support;
additional operation payloads and finer-grained backend operations remain future work. No flashing or raw passthrough
is modeled.

Read-only means semantically non-mutating with respect to keyboard configuration. Status queries can require outbound
bytes and write-capable transport handles. Event subscription changes delivery bookkeeping, not keyboard configuration.
Declaring writable support does not execute a write or alter the initial status-first application scope.

## Support, access availability and observations

Every operation has one of three support states:

- `Unknown`: unprobed, inconclusive, omitted from the current report, refreshing or invalidated. This is the default.
- `Supported`: the reporter established support in this device/firmware/protocol context.
- `Unsupported`: the reporter explicitly established lack of support in this context, not a permanent property of a model.

For example, a report may support `ReadLayers`, explicitly reject `WriteMacros`, and omit `ActivateLayer`. The omitted
activation operation remains unknown; successful reading proves nothing about writing. An entirely empty report is
valid and establishes no operation as unsupported.

`SourceAvailability` separately describes whether the accepted report's source endpoint is currently available,
unavailable with a typed reason, or unknown. Permission denial or busy access does not rewrite known support as
unsupported. Source readiness is not per-command authorization, proof of backend routing, or successful execution.
Consumers must check relevant current access and future application/backend policy before attempting an operation.

Observed battery percentages, indicator values, layer state, timestamps and observation quality are separate concerns.
This cache stores support evidence only. It does not manufacture a zero battery, average component batteries or
infer feature support from an observed value's absence.

## Scope and provenance

`CapabilityScope` explicitly identifies either a `LogicalKeyboard` or a `Component` by its live runtime ID. Reports
never inherit from a parent, propagate to siblings or automatically union into keyboard-level support. A keyboard
report is a separate explicit claim and does not mean all components support every reported operation.

A source endpoint must be ready and belong to the scope's keyboard. It may belong to another physical member of
that keyboard, accommodating controller-mediated access. A keyboard report does not require a representative
component; even a partially recognized keyboard with no components can have keyboard-scoped evidence.

Accepted provenance records the scope, source endpoint, source attachment generation and unique refresh ID, together
with report metadata. `reporter` names the interpreting implementation/namespace, not an authenticated principal or
backend registry. Only one source/reporter is authoritative per scope at a time; source selection is caller policy.
The cache does not combine competing reports or establish protocol routing authorization.

## Firmware and protocol constraints

`ReportedVersion` carries firmware/protocol kind, issuer namespace and exact value. `CapabilityConstraint` carries a
namespace and opaque expression. Values need not be SemVer, comparable, or even ordered. A reporter must interpret
its own protocol's rules and resolve support before submitting the report; unresolved constraints require `Unknown`.
`Supported` means supported in the reported context, not merely possible if a condition holds someday.

For a synthetic reporter, layer activation might be unsupported for firmware `legacy-r4`, supported for
`release-blue` with protocol `dialect-B`, and unknown when firmware information is absent or unrecognized. The report
can preserve a constraint such as `firmware belongs to blue releases; protocol is dialect-B` under its reporter's
constraint namespace. The core stores that evidence without comparing release names or evaluating expressions.
These are test examples, not claims about any target keyboard.

Report constructors validate nonblank reporter identifiers, namespaces, version values and constraint expressions.
Exact values are retained, including surrounding whitespace in nonblank identifiers. Duplicate firmware/protocol
version keys and duplicate operation entries are rejected, rather than silently applying last-write-wins. Firmware
and protocol facts may share a namespace because their kinds distinguish them. Missing version facts stay absent.

## Discovery, replacement and refresh

`Topology` owns the cache and exposes borrowed immutable `CapabilityView` values. A view is `NotDiscovered`,
`Refreshing` or `Reported`; operation lookup always defaults to unknown. No historical reports are retained behind
a pending refresh, and there is no on-disk cache, timer expiry, automatic discovery or retry scheduler.

1. `begin_capability_refresh(scope, source)` validates live scope, same-keyboard source membership and ready attachment,
   then allocates a process-unique `CapabilityRefreshId`. Failed validation/allocation preserves the existing slot.
2. A successful begin replaces that scope's slot with pending work immediately. Previous support becomes unknown.
   Latest **start** wins per scope, even when two refreshes use the same attachment or different sources.
3. `complete_capability_refresh(token, report)` accepts only the exact pending token, live target/source membership,
   and original ready attachment. It publishes a validated report under the same exclusive topology borrow.
4. Acceptance replaces the entire scope inventory. Omitted operations become unknown; nothing merges with the old
   report or another scope/source. Repeating a completion is stale.
5. `abandon_capability_refresh(token)` removes only matching pending work and leaves support unknown. Stale abandonment
   cannot erase a newer refresh or accepted report. Failed discovery is not an unsupported report.

`CapabilityRefreshToken` is opaque. The unique ID prevents older results from reviving after cache removal, reconnects
or same-attachment supersession. Begin, completion, abandonment and invalidation require exclusive topology access;
future I/O can run outside that borrow and return its token. Do not split validation and publication into separate
public calls. An owned copy of accepted metadata is historical evidence, never an authorization token.

## Invalidation and lifetime

Without a capability-dependency graph, structural/context changes conservatively invalidate the entire keyboard's
accepted reports and pending refreshes. This may cause extra rediscovery but avoids retaining evidence whose hidden
controller/component dependencies are not yet modeled.

| Change through `Topology` | Effect |
|---|---|
| Add a physical member or add/remove a component | Clear that keyboard's reports and pending work |
| Move a physical member between keyboards | Clear both keyboards; moving away and back never revives pending results |
| Retire a physical unit | Clear its former keyboard before removing owned instances |
| Successfully revalidate any endpoint | Clear its keyboard; old evidence cannot transfer onto a new attachment |
| Disconnect, suspend or protocol failure | Clear its keyboard |
| `invalidate_capabilities(keyboard)` | Explicitly clear known firmware/protocol-context changes without a reconnect |
| Permission denied or busy, with no evidence of changed context | Retain accepted support; cancel pending work sourced through that endpoint; show unavailable access |
| Rename or associate a unit with its existing keyboard | Preserve cache and pending work |
| Failed topology edit | Preserve topology and cache |
| Change another keyboard | Preserve unaffected keyboard evidence |

All validation/allocation precedes mutation/invalidation. Invalidating removes pending slots, and refresh IDs never
repeat, so target retirement or membership changes cannot revive old results. There is no disconnected-device history.

Permission/busy reasons describe access loss without evidence of changed context. A caller that learns firmware or
protocol context changed must explicitly invalidate, even if access is also denied. The generic model cannot detect
that change from opaque version strings. Revalidating a temporarily inaccessible source clears its retained evidence
and requires rediscovery; it does not silently mark old support current again.

## Verification and next layers

[`src/model/capability/tests.rs`](../../src/model/capability/tests.rs) exercises partial support, backend-resolved
non-SemVer restrictions, scope isolation, source validation, complete replacement, refresh ordering, abandonment,
permission/busy retention and structural/context invalidation. Existing topology tests remain applicable.

The [implemented backend contract](backends.md) publishes discovered reports through real refresh tokens, including
supersession/abandonment and keyboard scopes without components. Its read interfaces use their driver's current
interpreted support evidence; the topology cache remains the application's published view. Discovery scheduling,
full normalized state, additional payloads, timestamps, snapshots and committed events remain separate work.
See the [domain model](model.md),
[ADR 0002](adrs/0002-device-model.md) and [ADR 0004](adrs/0004-state-and-events.md).
