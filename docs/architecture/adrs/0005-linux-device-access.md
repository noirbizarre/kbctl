# ADR 0005: Unprivileged Linux access and non-mutating status queries

Status: Accepted design; implementation pending.

## Context

Linux device discovery does not imply permission to open a node. HID and serial access differ, desktop sessions and
headless users receive different access, and other software may already own a protocol channel. A session daemon
does not solve these issues by itself.

## Decision

Run both hosts as the invoking user, with no privilege escalation. The user daemon has exactly the same OS-level
access as direct operation. D-Bus exposes normalized status, not arbitrary device handles or a privileged proxy.
The session bus is a same-user integration boundary, not isolation against other applications of that user.

### Discovery and permissions

Use Linux discovery metadata and hotplug monitoring rather than scanning guessed node numbers. The kernel's
[hidraw documentation][hidraw] describes raw HID reports and dynamically allocated nodes; [udev][udev] manages
node metadata, permissions and events. Support `/dev/hidraw*` and USB serial endpoints such as `/dev/ttyACM*` where
verified backends need them. Do not detach keyboard kernel drivers or seize normal input interfaces.

Installation policy must narrowly match the verified model/interface, not every keyboard, HID device, serial port
or device of a vendor indiscriminately. On logind desktops, appropriately ordered `uaccess` rules can grant access
to the active local seat user. On headless/SSH sessions or non-logind systems, an administrator must deliberately
provision a restricted group or ACL. Group names and rule ordering vary by distribution; `dialout` is not universal.
Broad serial/input groups may grant more access than this application needs.

Exact rules require verified identifiers and interfaces and are deferred to backend packaging. KBCTL must not install
rules silently, recommend world-writable `0666` nodes, or require running the application as root. Administrative rule
installation is distinct from application execution. After an access change, revalidate/reopen handles and replug or
refresh as the installation procedure requires; do not treat a cached permission check as authoritative.

For a denied open, report the affected endpoint and operation, whether discovery still succeeded, and how to check
the installed device-specific rule or configured group/seat access. For contention, advise closing/releasing the
configuration tool and retrying. Report unsupported firmware separately from permission failure.

### Safe status access

Status-first means semantically non-mutating operations, not necessarily opening a node read-only. Serial status
requests and HID feature queries may require outbound bytes and write-capable handles. Backend admission requires
an allowlist of known-safe requests for recognized firmware, bounded response sizes, timeouts and retries.
Unknown protocols remain metadata-only candidates rather than receiving exploratory packets.

Opening/closing serial ports may change control lines or reset some devices. A backend must verify safe open settings
before enabling queries; read-like command names alone do not establish safe behavior. Avoid unnecessarily frequent
polling or waking sleeping devices. Unexpected replies fail that query rather than triggering a sequence of guesses.

Do not write configuration, change pairing, reboot into bootloaders, flash firmware, enable keyscan streams or expose
raw protocol commands. Keep keyboard events and unnecessary unique identifiers out of routine logs. Existing firmware
and vendor tools remain responsible for layout/configuration and updates.

### Ownership and coexistence

[ADR 0001](0001-execution-model.md)'s held lock prevents cooperating same-user KBCTL hosts from opening competing
sessions. It is not system-wide hardware exclusivity: another Unix user, vendor tool or system probe may still open
the same endpoint. Use transport-specific exclusive access where verified and available, but never claim it covers
unrelated interfaces, every driver or every other application.

Simultaneous use with vendor configuration tools is not guaranteed. Before configuration, stop the KBCTL daemon or
finish direct observation so it closes endpoints; after configuration, restart/retry and renegotiate capabilities.
KBCTL never kills another tool or forcibly takes over its channel. An explicit future release/reacquire API can be
added when needed. Treat transient contention as unavailable status with bounded retries, not an invitation to reset
hardware. Device-specific interference from system services, such as serial probing, requires a narrowly justified
packaging rule rather than disabling that service globally.

## Alternatives and trade-offs

- **Root/system daemon:** could coordinate users and bypass some permission errors, but expands privilege and policy
  requirements for an application that only needs narrow device access.
- **Privileged helper:** can isolate privileged opens, but adds an authorization protocol and deployment complexity.
  Reconsider only if narrow device permissions prove insufficient for a verified requirement.
- **World-writable nodes or broad default groups:** easy setup at the cost of unnecessary access to unrelated users
  or devices. Prefer device-specific rules and deliberate headless provisioning.

## Consequences and follow-up

Installation may require one-time administrative provisioning; runtime does not. Access can vary across seats,
sessions and distributions and must remain an explicit state. Hardware validation must cover normal input preservation,
transport open/close effects, permission loss and vendor-tool contention before support is advertised.

[hidraw]: https://docs.kernel.org/hid/hidraw.html
[udev]: https://www.freedesktop.org/software/systemd/man/latest/udev.html
