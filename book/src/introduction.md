# Introduction

> Orchestraitor - An agent harness with trust issues.

Orchestraitor is a **local-first, security-first coding-agent harness and control plane** secured by [Arbitraitor](https://github.com/arbsec/arbitraitor). It combines orchestration, provider/harness adapters, contextual token optimization, and a native developer experience — with every security primitive delegated to Arbitraitor.

Its first product axis is a bounded, self-improving delivery loop: work items live on a kanban board, a fresh manager session selects the next eligible task, a worker implements it in an isolated workspace and opens a pull request, adversarial review converges on the result, and merges of security-sensitive changes are human-gated. Orchestraitor's own backlog is the loop's first and continuous workload (self-hosting).

**Status:** MVP implementation in progress — early Rust crates exist for selected subsystems; no tagged release yet. See the [README](https://github.com/arbsec/orchestraitor#status) for what ships today.

- [Product specification](https://github.com/arbsec/orchestraitor/blob/main/docs/spec/spec.md)
- [Technology stack](https://github.com/arbsec/orchestraitor/blob/main/docs/spec/tech-stack.md)
- [Contributing guide](https://github.com/arbsec/orchestraitor/blob/main/CONTRIBUTING.md)
