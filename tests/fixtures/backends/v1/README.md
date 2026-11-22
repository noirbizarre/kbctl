# Backend protocol fixtures, schema v1

All current JSON files under `synthetic/` are generated examples, **not hardware captures**. Their protocol is the
toy serial v1 codec in `src/backends/fake.rs`. Fixture schema version and protocol version are separate concepts.

| File | Scenario |
|---|---|
| `synthetic/battery.json` | Handshake, capability query and battery read |
| `synthetic/firmware.json` | Capability query and variable-length firmware reply |
| `synthetic/event.json` | Battery event while a firmware reply waits on a manual gate |
| `synthetic/malformed.json` | Complete response with invalid battery percentage |
| `synthetic/truncated.json` | Incomplete response and an explicit pending receive tail |

`test_support::fixture::CORPUS` registers the files with `include_str!`. Ordinary tests validate every entry and check
that every synthetic v1 JSON file is registered. See [backend testing](../../../../docs/backend-testing.md) and
[hardware/capture guidance](../../../../docs/hardware-testing.md).

## Document

The loader rejects unknown fields/tags and unsupported schema versions. Required fields:

- `schema_version`: integer `1`.
- `id`: stable nonblank fixture identity; mutations retain this base ID.
- `origin`: tagged object with `kind: "synthetic"` or `"captured"`, `procedure` and `sanitization`; captured also requires
  `source`. All statements must be nonblank. Validation is not proof of sanitization or hardware compatibility.
- `device`: nonblank `model`, `revision`, `firmware` labels.
- `protocol`: nonblank `id` and `version` labels. An adapter checks the exact expected protocol/version before replay.
- `purpose`: nonblank scenario intent.
- Optional `unknown_metadata_reason`: required when a version/revision label is the reserved string `"unknown"`.
- `endpoints`: 1..=64 entries with distinct nonblank `alias`, `transport` (`"serial"`/`"hid"`), `gates`, `idle` and `steps`.

Endpoint aliases resolve through test-built topology and never contain fabricated opaque runtime IDs. Device labels
are context, not registry selection rules or capability claims.

## Endpoint script

`gates` declares unique nonblank names. A test can release one explicitly; completing a step can release one too.
`idle` explicitly lists operations allowed to remain pending once their lane is empty. Unlisted exhausted operations
are sticky failures. `steps` has at most 8192 entries with distinct IDs and these fields:

| Field | Meaning |
|---|---|
| `id` | Stable checkpoint/diagnostic label |
| `op` | `serial_read`, `serial_write`, `hid_input`, `hid_output`, `hid_get_feature`, `hid_set_feature` |
| `report_id` | Required for HID output/get/set feature, absent for other operations |
| `after` | Optional declared gate that must be released before this step progresses |
| `releases` | Optional declared gate released only when this step completes |
| `outcome` | Tagged outcome object below |

Outcome objects:

- `{"kind":"input","bytes":[1,2],"chunk":1}`: data, with optional positive serial chunk limit.
- `{"kind":"output","bytes":[16,0],"accept":[1,1]}`: exact logical output with optional acceptance counts. Default
  progress accepts all offered matching data; explicit counts cannot exceed offered/remaining bytes.
- `{"kind":"failure","code":"disconnected","terminal":true}`: typed error; code is `disconnected`, `access_denied`,
  `busy` or `other`. `terminal` defaults false for a one-operation fault.
- `{"kind":"eof"}`: serial read EOF, persistent until a new channel is created.

Input/output byte arrays are at most 65536 bytes, with values in 0..=255. Serial input/output steps cannot use empty
arrays as a substitute for pending or EOF. There are no serial-read steps after EOF. HID input remains one report;
chunking/partial report outcomes are rejected. HID input contains exactly the supplied bytes; explicit report arguments
remain separate. No platform-specific report-prefix normalization is implied.

Steps are ordered within operation lanes, not one global duplex order. Gates express dependencies between writes
and responses. Gate validation does not schedule time or prove progress: manual gates need explicit release. Empty
serial reads are pending only when declared idle, not automatically disconnected or malformed.

Bad protocol payloads are legitimate negative fixtures. The loader validates script structure and bounds, not battery
values, checksums or codec message types. It never repairs a malformed/truncated frame.

## Toy serial protocol v1

Protocol ID is `kbctl.synthetic.serial`, version `1`. Every inbound frame is a two-byte little-endian payload length
followed by payload bytes. Length must be nonzero and within the configured decoded-message bound. The decoder retains
partial headers/payloads per endpoint and does not equate serial read boundaries with frames.

Outbound commands are `[opcode, 0]`: `0x10` reads battery, `0x11` reads firmware, `0x12` queries capability metadata.
The writer tracks accepted offset and never completes a command on a short/zero write.

Relevant payloads:

| Bytes (hex) | Meaning |
|---|---|
| `7e` | Handshake |
| `90 <percentage>` | Battery response |
| `91 <UTF-8 version...>` | Firmware response |
| `92` | Profile's interpreted capability report |
| `a0 <percentage>` | Unsolicited battery report with unknown charging |

Other toy payloads support specialized normalized-state/negative tests in the Rust adapter. These are not vendor
protocol codes. A capability marker uses the selected fake profile's report, not a real serialized capability protocol.

## Mutations and adding data

Typed helpers clone a fixture, preserve its ID, annotate the mutation and revalidate the schema. They fragment/coalesce
ready input, change write acceptance, replace bytes, truncate input, and insert EOF/disconnect after a checkpoint.
They do not cross synchronization boundaries when coalescing. Terminal insertion cuts that operation lane's later
steps; truncating all serial bytes models silence, not an empty successful read.

Add real data only with captured provenance and the review described in the hardware guide. Keep old versions and
document transformations rather than overwriting historical meaning. No capture command, device autodetection or
hardware-in-the-loop success is implied by a replayable fixture.
