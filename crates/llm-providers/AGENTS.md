# crates/llm-providers — Pluggable LLM Abstraction

Trait-based LLM provider interface. Used for Telegram offer editing (natural language → structured overrides), the email responder, and vision estimation.

## Trait

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, messages: &[LlmMessage]) -> Result<String, LlmError>;
    async fn analyze_image(&self, image_data: &[u8], mime_type: &str, prompt: &str) -> Result<String, LlmError>;
}
```

`LlmMessage { role: LlmRole, content: String, images: Vec<String> }` — `images` holds
base64-encoded images (no data-URI prefix), skipped during serialization when empty.

**`images` is only honoured by `OllamaProvider::complete`.** `ClaudeProvider::complete`
and `OpenAiProvider::complete` build their request from `content` alone and silently
drop `m.images` for every message — multi-turn vision through `complete` only works
against Ollama. Claude and OpenAI vision goes through `analyze_image` instead, which
all three real providers implement (single image + prompt, no conversation history).

## Provider Details

| Provider | API | Vision via `complete()` | Vision via `analyze_image()` |
|----------|-----|--------------------------|-------------------------------|
| Claude | Anthropic Messages API | No (images dropped) | Yes (base64) |
| OpenAI | Chat Completions | No (images dropped) | Yes (base64) |
| Ollama | Local HTTP | Yes (`images` field per message) | Yes (base64) |
| MockLlmProvider | Returns the configured string for both methods | — | — |

## Factory

```rust
let provider = create_provider(&llm_config)?; // NOT async — picks based on default_provider
```

`LlmConfig.default_provider` selects: `"claude"`, `"openai"`, or `"ollama"`. Config: `AUST__LLM__DEFAULT_PROVIDER`.
Each branch requires its own sub-config to be present (`config.claude`/`config.openai`/`config.ollama`) or returns `LlmError::Configuration`. Ollama uses `OllamaProvider::with_api_key(...)` when `ollama.api_key` is set and non-empty, else `OllamaProvider::new(...)` (self-hosted, no auth).

Called once at startup in `src/main.rs`; the resulting `Arc<dyn LlmProvider>` is cloned into the email agent, offer generator, and volume estimator. Note: the Telegram assistant (Josie) uses a separate, hardcoded Ollama Cloud provider configured directly in `crates/assistant`, not this factory.

## Types

- `LlmMessage` — `{ role: LlmRole, content: String, images: Vec<String> }`, with `system()`/`user()`/`user_with_images()`/`assistant()` constructors
- `LlmRole` — System, User, Assistant
- `LlmError` — Configuration or API errors
