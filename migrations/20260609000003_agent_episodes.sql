-- Episodic memory: one-line event log with vector embeddings for similarity retrieval.
--
-- Each episode captures a meaningful event in the assistant's lifetime (tool execution,
-- decision, observation). The 768-dim embedding (embeddinggemma:300m via Ollama) enables
-- semantic similarity search at retrieval time, boosted by recency.
--
-- `refs` holds structured references: {"inquiry_id": "...", "customer_id": "..."}
-- `tags` is a free-form text array for thematic grouping (used by consolidation job).

CREATE TABLE agent_episodes (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- One-line human-readable summary of the event.
    summary     TEXT NOT NULL,
    -- 768-dimensional embedding from embeddinggemma:300m.
    embedding   vector(768),
    -- Thematic tags for grouping (e.g. {"offer", "customer:abc"}).
    tags        TEXT[] NOT NULL DEFAULT '{}',
    -- Structured entity references for fast scoped retrieval.
    refs        JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- HNSW index for fast approximate nearest-neighbour search (cosine distance).
-- ef_construction=128, m=16 — sensible defaults for <1M rows.
CREATE INDEX idx_agent_episodes_embedding ON agent_episodes
    USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 128);

CREATE INDEX idx_agent_episodes_created_at ON agent_episodes(created_at DESC);
CREATE INDEX idx_agent_episodes_tags ON agent_episodes USING gin(tags);
