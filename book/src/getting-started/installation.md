# Installation

> Orchestraitor is pre-implementation. No binary releases are available yet.

## Build from source

```sh
git clone https://github.com/arbsec/orchestraitor.git
cd orchestraitor
cargo build --release
```

The `orc` binary will be at `target/release/orc`.

## Prerequisites

- Rust 1.96.0 (pinned in `rust-toolchain.toml`)
- [Arbitraitor](https://github.com/arbsec/arbitraitor) checked out adjacent for local development (optional)
