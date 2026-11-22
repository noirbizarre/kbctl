# Opt-in hardware-in-the-loop tests

Regular CI runs without physical keyboards. The deterministic [backend tests](backend-testing.md), scripted transports
and synthetic fixtures are the primary regression suite. No real management transport/backend or hardware test exists yet;
the following convention applies when one is added.

## Adding a real test

Keep it in a private binary unit-test module such as `backends::<backend>::hil`, with an explicit ignore reason:

```rust,ignore
#[test]
#[ignore = "requires explicitly selected physical hardware"]
fn reading_status_preserves_the_existing_configuration() {
    // Resolve the explicitly supplied selector, acquire ownership and use the
    // real adapter/backend. Missing selection/hardware must fail this invoked test.
}
```

This is an authoring sketch, not an implemented test or a successful hardware assertion. Do not add an ignored
placeholder that returns success without exercising an adapter.
Ignored tests still compile: apply the appropriate target `cfg` to platform-specific modules so other CI platforms
can build the regular test target.

A backend-specific selector must identify the intended physical device and endpoint with enough descriptor evidence
to avoid ambiguity. Do not select the first keyboard, rely on discovery order or reinterpret a process-local runtime
ID as persistent identity. Missing selectors, ambiguous selection, missing hardware, inaccessible nodes and unsupported
firmware are failures of an explicitly invoked test, not successful early returns.

Use the established [ownership/access contract](architecture/adrs/0005-linux-device-access.md): retain the host's
process reservation before opening and until every channel is closed, negotiate required exclusive access, bound I/O
and cleanup, and verify safe serial open settings. A fake access grant is not proof of real OS exclusivity.

The initial hardware baseline is allowlisted recognition/status reads that preserve existing firmware/configuration
and normal input. Sending status-request bytes is different from changing configuration. Reset, flashing, keymap,
pairing and lighting modifications are outside this baseline. Test actual cleanup after success/failure and record
which firmware, transport mode and interface were exercised.

## Running an added test

Only after implementing a real backend/test, replace the name below with that test's **actual fully qualified name**
and supply the explicit selector required by it. These are future command patterns, not currently runnable hardware
coverage:

```sh
mise exec -- cargo test --bin kbctl -- --list --ignored
mise exec -- cargo test --bin kbctl 'backends::your_backend::hil::reading_status_preserves_the_existing_configuration' -- --ignored --exact --nocapture
```

Cargo passes arguments after `--` to libtest. Verify that the named ignored test exists and that exactly one test
actually ran; a zero-match filtered invocation is not evidence of interoperability. Ordinary CI must not add
`--ignored`/`--include-ignored` to its default suite. Hardware jobs, if added later, are separate opt-in jobs with
documented device ownership and selection.

## Captures and evidence

Real response fixtures supplement hardware tests and allow the parser/driver regression to run without hardware.
Use the [versioned fixture schema](../tests/fixtures/backends/v1/README.md), record captured origin, procedure/source,
device model/revision, firmware and protocol versions, endpoint/report conventions and scenario intent. Explicitly
label unrecorded metadata with its reason rather than inventing a version or compatibility range.

Before committing traces, remove or replace serials, MAC/peer identifiers, identifying host paths, key events, personal
macros/keymaps and unrelated payloads. Preserve aliases, routing relationships and lengths/checksums required by the
protocol; document any byte transformations. Review both bytes and metadata. A nonblank sanitization statement is
required by the loader but is not an automated privacy review or proof that the data is safe to publish.

Keep captures pinned/versioned and describe their limits. One successful device/firmware/transport combination does
not validate other revisions, bootloaders or wireless management paths. The current corpus is entirely synthetic and
makes none of those claims.
