# crates/llm-providers — LLM Abstraction

> **Full context**: [AGENTS.md](AGENTS.md)

Pluggable LLM trait: Claude, OpenAI, Ollama, MockProvider. Used for Telegram offer editing.

`LlmProvider::complete(messages)` and `complete_with_images(messages, images)`. Factory via `create_provider(&config)`.

See [AGENTS.md](AGENTS.md) for: trait details, provider comparison, config.