-- Enable pgvector extension for embedding storage (768-dim, embeddinggemma:300m).
-- Idempotent — safe to run against databases that already have the extension.
CREATE EXTENSION IF NOT EXISTS vector;
