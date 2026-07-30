# Installation

> Orchestraitor is pre-release. No binary installers or tagged releases are available yet.
> Build from source against the pinned Rust toolchain.

## Prerequisites

- **Rust 1.96.0**, pinned in [`rust-toolchain.toml`](https://github.com/arbsec/orchestraitor/blob/main/rust-toolchain.toml).
  This matches the Arbitraitor MSRV (spec: Rust conventions). Any `rustup`-managed toolchain
  picks up the pinned channel automatically when you build inside the repository.
- **A C toolchain and linker** suitable for the platform (e.g. `build-essential` on Debian/Ubuntu,
  Xcode Command Line Tools on macOS). Orchestraitor forbids `unsafe` in its own crates where
  avoidable, but transitive native dependencies may require a C compiler.
- **Git**, for cloning and for the worktree-first workflow.
- **[Arbitraitor](https://github.com/arbsec/arbitraitor) checked out locally for development.**
  Orchestraitor depends on Arbitraitor via a pinned git revision (no crates.io publishes exist
  yet — see [tech-stack §2.1](https://github.com/arbsec/orchestraitor/blob/main/docs/spec/tech-stack.md)).
  For local development you provide a path override so Cargo resolves Arbitraitor crates from
  a sibling checkout.

## Build from source

```sh
git clone https://github.com/arbsec/orchestraitor.git
cd orchestraitor
cargo build --release
```

The release binaries are written to `target/release/`:

| Binary | Crate | Purpose |
|---|---|---|
| `orc` | `orchestraitor-cli` | Local control-plane CLI (project init, config, models catalog). |
| `orcd` | `orchestraitor-daemon` | Durable JSON-RPC daemon over a Unix-domain socket. |

Add `target/release` to your `PATH`, or copy/symlink the binaries to a directory already on
it:

```sh
ln -s "$(pwd)/target/release/orc" ~/.local/bin/orc
ln -s "$(pwd)/target/release/orcd" ~/.local/bin/orcd
```

The long-form `orchestraitor` alias is intended to remain available for discoverability where
`orc` conflicts with another installed program (spec §1.2); until a release ships, create the
symlink manually if you need it:

```sh
ln -s "$(pwd)/target/release/orc" ~/.local/bin/orchestraitor
```

## Local Arbitraitor checkout (development)

Orchestraitor's `Cargo.toml` pins Arbitraitor crates to a specific git revision. For local
development against a live Arbitraitor checkout, clone Arbitraitor as a sibling directory and
provide a Cargo `[patch]` override (this file is **not** checked in):

```sh
git clone https://github.com/arbsec/arbitraitor.git ../arbitraitor
```

Then create `.cargo/config.toml` (or `~/.cargo/config.toml`) at the repository root:

```toml
# .cargo/config.toml  (NOT checked in)
# Adjust "../arbitraitor" to wherever you cloned arbsec/arbitraitor locally.
[patch."https://github.com/arbsec/arbitraitor.git"]
arbitraitor-core       = { path = "../arbitraitor/crates/arbitraitor-core" }
arbitraitor-model      = { path = "../arbitraitor/crates/arbitraitor-model" }
arbitraitor-policy     = { path = "../arbitraitor/crates/arbitraitor-policy" }
arbitraitor-sandbox    = { path = "../arbitraitor/crates/arbitraitor-sandbox" }
arbitraitor-exec       = { path = "../arbitraitor/crates/arbitraitor-exec" }
arbitraitor-receipt    = { path = "../arbitraitor/crates/arbitraitor-receipt" }
arbitraitor-mcp        = { path = "../arbitraitor/crates/arbitraitor-mcp" }
arbitraitor-plugin-api = { path = "../arbitraitor/crates/arbitraitor-plugin-api" }
```

With the patch in place, `cargo build` resolves all `arbitraitor-*` crates from the local
checkout. Without it, Cargo fetches the pinned revision from GitHub. The CI "Arbitraitor
bump" job runs the Orchestraitor suite against the latest `main` weekly; red status is
informative, not blocking (tech-stack §2.1).

## Verify the build

```sh
orc --version
orc --help
orcd --help 2>/dev/null || true   # orcd takes a socket path, not --help
```

`orc init --dry-run` is a zero-side-effect way to confirm the CLI works against a real project
without writing any files (spec §9.20):

```sh
orc init --dry-run --project .
```

## Pre-PR gate

Before contributing, the full quality gate from
[tech-stack §15](https://github.com/arbsec/orchestraitor/blob/main/docs/spec/tech-stack.md)
applies. The minimum local check is:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets --all-features --locked
cargo nextest run
```

## Platform support

Orchestraitor targets Linux, macOS, and WSL2 during MVP, with Windows-native as a future
target (spec §9.32, §16.8). Arbitraitor currently documents strong Linux primitives but
incomplete macOS and Windows containment; Orchestraitor must not advertise uniform
cross-platform isolation until Arbitraitor reports equivalent effective controls for the
current platform. See [Arbitraitor Integration](../reference/arbitraitor-integration.md) for
the fail-closed behavior.
