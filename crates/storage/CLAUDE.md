# crates/storage — S3 File Storage

> **Full context**: [AGENTS.md](AGENTS.md)

StorageProvider trait (upload, download, delete, exists). S3 and Local implementations.

Key convention: `offers/{uuid}/angebot.pdf`, `estimates/{id}/images/{idx}.jpg`, `employees/{id}/arbeitsvertrag.pdf`.

Orphan handling: inquiry hard-delete logs all failed S3 deletions.

See [AGENTS.md](AGENTS.md) for: trait signature, S3 key convention, LocalStorage setup.