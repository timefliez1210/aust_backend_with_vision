# crates/email-agent — IMAP + Telegram Approval

> **Full context**: [AGENTS.md](AGENTS.md)

Background service: IMAP polling → parse email → create inquiry → offer → Telegram approval → SMTP send.

**Currently not deployed in production**. Customer inquiries enter via web form or admin dashboard.

**Key**: Customer email comes from parsed form data, NOT the IMAP sender.

See [AGENTS.md](AGENTS.md) for: JSON field mappings, state management, external connections.