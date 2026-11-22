# ADR 0001: Single binary with shared direct and daemon execution

Status: Accepted design; implementation pending.

Issue #7 implements [injected lease ownership and shared backend session cleanup](../backends.md).
Process-lock acquisition, daemon hosting and CLI mode selection below remain pending.
Issue #9 implements the shared [normalized runtime and commit service](../state.md); direct/daemon hosting can build on it.

## Context

Standalone inspection must work without a running daemon or session bus. Long-lived desktop clients need a shared
owner of discovery, polling and state. Duplicating application behavior between these paths would produce different
answers and competing device sessions. The repository requires a binary-only crate and argument-only `cli.rs`.

## Decision

Keep one executable and private modules. A direct command hosts the application service locally; a foreground
`kbctl daemon` hosts the same service for session-bus clients. These commands/options are proposed syntax, not
currently implemented. The daemon opens hardware on behalf of clients and has the same privileges as direct mode.

Proposed CLI mode selection is `--mode=auto|direct|daemon`, defaulting to `auto`:

| Mode | Behavior |
|---|---|
| `direct` | Acquire local ownership and run the service in-process; no bus is required. |
| `daemon` | Connect to the existing compatible daemon; never open hardware locally. |
| `auto` | Prefer an existing daemon; choose direct only when no session bus is configured/available or the bus positively reports no service owner. |

A missing/unreachable local bus socket permits direct selection before acquisition. Authentication failures,
service timeouts, permission errors and incompatible APIs are errors, not evidence of daemon absence. Do not
autostart a service implicitly through D-Bus in the initial design. Once a daemon operation begins, its failure
does not trigger a direct retry. Report how to start the daemon or explicitly choose direct operation.

### Ownership and startup ordering

All hardware-owning KBCTL processes acquire one advisory per-user lock at `$XDG_RUNTIME_DIR/kbctl/owner.lock` before
opening any endpoint, and retain it until their endpoints are closed. Use a user-owned, private runtime directory
and a held OS lock, not a PID-file existence check. Missing/invalid runtime storage is an actionable startup error;
standalone operation requires no bus but still requires safe ownership storage.

The daemon acquires the lock before claiming its well-known bus name. It releases the name and closes hardware
before releasing ownership on shutdown; loss of its bus connection also stops its hardware service. A direct owner
prevents a daemon from starting and vice versa. A second owner fails promptly with a busy diagnostic.

For `auto`, absence is checked before lock acquisition. If acquiring the lock fails, recheck for a compatible daemon:
connect if one is now present, otherwise report busy/startup-in-progress. If acquisition succeeds, recheck any
available bus before opening hardware; a newly found daemon causes release and connection instead. The shared lock
prevents two same-user owners even during startup, when no bus name has been published yet. Never break another
process's lock or delete its lock file to recover from contention.

This lock covers all devices for that user, including when direct mode requests just one keyboard. It does not
coordinate different Unix users or unrelated tools. [ADR 0005](0005-linux-device-access.md) defines these limits.

### Shared operations

The application service owns discovery, status snapshots and live observation. CLI and D-Bus adapters map requests
and responses without reproducing backend or lifecycle policy. The direct host closes endpoints on completion;
watch-style operation retains them until cancelled. The daemon retains them until stopped or access is suspended.

## Alternatives and trade-offs

- **Daemon-only:** simpler ownership and a single cache, but prevents independent troubleshooting without D-Bus.
- **Direct-only:** minimal process management, but each desktop consumer would duplicate discovery and polling.
- **Separate binaries/crates:** can isolate dependencies, but introduces public interfaces and packaging work before
  stable boundaries exist. Reconsider only when concrete reuse or build constraints justify it.
- **Per-device locks:** allow direct operation beside the daemon on different keyboards, but require reliable device
  identity before ownership and complicate multi-endpoint acquisition. Defer until coarse ownership is limiting.

## Consequences

The two hosts can be checked against the same simulated-backend scenarios. D-Bus loss never silently transfers an
in-flight request to another host. Direct mode trades simultaneous independent device sessions for straightforward
ownership. Daemon installation/activation and exact CLI command inventory are later implementation work; the current
systemd user unit remains a placeholder until its command is implemented.
