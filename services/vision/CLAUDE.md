# services/vision — Python ML Pipeline (GPU)

> **Full context**: [AGENTS.md](AGENTS.md)

FastAPI service for 3D volume estimation: photo, depth, video, AR per-item.

**Two pipelines**: Photo (DINO→SAM2→Depth→OBB), Video (keyframes→MASt3R→SAM2→OBB). Deployed on Modal (serverless L4 GPU).

See [AGENTS.md](AGENTS.md) for: file map, estimation methods, deployment, API endpoints, config, model inventory.