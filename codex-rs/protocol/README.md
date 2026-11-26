# codex-protocol

This crate defines the protocol types used by Web2Codex CLI. It includes internal types for communication between `codex-core` and `codex-tui`, as well as external types used with `web2codex app-server`.

This crate should have minimal dependencies.

Ideally, we should avoid "material business logic" in this crate, as we can always introduce `Ext`-style traits to add functionality to types in other crates.
