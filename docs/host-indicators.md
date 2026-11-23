# Host keyboard indicators

Issue [#12](https://github.com/noirbizarre/kbctl/issues/12) adds an internal, read-only Linux input-indicator collector for
Caps Lock, Num Lock and Scroll Lock. It publishes through the existing normalized runtime; queries use `StateReader`
snapshots and changes use its atomic snapshot/subscription API. The [CLI](cli.md) hosts it for read-only status and watch.

## Reporting scope and outcomes

Each collected input endpoint has its own host source, with a typed `InputEndpointId` binding and scope namespace
`linux.evdev.indicators` with the endpoint ID as its value. IDs are process-local. Two endpoints can disagree; the runtime
does not elect a system-wide, seat-wide or compositor-wide winner, merge receiver siblings, or copy values into device LED
state. The CLI adds conservative attachment-group summaries: only fresh, known agreement becomes on/off; disagreement
and mixed evidence remain explicit, with endpoint details available through `--verbose` and JSON.

Linux `EVIOCGLED` reports the input device's kernel-maintained indicator bitmap. It is **not** proof of physical LED
illumination, keyboard firmware state or the active desktop input context. No indicator writes, keymap queries, live key
state queries, input grabs or event injection are implemented here. Indicator control remains issue #13.

Supported-event and LED-code queries establish support independently of current values. For each of the three locks:

| Evidence | Normalized outcome |
|---|---|
| Supported and bit set | `Known(true)` |
| Supported and bit clear | `Known(false)` |
| Successfully established absent support | Per-lock `Unavailable(Unsupported)` |
| Capability/interface query failed | Source unavailable with its actual failure, not an empty support set |
| Open or query denied | Source `PermissionDenied`; old values become stale |
| Node missing/replaced/revoked or unusable | Source inaccessible; old values become stale |

Per-lock unavailable receipts store a stamped unknown value plus a specific failure fact while preserving `last_known`.
`FieldQuality::Failed` therefore includes unsupported/unavailable facts, not just transient I/O errors. Plain unknown is
distinct from unsupported, and neither is false. An accepted known/plain-unknown receipt clears the prior field failure;
an obsolete receipt cannot poison newer knowledge. Failure facts carry their own occurrence time, not a heartbeat.

A working interface with all three indicators unsupported can remain source-active with explicit unsupported fields.
Active registration alone does not establish fresh boolean knowledge: inspect field quality, provenance and deadlines.
Renewal clears old-generation failure facts and requires a new receipt. Input-ID retirement invalidates bound sources in
the same commit, rejecting old applications/renewals even if a node with the same pathname reappears. Collector cleanup
then closes and retires those sources. Discovery uncertainty that retains input IDs does not itself revoke a valid FD.

## Ownership and checked opens

The real Linux opener requires a held `OwnerGuard`. It acquires the shared per-user advisory lock at
`$XDG_RUNTIME_DIR/kbctl/owner.lock` before any indicator node is opened:

- Runtime storage and `kbctl` must be invoking-user-owned private directories (`0700`). Directory-relative no-follow
  traversal rejects unsafe paths; the lockfile must be regular, single-link, owned by that user and mode `0600`.
- `flock` acquisition is nonblocking. An existing empty file is normal; file existence or PID contents are not ownership.
  KBCTL neither truncates/unlinks live lockfiles nor changes permissions to bypass an error.
- Every lease retains the real guard and closes its endpoint before releasing ownership. This coordinates cooperating
  same-user KBCTL processes, not other applications/users or hostile same-user mutations.

Input nodes are opened read-only, nonblocking, close-on-exec and no-follow, without acquiring a controlling terminal.
Admission validates character-device type, actual device number, `/sys/dev/char` attachment mapping and before/after
sysfs incarnation evidence from discovery. A path, VID/PID, product label or advertised serial is not enough.

Discovery remains metadata-only, and its `StreamAccess::NotChecked` stays unchanged. This collector's host-source
availability describes its own access. Access errors retain endpoint, operation and OS cause, with
`kbctl::indicators::access` diagnostics. Ownership errors use `kbctl::ownership::{storage,io,busy}`. Recovery checks the
specific node, runtime storage and deliberately configured seat/ACL policy; broad input-group access, root execution and
world-writable nodes are not default remedies. No access rules are installed.

## Events, initial state and compatibility

On upstream-compatible Linux implementations from 4.4 onward, the adapter prefers per-client LED-only notifications:

1. Validate support with `EVIOCGBIT` using initialized native unsigned-long bitmap storage and checked result lengths.
2. Set an event-type mask allowing `EV_SYN`/`EV_LED`, then an LED-code mask allowing only Num/Caps/Scroll.
3. Select the client's realtime clock and then its monotonic clock. An actual clock change discards the entire unread
   client queue in the audited implementation, including input buffered before filtering.
4. Query an authoritative initial `EVIOCGLED` snapshot.
5. Only after successful initialization, permit event reads and offer a borrowed readiness FD.

Mask setters and clock setters configure this client; they do not change indicators or the system clock. Mask installation
alone does not clear old queued data. `EVIOCGLED` flushes queued LED events only, so it is not a key-backlog purge.

The queue-discard behavior is an **audited implementation dependency**, not a general-purpose purge promise in the UAPI.
The release check and successful ioctls are compatibility preconditions, not attestation of a custom kernel. The supported
environment assumes upstream-compatible mask and clock semantics. Older/unparseable releases or unsupported/denied setup
use ioctl-only snapshots; hard device failures close/fail the lease. Snapshot mode never reads or polls the input queue.

Under that implementation contract, key events/state do not enter application memory. Pre-filter events can briefly
exist in kernel-owned client storage. This is not a sandbox against arbitrary future/downstream kernel behavior. The
adapter has no general evdev synchronizer or constructor that can secretly read key state.

Audit references:

- [`evdev_set_clk_type`, filtering, masks and bitmap queries][evdev] at upstream commit
  `4843f6873f7c4835e62f2c73ba0fd538e57a2f22`.
- [Clock-change queue discard][purge] and [per-client event masks][masks].
- [Linux input event synchronization][syn].

Allowed LED events invalidate cached knowledge; a bounded batch coalesces into one LED snapshot. Their payloads are never
replayed as authoritative lock state. `SYN_DROPPED` immediately marks a source gap, discards pending old receipts, and
ignores events through the next `SYN_REPORT`. The rest of that userspace batch is discarded before a new snapshot is
published under a renewed host generation. Old readiness tokens cannot affect it.

Initialization can itself queue `SYN_DROPPED`; it need not become readable immediately. Initial publication does not wait
for that marker. A recovery boundary that never arrives triggers snapshot fallback after its deadline. Unexpected event
classes or malformed data disable streaming without logging codes, raw payloads or timestamps. Draining a configured
budget is pending work, not overflow or proof that the queue is idle.

## Hosting and scheduling contract

The portable `indicators::Service` owns injected opener/lease objects and per-endpoint lifecycle/publication work. The
real adapter is `platform::linux::indicators::LinuxOpener`, constructed with `Arc<OwnerGuard>`. Default admission requires
a positive typing endpoint or a confirmed, non-pointer/controller sibling advertising standard lock LEDs within a
recognized keyboard attachment. LED support on an unrelated device is insufficient. Partials are excluded by default;
explicit `indicator status --device` selection can admit a positive partial group and limits collection to that group.
Selector resolution uses the full inventory before indicator opens; classification and collection eligibility share the
same domain policy. Known typing keyboards whose indicator interfaces cannot be opened still report actionable outcomes.

Definitive reclassification/scope removal closes leases, discards pending receipts and retires sources even if input IDs
remain unchanged. Re-admission uses fresh generations and schedules immediate work after deferred retirement. Merely
missing/denied classification metadata may preserve an already checked live lease, but never admits a new open or reopens
a failed lease without renewed positive evidence. Unknown groups remain outside default keyboard views. Input-removal
and checked-open binding rules continue to apply independently.

The direct CLI host (and a future daemon host) starts metadata discovery, acquires ownership and calls indicator `step`.
It registers event-mode FDs from `service.leases()` and `lease.registration()`, associating each with its endpoint and
collector generation, then passes matching readiness/HUP/error tokens back to `step`. Registrations are borrowed only
for readiness: do not duplicate, close or reconfigure them outside their lease/ownership lifetime. Re-register changed
generations, including renewal on an existing FD. Snapshot/unsupported modes supply no input-read readiness FD.

Defaults are configurable and validated:

| Policy | Default |
|---|---|
| Event-mode health snapshot | 30 seconds |
| Snapshot fallback sampling | 1 second |
| Sample freshness | Three times that sample's mode interval |
| Failed-open/read retry | Exponential 1–30 seconds |
| Missing synchronization boundary | Downgrade after 1 second |
| Work per turn | 32 endpoints, 64 events per endpoint, 256 events total |

Endpoint rounds rotate their first claimant of the global event budget so a hot source cannot starve others. Pending
work schedules another turn. Receipt time comes from the shared monotonic clock after I/O; publication retries retain
the same sequence, receipt and deadline. No visible commit is required to acknowledge a successful runtime call.
Mode changes never extend an old sample's validity.

The host must schedule the minimum discovery, indicator and runtime deadlines and drive runtime expiry. `step` advances
expiry as it processes work; snapshots alone do not run a timer. Snapshot fallback can delay observations by its sampling
interval and miss changes wholly between samples. Scheduling capacity can also delay a busy host; expiry exposes stale
knowledge instead of extending it silently. Synchronous opens/ioctls are not given fictitious wall-clock guarantees.

Explicit `stop` terminally closes leases and retires sources, retaining unfinished lifecycle work on publication failure.
Subsequent steps finish retirement without reopening sources. Drop closes
resources but does not mutate a borrowed runtime; a surviving host must continue driving expiry for retained reports.
The collector itself introduces no background thread or async runtime. The CLI provides the synchronous readiness and
deadline loop; daemon hosting and D-Bus export remain separate integrations.

## Verification

Hardware-free tests cover initial and changed values, partial support, failure ordering, source disagreement, input
retirement, overflow/recovery, stale readiness, fair budgets, finite freshness and retained retry timestamps. Native
adapter tests use an independent scripted kernel queue, checked bitmap return lengths and ordinary temporary files;
ownership tests include real contention from a child process. No physical input endpoint or root privilege is required.

```sh
mise run test -p kbctl -E 'test(indicators::) | test(ownership::) | test(runtime::)'
```

The Linux-only `nix` dependency needs no additional native library; existing libudev build/cross prerequisites still
apply. Native tests and source inspection establish the implemented contracts, not physical-keyboard interoperability
or desktop-global authority. Hardware testing remains explicitly opt-in.

[evdev]: https://github.com/torvalds/linux/blob/4843f6873f7c4835e62f2c73ba0fd538e57a2f22/drivers/input/evdev.c
[purge]: https://github.com/torvalds/linux/commit/0c3e99437a66e4c869c60c2398449e6d98f3a988
[masks]: https://github.com/torvalds/linux/commit/06a16293f71927f756dcf37558a79c0b05a91641
[syn]: https://docs.kernel.org/input/event-codes.html#ev-syn
