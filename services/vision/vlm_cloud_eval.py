"""Ollama Cloud eval: run hosted VLMs (kimi-k2.6, minimax-m3, ...) on the FULL
freestyle photo set + RE catalogue, doing the end-to-end dedup -> catalogue-map
-> volume task.

Same prompt + image prep as vlm_ollama_eval.py (the Modal/local-GPU variant) so
results are directly comparable to the earlier four-way:
    crop pipeline (Qwen on crops)     60.5 m3   ~2x OVER-count
    Haiku (full + catalogue)          17.4 m3
    gemma4:e4b (full + catalogue)     11.1 m3
    qwen2.5vl:7b (full + catalogue)   10.0 m3
    Opus gold standard                ~37 m3 (range 35-42)

Run (from repo root or services/vision):
    .venv-modal/bin/python vlm_cloud_eval.py                      # both models
    .venv-modal/bin/python vlm_cloud_eval.py --model kimi-k2.6    # one model
    .venv-modal/bin/python vlm_cloud_eval.py --smoke              # 2 imgs sanity check
"""
from __future__ import annotations

import argparse
import base64
import io
import json
import sys
import time
from pathlib import Path

import requests
from PIL import Image

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))

from app.models.schemas import RE_CATALOG, RE_M3  # noqa: E402

OLLAMA_URL = "https://ollama.com/api/chat"

PROMPT_HEADER = """You are estimating moving volume for a German moving company, working end-to-end as the whole pipeline.

You are given photos of ONE apartment, taken from many angles for a moving quote, plus the RE volume catalogue below. Each catalogue line is `english_key | German name | volume | flags`. Volume may be fixed "X m3", a "size-variant A-B m3" range, or "X m3 per seat/meter". Flag NOT-MOVED = stays with property (exclude from total). Flag BOX = small, packed into moving boxes (Umzugskarton = 0.1 m3 each).

CATALOGUE:
{catalogue}

TASK — using ALL the photos:
- DEDUPLICATE across photos: the same physical object shown in several photos is ONE object. Use whole-room context to decide same-vs-different.
- For each distinct movable object: pick the best-matching catalogue key, take its volume (for "per seat/meter" estimate the count; for size-variants pick small/large by what you see).
- EXCLUDE NOT-MOVED items from the total; list them separately. Built-in kitchen units (Einbaukueche), built-in oven/dishwasher, radiators stay with the property.
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


def build_catalogue() -> str:
    """Render RE_CATALOG as `english_key | German name | volume | flags` lines."""
    lines: list[str] = []
    for key, entry in RE_CATALOG.items():
        german = entry["german"]
        if "re" in entry:
            vol = f"{entry['re'] * RE_M3:.1f} m3"
        elif "variants" in entry:
            res = [re for _, re in entry["variants"]]
            vol = f"size-variant {min(res) * RE_M3:.1f}-{max(res) * RE_M3:.1f} m3"
        elif "re_per_unit" in entry:
            vol = f"{entry['re_per_unit'] * RE_M3:.1f} m3 per {entry['unit']}"
        else:
            continue
        flags = []
        if not entry.get("moveable", True):
            flags.append("NOT-MOVED")
        if entry.get("packs_into_boxes", False):
            flags.append("BOX")
        suffix = f" | {' '.join(flags)}" if flags else ""
        lines.append(f"{key} | {german} | {vol}{suffix}")
    return "\n".join(lines)


def load_api_key() -> str:
    for env_path in (HERE.parent.parent / ".env", Path(".env")):
        if env_path.exists():
            for line in env_path.read_text().splitlines():
                if line.startswith("AUST__LLM__OLLAMA__API_KEY="):
                    key = line.split("=", 1)[1].strip()
                    if key:
                        return key
    raise SystemExit("AUST__LLM__OLLAMA__API_KEY not found in .env")


def load_images(smoke: bool, max_dim: int) -> list[str]:
    set_dir = HERE / (
        "testsets/019dd584-faa1-72b3-8e0c-53cb097367b7/"
        "019dd585-06ac-7e90-b565-d0c598ac714e"
    )
    files = sorted(set_dir.glob("*.jpg"))
    if not files:
        raise SystemExit(f"no images in {set_dir}")
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
    return images_b64


def run_model(model: str, images_b64: list[str], prompt: str, api_key: str) -> dict:
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": prompt, "images": images_b64}],
        "stream": True,
        "options": {"temperature": 0},
    }
    t0 = time.monotonic()
    text_parts: list[str] = []
    thinking_parts: list[str] = []
    eval_count = prompt_eval_count = None
    n_chunks = 0

    with requests.post(
        OLLAMA_URL,
        json=payload,
        headers={"Authorization": f"Bearer {api_key}"},
        timeout=(30, 300),
        stream=True,
    ) as r:
        r.raise_for_status()
        for line in r.iter_lines():
            if not line:
                continue
            chunk = json.loads(line)
            msg = chunk.get("message", {})
            text_parts.append(msg.get("content", ""))
            thinking_parts.append(msg.get("thinking", "") or "")
            n_chunks += 1
            if n_chunks % 200 == 0:
                print(
                    f"  ... {n_chunks} chunks, thinking={len(''.join(thinking_parts))} chars, "
                    f"answer={len(''.join(text_parts))} chars, {time.monotonic() - t0:.0f}s",
                    flush=True,
                )
            if chunk.get("done"):
                eval_count = chunk.get("eval_count")
                prompt_eval_count = chunk.get("prompt_eval_count")

    return {
        "model": model,
        "n_images": len(images_b64),
        "gen_s": round(time.monotonic() - t0, 1),
        "eval_count": eval_count,
        "prompt_eval_count": prompt_eval_count,
        "thinking": "".join(thinking_parts),
        "text": "".join(text_parts),
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="both", help="model tag, or 'both'")
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--max-dim", type=int, default=512)
    args = ap.parse_args()

    api_key = load_api_key()
    images_b64 = load_images(args.smoke, args.max_dim)

    if args.smoke:
        prompt = (
            "List every distinct piece of furniture you can see across these "
            "images, one per line. State how many separate images you were given."
        )
    else:
        prompt = PROMPT_HEADER.format(catalogue=build_catalogue())

    models = ["kimi-k2.6", "minimax-m3"] if args.model == "both" else [args.model]
    for m in models:
        print(f"\n{'=' * 70}\nRunning {m} on {len(images_b64)} images\n{'=' * 70}")
        try:
            res = run_model(m, images_b64, prompt, api_key)
        except Exception as exc:  # one model failing must not abort the others
            print(f"[{m}] FAILED: {exc}")
            continue
        print(
            f"[{m}] gen={res['gen_s']}s "
            f"prompt_tokens={res['prompt_eval_count']} out_tokens={res['eval_count']}"
        )
        print(res["text"])
        out = HERE / f"eval_cloud_{m.replace(':', '_').replace('/', '_')}.txt"
        out.write_text(res["text"])
        Path(f"{out}.meta.json").write_text(json.dumps(res, indent=2))
        print(f"saved -> {out}")


if __name__ == "__main__":
    main()
