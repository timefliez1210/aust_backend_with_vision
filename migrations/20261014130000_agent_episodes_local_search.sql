-- Josie's episodic memory: searchable without a cloud embedding API.
--
-- Additive only. `agent_episodes.embedding` (768-dim) was meant for Ollama Cloud
-- embeddings, which Ollama Cloud does not offer — so no episode was ever written
-- or found. Episodes now get
--   * `search_tsv`: German full-text search over the summary (always available), and
--   * `embedding_local`: a 384-dim vector from the in-process multilingual model
--     (paraphrase-multilingual-MiniLM-L12-v2), when that is enabled.
-- The old 768-dim column stays untouched.

ALTER TABLE agent_episodes ADD COLUMN IF NOT EXISTS embedding_local vector(384);
ALTER TABLE agent_episodes
    ADD COLUMN IF NOT EXISTS search_tsv tsvector
    GENERATED ALWAYS AS (to_tsvector('german', summary)) STORED;

CREATE INDEX IF NOT EXISTS idx_agent_episodes_search ON agent_episodes USING GIN (search_tsv);
CREATE INDEX IF NOT EXISTS idx_agent_episodes_embedding_local
    ON agent_episodes USING hnsw (embedding_local vector_cosine_ops);
