# crates/email-agent — IMAP + Telegram Approval

> **Full context**: [AGENTS.md](AGENTS.md)

Background service: IMAP polling → parse email → create inquiry → offer → Telegram approval → SMTP send.

**Runs in production**, spawned as a background task inside the `aust_backend` process (`src/main.rs`). Because it shares the process, a panic here aborts the whole backend. Inquiries also arrive via the web forms and the admin dashboard.

**Key**: Customer email comes from parsed form data, NOT the IMAP sender.

See [AGENTS.md](AGENTS.md) for: JSON field mappings, state management, external connections.