# crates/llm-providers — Multi-Provider LLM Abstraction

> External service map (provider selection, primary = Claude for German): [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md#external-service-dependencies)

Pluggable LLM backend with a unified trait interface. Supports Claude, OpenAI, and Ollama.

## Key Files

- `src/lib.rs` - `LlmProvider` trait, provider factory
- `src/claude.rs` - Anthropic Claude implementation
- `src/openai.rs` - OpenAI GPT implementation
- `src/ollama.rs` - Local Ollama implementation

## LlmProvider Trait

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, messages: &[LlmMessage]) -> Result<String, LlmError>;
    async fn complete_with_images(&self, messages: &[LlmMessage], images: &[Vec<u8>]) -> Result<String, LlmError>;
}
```

## Types

- `LlmMessage` — (role: LlmRole, content: String)
- `LlmRole` — enum: System, User, Assistant
- `LlmError` — Configuration or API errors

## Provider Details

| Provider | API | Vision | Best For |
|----------|-----|--------|----------|
| Claude | Messages API | Yes (base64) | German language, primary |
| OpenAI | Chat Completions | Yes | Alternative |
| Ollama | Local HTTP | Limited | Privacy, offline |

## Factory

```rust
let provider = create_provider(&llm_config).await?;
```

Selects implementation based on `llm_config.default_provider` ("claude"/"openai"/"ollama").

## Configuration

Uses `LlmConfig` with provider-specific sub-configs:
- `default_provider` — which to use
- `claude.api_key`, `claude.model`
- `openai.api_key`, `openai.model`
- `ollama.base_url`, `ollama.model`
