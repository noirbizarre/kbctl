<h1 align="center">kbctl</h1>

<p align="center"><strong>A unified control plane for programmable keyboards</strong></p>

<p align="center">
  <a href="https://github.com/noirbizarre/kbctl/actions/workflows/ci.yaml">
    <img src="https://github.com/noirbizarre/kbctl/actions/workflows/ci.yaml/badge.svg" alt="CI">
  </a>
  <a href="https://codecov.io/gh/noirbizarre/kbctl">
    <img src="https://codecov.io/gh/noirbizarre/kbctl/graph/badge.svg" alt="Codecov">
  </a>
  <a href="https://crates.io/crates/kbctl">
    <img src="https://img.shields.io/crates/v/kbctl" alt="crates.io">
  </a>
  <img src="https://img.shields.io/github/v/release/noirbizarre/kbctl" alt="Release">
  <img src="https://img.shields.io/github/license/noirbizarre/kbctl" alt="License">
</p>

---

<!-- One paragraph: what this does and why it exists. Lead with the problem, -->
<!-- not the implementation. -->

## Installation

```bash
cargo install kbctl
```

```bash
brew install noirbizarre/homebrew-tap/kbctl
```

```bash
paru -S kbctl  # or your AUR helper of choice
```

Or download a binary for your platform from the
[latest release](https://github.com/noirbizarre/kbctl/releases/latest).

## Usage

```bash
kbctl --help
kbctl device list
kbctl device inspect usb-15ac7a64ff726c151efde8b1cbf1218e
kbctl status
kbctl indicator status
kbctl indicator status --device usb-15ac7a64ff726c151efde8b1cbf1218e
kbctl --verbose indicator status
kbctl --json indicator watch
```

Use an ID printed by `device list`; the ID above illustrates its format.

On Linux, device commands show compact attachment listings and evidence-qualified selectors. Indicator commands report
Caps Lock, Num Lock and Scroll Lock per attachment, explicitly exposing disagreement or unavailable endpoint state.
Use `--verbose` or `device inspect` for diagnostics. JSON remains complete: colored and pretty on supported terminals,
compact and uncolored when piped. Watch emits an initial snapshot followed by updates until Ctrl-C.
See the [CLI guide](docs/cli.md) for selector stability/fallbacks, grouping, detail and output contracts.

### Host lock state vs. keyboard LED state

KBCTL distinguishes two related but different things:

- **Host lock state** is the logical Caps Lock, Num Lock or Scroll Lock state maintained by the operating system. Changing it affects the host and may affect more than one keyboard.
- **Keyboard LED state** is the physical indicator state reported by, or controllable on, an individual keyboard. It belongs to that device and can differ from the host's logical state.

These states often stay synchronized, but they are not interchangeable. A command that changes the host lock state does not necessarily target a particular keyboard's LED, and changing a device LED does not necessarily change the host's logical lock state. KBCTL keeps the two operations separate so scripts and desktop widgets can choose the intended behavior explicitly. Per-device LED control with `get`, `set` and `toggle` is tracked in [issue #36](https://github.com/noirbizarre/kbctl/issues/36); host lock-state control is tracked separately in [issue #13](https://github.com/noirbizarre/kbctl/issues/13).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Architecture

See the [architecture overview and decision records](docs/architecture/README.md) for the status-first design,
execution modes, device model and hardware research. These describe the intended architecture; device backends
and the user daemon are not implemented yet.

## License

MIT — see [LICENSE](LICENSE).
