"""Unity's desktop clips: a rendered clip (Agent Gen's video engine, .avi / .mp4) -> a see-through looping animated WebP
for the desktop companion (ui/companion/<name>.webp).

Every frame is parsed by the SegFormer clothes model (anything that is not background is her), the edge softened, the
frames cropped to the box she moves in, scaled to a fixed height and saved as one animated WebP with alpha.

    python companion-clips.py <clip> <name> [--seg <segformer-clothes folder>] [--height 420] [--out ui/companion]
"""
import argparse
import os

import numpy as np
import torch
from PIL import Image, ImageFilter

ap = argparse.ArgumentParser()
ap.add_argument("clip")
ap.add_argument("name")
ap.add_argument("--seg", default=os.environ.get("MILLS_SEG_DIR") or os.path.join(os.path.expanduser("~"), "Desktop", "Unity 18+", "models", "segformer-clothes"))
ap.add_argument("--height", type=int, default=420)
ap.add_argument("--fps", type=float, default=0)
ap.add_argument("--from", dest="start", type=int, default=0, help="first frame to keep (drop a lead-in, e.g. her turning before she walks)")
ap.add_argument("--once", action="store_true", help="play once and hold the last frame (sit), instead of looping")
ap.add_argument("--out", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "ui", "companion"))
a = ap.parse_args()

import cv2  # noqa: E402
from transformers import AutoModelForSemanticSegmentation, SegformerImageProcessor  # noqa: E402

proc = SegformerImageProcessor.from_pretrained(a.seg)
model = AutoModelForSemanticSegmentation.from_pretrained(a.seg).eval()
dev = "cuda" if torch.cuda.is_available() else "cpu"
model.to(dev)

cap = cv2.VideoCapture(a.clip)
fps = a.fps or cap.get(cv2.CAP_PROP_FPS) or 24
frames = []
while True:
    ok, f = cap.read()
    if not ok:
        break
    frames.append(Image.fromarray(cv2.cvtColor(f, cv2.COLOR_BGR2RGB)))
frames = frames[a.start:]
if not frames:
    raise SystemExit("no frames in " + a.clip)

cut, box = [], None
with torch.no_grad():
    for im in frames:
        lo = model(**{k: v.to(dev) for k, v in proc(images=im, return_tensors="pt").items()}).logits
        seg = torch.nn.functional.interpolate(lo, size=im.size[::-1], mode="bilinear").argmax(1)[0].cpu().numpy()
        m = Image.fromarray(((seg != 0) * 255).astype(np.uint8)).filter(ImageFilter.MinFilter(3)).filter(ImageFilter.GaussianBlur(1.2))
        rgba = im.convert("RGBA")
        rgba.putalpha(m)
        b = m.point(lambda v: 255 if v > 40 else 0).getbbox()
        if b:
            box = b if box is None else (min(box[0], b[0]), min(box[1], b[1]), max(box[2], b[2]), max(box[3], b[3]))
        cut.append(rgba)
box = box or (0, 0) + frames[0].size
pad = 6
box = (max(0, box[0] - pad), max(0, box[1] - pad), min(frames[0].width, box[2] + pad), min(frames[0].height, box[3] + pad))
k = a.height / (box[3] - box[1])
size = (max(1, int((box[2] - box[0]) * k)), a.height)
out = [c.crop(box).resize(size, Image.LANCZOS) for c in cut]
os.makedirs(a.out, exist_ok=True)
dst = os.path.join(a.out, a.name + ".webp")
out[0].save(dst, save_all=True, append_images=out[1:], duration=int(1000 / fps), loop=1 if a.once else 0, lossless=False, quality=82, method=4)
print("%s: %d frames, %dx%d, %.0f KB" % (dst, len(out), size[0], size[1], os.path.getsize(dst) / 1024))
