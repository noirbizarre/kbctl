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
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Architecture

See the [architecture overview and decision records](docs/architecture/README.md) for the status-first design,
execution modes, device model and hardware research. These describe the intended architecture; device backends
and the user daemon are not implemented yet.

## License

MIT — see [LICENSE](LICENSE).
