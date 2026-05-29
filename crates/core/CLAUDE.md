# crates/core — Domain Models & Config

> **Full context**: [AGENTS.md](AGENTS.md)

Shared foundation crate. Domain models, config, error types, plus the
service-trait bundle and a sqlx-backed `domain_events` emitter/consumer pair
that the assistant subsystem dispatches on.

**Key**: `InquiryStatus` state machine (`can_transition_to`), `CompanyConfig` pricing constants, `Services` struct, `EstimationMethod` enum, `services::ServiceBundle` (`InquiryService`/`OfferService`/… traits), `events::{EventEmitter,EventConsumer}`.

**Note**: the `events` and `services` modules pull `sqlx` into core; the older "no DB / no I/O" framing in older docs is stale.

See [AGENTS.md](AGENTS.md) for: full model list, config fields, status machine diagram.