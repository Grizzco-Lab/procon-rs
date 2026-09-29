"""Does a picture look like Splatoon? SigLIP 2 zero-shot on the CPU, as a
helper process for ``cover.mjs``: a JSON line per request on stdin
(``{"id": ..., "image": "<base64>"}``), a JSON line per answer on stdout
(``{"id": ..., "p": 0.35, "best": "<the closest prompt>"}``, or ``{"id":
..., "error": "..."}``), ``{"ready": true, ...}`` once the model is loaded.
``p`` is the model's own probability that the picture matches a prompt
(SigLIP is trained with a sigmoid per pair: its scale and bias on the
cosine similarity), the highest over the Splatoon prompts. It is small in
absolute terms: on the captured notes' covers, game screenshots and art
score 0.07 to 0.9 (median 0.35), everyday photos, chats and text cards
0.00 to 0.03, with one photo at 0.33; a softmax over Splatoon and
everyday prompts was tried first and rated a plush, leaves and a
handwritten note above 0.85, so the relative reading is not used.

Run in AgentZero's environment, which has torch and transformers and the
model cached (``uv run python cover.py`` in that folder). The GPU is never
used: ``CUDA_VISIBLE_DEVICES`` is emptied unless set already.
"""

import base64
import io
import json
import os
import sys

os.environ.setdefault("CUDA_VISIBLE_DEVICES", "")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

MODEL = "google/siglip2-base-patch16-256"
"""SigLIP 2 base (Apache-2.0), the model AgentZero's vtext uses: 86M
parameters, 256 x 256 input, about 0.35 s an image on four CPU threads."""

SPLATOON = [
    "a screenshot from the video game Splatoon 3",
    "Salmon Run in Splatoon 3: Salmonid fish enemies, golden eggs and ink"
    " on a Nintendo Switch screen",
    "Splatoon gameplay with inklings and octolings shooting colorful ink",
    "the results screen or statistics of a Splatoon 3 match",
    "fan art of Splatoon inklings, octolings or Salmonids",
    "a photo of a TV or a Nintendo Switch showing Splatoon",
]
"""A merchandise prompt ("amiibo figures or plush toys") was dropped: it
matched any plush."""


def main():
    import torch
    from PIL import Image
    from transformers import AutoModel, AutoProcessor

    torch.set_num_threads(int(os.environ.get("COVER_THREADS", "4")))
    model = AutoModel.from_pretrained(MODEL, dtype=torch.float32).eval()
    processor = AutoProcessor.from_pretrained(MODEL)
    with torch.no_grad():
        tokens = processor(
            text=SPLATOON,
            padding="max_length",
            max_length=64,
            truncation=True,
            return_tensors="pt",
        )
        text = model.get_text_features(input_ids=tokens["input_ids"]).pooler_output
        text = text / text.norm(dim=-1, keepdim=True)
    scale = model.logit_scale.exp().item()
    bias = model.logit_bias.item()
    print(json.dumps({"ready": True, "model": MODEL, "device": "cpu"}), flush=True)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        request_id = None
        try:
            request = json.loads(line)
            request_id = request.get("id")
            image = Image.open(io.BytesIO(base64.b64decode(request["image"])))
            image = image.convert("RGB")
            with torch.no_grad():
                pixels = processor(images=[image], return_tensors="pt")["pixel_values"]
                feat = model.get_image_features(pixel_values=pixels).pooler_output
                feat = feat / feat.norm(dim=-1, keepdim=True)
                probs = torch.sigmoid(feat @ text.T * scale + bias)[0]
            answer = {
                "id": request_id,
                "p": round(probs.max().item(), 4),
                "best": SPLATOON[int(probs.argmax())],
            }
        except Exception as error:  # noqa: BLE001 - reported to the caller
            answer = {"id": request_id, "error": f"{type(error).__name__}: {error}"}
        print(json.dumps(answer), flush=True)


if __name__ == "__main__":
    main()
