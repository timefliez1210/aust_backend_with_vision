# crates/core — Domain Models & Config

> **Full context**: [AGENTS.md](AGENTS.md)

Shared foundation crate. Domain models, config, error types. No DB, no HTTP, no I/O.

**Key**: `InquiryStatus` state machine (`can_transition_to`, `is_locked_for_modifications`), `CompanyConfig` pricing constants, `Services` struct, `EstimationMethod` enum.

See [AGENTS.md](AGENTS.md) for: full model list, config fields, status machine diagram.