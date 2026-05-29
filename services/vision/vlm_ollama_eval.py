"""Standalone Modal eval: run local VLMs (via Ollama) on the FULL freestyle
photo set + RE catalogue, doing the end-to-end dedup -> catalogue-map -> volume
task. Compares local models (gemma4:e4b, qwen2.5-vl:7b) against the Haiku
baseline (17.4 m3 on the same 59-image set).

This is NOT the production pipeline — it's an architecture experiment to measure
how a local VLM does when given FULL images (not crops) and the catalogue.

Run:
    modal run vlm_ollama_eval.py                       # both models, full 59 imgs
    modal run vlm_ollama_eval.py --model gemma4:e4b    # one model
    modal run vlm_ollama_eval.py --smoke               # 2 imgs, confirm multi-image works
"""
from __future__ import annotations

import modal

ollama_image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("curl", "zstd")
    .run_commands("curl -fsSL https://ollama.com/install.sh | sh")
    .pip_install("requests>=2.31", "Pillow>=11", "pillow-heif>=0.18")
)

app = modal.App("aust-vlm-eval")
# Cache pulled Ollama models across runs so we don't re-download every time.
ollama_cache = modal.Volume.from_name("aust-ollama-models", create_if_missing=True)


@app.cls(
    gpu="L4",
    timeout=3000,
    image=ollama_image,
    volumes={"/root/.ollama": ollama_cache},
    scaledown_window=60,
)
class OllamaVLM:
    @modal.enter()
    def start(self) -> None:
        import subprocess
        import time

        import requests

        self._proc = subprocess.Popen(
            ["ollama", "serve"],
            env={"OLLAMA_HOST": "0.0.0.0:11434", "HOME": "/root", "PATH": "/usr/local/bin:/usr/bin:/bin"},
        )
        # Wait for the server to accept connections.
        for _ in range(60):
            try:
                requests.get("http://localhost:11434/api/version", timeout=2)
                break
            except Exception:
                time.sleep(1)
        print("ollama serve up")

    @modal.method()
    def run(self, model: str, images_b64: list[str], prompt: str, num_ctx: int = 32768) -> dict:
        import subprocess
        import time

        import requests

        # Pull (idempotent; cached in the Volume after first time).
        t_pull = time.monotonic()
        subprocess.run(["ollama", "pull", model], check=True)
        ollama_cache.commit()
        pull_s = time.monotonic() - t_pull

        payload = {
            "model": model,
            "messages": [{"role": "user", "content": prompt, "images": images_b64}],
            "stream": False,
            "options": {"temperature": 0, "num_ctx": num_ctx},
        }
        t0 = time.monotonic()
        r = requests.post("http://localhost:11434/api/chat", json=payload, timeout=2400)
        gen_s = time.monotonic() - t0
        r.raise_for_status()
        data = r.json()
        return {
            "model": model,
            "n_images": len(images_b64),
            "num_ctx": num_ctx,
            "pull_s": round(pull_s, 1),
            "gen_s": round(gen_s, 1),
            "eval_count": data.get("eval_count"),
            "prompt_eval_count": data.get("prompt_eval_count"),
            "text": data.get("message", {}).get("content", ""),
        }


PROMPT_HEADER = """You are estimating moving volume for a German moving company, working end-to-end as the whole pipeline.

You are given photos of ONE apartment, taken from many angles for a moving quote, plus the RE volume catalogue below. Each catalogue line is `english_key | German name | volume | flags`. Volume may be fixed "X m3", a "size-variant A-B m3" range, or "X m3 per seat/meter". Flag NOT-MOVED = stays with property (exclude from total). Flag BOX = small, packed into moving boxes (Umzugskarton = 0.1 m3 each).

CATALOGUE:
{catalogue}

TASK — using ALL the photos:
- DEDUPLICATE across photos: the same physical object shown in several photos is ONE object. Use whole-room context to decide same-vs-different.
- For each distinct movable object: pick the best-matching catalogue key, take its volume (for "per seat/meter" estimate the count; for size-variants pick small/large by what you see).
- EXCLUDE NOT-MOVED items from the total; list them separately. Built-in kitchen units (Einbauküche), built-in oven/dishwasher, radiators stay with the property.
- For BOX items, estimate how many Umzugskartons instead of individual volumes.

Return ONLY:

MOVABLE INVENTORY:
<count>x <english_key> (<German name>) — <unit_vol> ea = <line_vol> m3
...

PACKED INTO BOXES:
~<N> Umzugskartons = <N*0.1> m3

NOT MOVED (excluded):
<item> — <reason>

TOTAL MOVABLE VOLUME: <sum> m3
"""


@app.local_entrypoint()
def main(model: str = "both", smoke: bool = False, max_dim: int = 512, num_ctx: int = 32768):
    import base64
    import io
    import json
    from pathlib import Path

    from PIL import Image

    set_dir = Path(
        "services/vision/testsets/019dd584-faa1-72b3-8e0c-53cb097367b7/"
        "019dd585-06ac-7e90-b565-d0c598ac714e"
    )
    if not set_dir.exists():  # allow running from inside services/vision
        set_dir = Path(
            "testsets/019dd584-faa1-72b3-8e0c-53cb097367b7/"
            "019dd585-06ac-7e90-b565-d0c598ac714e"
        )
    files = sorted(set_dir.glob("*.jpg"))
    if smoke:
        files = files[:2]
    print(f"{len(files)} images from {set_dir}")

    images_b64: list[str] = []
    for f in files:
        im = Image.open(f).convert("RGB")
        im.thumbnail((max_dim, max_dim))
        buf = io.BytesIO()
        im.save(buf, format="JPEG", quality=85)
        images_b64.append(base64.b64encode(buf.getvalue()).decode("ascii"))

    catalogue = Path("/tmp/re_catalogue.txt").read_text()
    if smoke:
        prompt = "List every distinct piece of furniture you can see across these images, one per line. State how many separate images you were given."
    else:
        prompt = PROMPT_HEADER.format(catalogue=catalogue)

    models = ["gemma4:e4b", "qwen2.5-vl:7b"] if model == "both" else [model]
    worker = OllamaVLM()
    for m in models:
        print(f"\n{'='*70}\nRunning {m} on {len(images_b64)} images (num_ctx={num_ctx})\n{'='*70}")
        res = worker.run.remote(m, images_b64, prompt, num_ctx)
        print(f"[{m}] pull={res['pull_s']}s gen={res['gen_s']}s "
              f"prompt_tokens={res['prompt_eval_count']} out_tokens={res['eval_count']}")
        print(res["text"])
        out = Path(f"/tmp/eval_{m.replace(':','_').replace('/','_')}.txt")
        out.write_text(res["text"])
        Path(f"{out}.meta.json").write_text(json.dumps(res, indent=2))
        print(f"saved -> {out}")
