# crates/llm-providers — Pluggable LLM Abstraction

Trait-based LLM provider interface. Used for Telegram offer editing (natural language → structured overrides).

## Trait

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, messages: &[LlmMessage]) -> Result<String, LlmError>;
    async fn complete_with_images(&self, messages: &[LlmMessage], images: &[Vec<u8>]) -> Result<String, LlmError>;
}
```

Note: `complete` takes `&[LlmMessage]` (not just a prompt string). This allows multi-turn conversations and system prompts.

## Provider Details

| Provider | API | Vision | Best For |
|----------|-----|--------|----------|
| Claude | Anthropic Messages API | Yes (base64) | German language, primary |
| OpenAI | Chat Completions | Yes | Alternative |
| Ollama | Local HTTP | Limited | Privacy, offline |
| MockLlmProvider | Returns `{}` | No | Tests |

## Factory

```rust
let provider = create_provider(&llm_config).await?; // picks based on default_provider
```

`LlmConfig.default_provider` selects: "claude", "openai", or "ollama". Config: `AUST__LLM__DEFAULT_PROVIDER`.

## Types

- `LlmMessage` — (role: LlmRole, content: String)
- `LlmRole` — System, User, Assistant
- `LlmError` — Configuration or API errors