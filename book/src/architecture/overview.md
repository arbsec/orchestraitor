# Architecture Overview

Orchestraitor is a Rust workspace secured by [Arbitraitor](https://github.com/arbsec/arbitraitor). The trusted control plane owns orchestration, provider/harness adapters, context compilation, and developer experience. Arbitraitor owns every security decision — sandboxing, policy, approvals, provenance, output promotion, and receipts.

See the [full specification](../../../docs/spec/spec.md) for the authoritative design.
