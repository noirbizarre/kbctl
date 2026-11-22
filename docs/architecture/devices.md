# Hardware evidence and architectural scenarios

This is feasibility research, not a supported-device list. Sources were reviewed on **2026-11-20**.
No KBCTL backend or physical-device validation accompanies this document. Source claims apply to their particular
firmware, revisions and connection modes; a product's typing connectivity does not establish management connectivity.
The [implemented domain model](model.md) tests proposed topologies with synthetic discovery data. Issue #8 adds a
[metadata registry](registry.md) with source-backed identification definitions and explicitly qualified fixtures.
Issue #11 adds [input metadata discovery](../input-discovery.md); its synthetic tests do not add physical-device validation.

## Evidence matrix

| Device | Source evidence | Proposed model and status coverage | Remaining verification |
|---|---|---|---|
| Glorious GMMK Pro | [QMK's rev1 ANSI definition][qmk] contains USB identifiers, an encoder and RGB matrix support. This describes a QMK target, not the stock management protocol. | One logical keyboard with a main component; group host interfaces by physical-device evidence. Discovery/access status is the baseline; advertise additional observations only after verifying the installed firmware's protocol. | Stock firmware's safe management queries, interface selection, individual identity, other revisions and ISO variants. QMK support does not establish VIA support or stock-firmware telemetry. |
| Dygma Defy | [Dygma's product page][defy] describes a split keyboard, Neuron, USB/Bluetooth/RF connectivity and per-half batteries on wireless models. [Bazecor][bazecor] explicitly requires USB serial for its communication protocol. | One logical keyboard with left/right components and a controller component when identifiable. A Neuron endpoint may serve multiple components. Battery and firmware observations are conditional on verified queries; wired variants need no invented battery. | Exact safe command subset, component identity, partial-half reporting, firmware versions and management availability in each connection mode. An internal RF link is not a separate host transport API. |
| Naya Create | Independent [nayactl][nayactl] documents USB CDC, half/dongle discovery, module status and an example of half/module battery and firmware readings. [OpenFlow][openflow] describes a serial port per USB-connected half and module integration. | Associate halves only with evidence of their relationship; represent modules as child components with optional battery/firmware capabilities. Multiple host endpoints may belong to one logical keyboard. | Validate protocol framing/queries, stable pairing identifiers, dongle routing, module attachment/removal and firmware-specific coverage. Do not assume a management API over Bluetooth. |

The QMK definition is upstream firmware evidence. Bazecor is vendor-maintained software evidence. nayactl and
OpenFlow are independent community implementation evidence, not manufacturer specifications; OpenFlow vendors
nayactl, so they are not two independent confirmations of the wire protocol. nayactl's example names firmware
3.30.1 and module firmware 2.2.2; that is an example, not a validated KBCTL compatibility range.

Naya's former product URL now serves a notice from a new domain owner. It is not used as a historical product
specification. Existing firmware and locally available configuration tools remain outside KBCTL's ownership.

## Source-backed identification definitions

The built-in registry records these exact identifiers from the pinned sources. They are identification data, not an
access/permission allowlist, proof of installed firmware or a supported-device list.

| VID:PID | Source mapping | Evidence and limits |
|---|---|---|
| `320f:5044` | GMMK Pro rev1 ANSI identifier | [QMK target definition][qmk], upstream firmware source; no stock/QMK/VIA installation inference or verified management interface/usage selector |
| `35ef:0010` | Defy wired runtime | [Vendor wired mapping][defy-wired]; USB serial reference remains research-only |
| `35ef:0011` | Defy wired bootloader | [Vendor wired mapping][defy-wired]; distinct bootloader classification, no runtime management candidate |
| `35ef:0012` | Defy wireless runtime | [Vendor wireless mapping][defy-wireless]; no Bluetooth management or firmware-version guarantee |
| `35ef:0013` | Defy wireless bootloader | [Vendor wireless mapping][defy-wireless]; no runtime management candidate |
| `37d1:0064` | Naya Create left | [Community constants][naya-constants]; no verified pairing identity |
| `37d1:00c8` | Naya Create right | [Community constants][naya-constants]; no automatic association with a left half |
| `37d1:012c` | Naya Create dongle | [Community constants][naya-constants]; routing and firmware coverage still need verification |

[nayactl's discovery implementation][naya-discovery] filters serial ports by VID/PID and records a serial number when
present. It does not require a particular serial value or establish uniqueness/pairing. The registry preserves that
optional advertised fact separately from verified hardware identity and does not copy first-device-by-side selection.

All built-in Defy/Naya serial protocol references are research-only; GMMK has no management selector. Built-in
definitions create no executable hardware backend or capability claims. Their expected component templates describe
proposed topology, not observed presence. The original model fixtures use synthetic identifiers; registry fixtures
exercise the source-backed ID table with separately labeled synthetic interface/channel descriptors.

## Concrete scenarios

### GMMK Pro: recognized hardware, unknown firmware protocol

Linux metadata can identify a candidate keyboard without permitting active management queries. Publish discovery
and access information, with protocol support unknown or unsupported as appropriate. Do not infer live layer/RGB
state from QMK build definitions, probe arbitrary report IDs, or ask the user to flash firmware to obtain support.
Multiple interfaces from one keyboard must not appear as multiple logical keyboards.
The domain fixture groups those interfaces under one host-discovered physical-device record.

### Defy: one half unavailable

If a verified controller query reports only the left half reachable, keep the logical device and controller visible.
Mark the right component unavailable and its previous observations stale. Report the left battery separately rather
than averaging the halves or returning zero for the missing one. If the protocol cannot distinguish absence from
an unanswered query, report unknown reachability instead. A Bluetooth typing connection alone does not imply that
the USB serial management endpoint is available.
The domain fixture represents a discovered Neuron as the physical unit and its controller/hidden halves as components.
If the host independently discovers other units in a verified mode, they receive their own physical-device records.

### Naya Create: module removed and a second pair attached

A verified module-removal observation retires that module instance and removes its public object. Its half remains
present. When a second pair is attached, correlate halves using verified identifiers rather than side labels,
VID/PID or discovery order. If association is ambiguous, expose separate partial logical devices with that limitation
instead of combining unrelated halves. A reattached module gets a new runtime instance; delayed results for its old
attachment cannot populate the new one.
The domain fixture explicitly associates two discovered physical halves into one keyboard and represents hidden modules
as component children of their half. Independently discovered modules would instead receive physical-device records.

## Backend admission work

Before claiming a combination supported, capture its firmware/version evidence, endpoint matching and identity
rules, allowlisted non-mutating queries, expected response bounds, and disconnect/error behavior. Confirm that
opening the transport does not reset the device or disrupt normal input. Keep protocol captures free of key events
and unnecessary identifiers. Unknown firmware may be discoverable without enabling active queries.

The registry now has the identification list above. A safe-query/access allowlist, installation rules, polling intervals
and concrete OS transport adapters still require verified backend/hardware work described in the [overview](README.md).

## Sources

Repository references are pinned to the revisions reviewed. Product pages can change.

- [QMK: GMMK Pro rev1 ANSI hardware definition][qmk].
- [Dygma: Defy product specifications][defy].
- [Dygma: Bazecor README, USB serial requirement][bazecor].
- [Dygma: Defy wired runtime and bootloader mapping][defy-wired].
- [Dygma: Defy wireless runtime and bootloader mapping][defy-wireless].
- [nayactl: README, status example and protocol-reference links][nayactl].
- [nayactl: USB identifier constants][naya-constants].
- [nayactl: serial-port discovery and optional serial metadata][naya-discovery].
- [OpenFlow: README, topology and vendored nayactl attribution][openflow].

[qmk]: https://github.com/qmk/qmk_firmware/blob/80973aa0b5413e9a5bbbf9293507f39e870f9aa3/keyboards/gmmk/pro/rev1/ansi/keyboard.json
[defy]: https://dygma.com/pages/defy
[bazecor]: https://github.com/Dygmalab/Bazecor/blob/f49f25b11ce21ec22cd8fd6ad44410d749793c07/README.md
[nayactl]: https://github.com/Qonfused/nayactl/blob/8049c9f94d4df9f9c432574f1ae68b10578c38ef/README.md
[openflow]: https://github.com/create-collective/openflow/blob/7c622cae903d01db473b8d888ccf0a023c60bd5f/README.md
[defy-wired]: https://github.com/Dygmalab/Bazecor/blob/f49f25b11ce21ec22cd8fd6ad44410d749793c07/src/api/hardware-dygma-defy-wired/index.ts
[defy-wireless]: https://github.com/Dygmalab/Bazecor/blob/f49f25b11ce21ec22cd8fd6ad44410d749793c07/src/api/hardware-dygma-defy-wireless/index.ts
[naya-constants]: https://github.com/Qonfused/nayactl/blob/8049c9f94d4df9f9c432574f1ae68b10578c38ef/src/nayactl/constants.py
[naya-discovery]: https://github.com/Qonfused/nayactl/blob/8049c9f94d4df9f9c432574f1ae68b10578c38ef/src/nayactl/discovery.py
