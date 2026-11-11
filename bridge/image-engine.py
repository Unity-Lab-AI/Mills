#!/usr/bin/env python3
"""
Mills image engine: local Stable Diffusion on the host (a realistic-vision SD1.5 checkpoint through
diffusers, DPM++ sampling, img2img, inpainting, and the IP-Adapter face lock so a person in a source
photo stays themselves). Started and supervised by the Mills hub; listens on 127.0.0.1 only and is
reached by people only through the signed-in hub. No hosted image API, no key.

    python bridge/image-engine.py --port 5124 --checkpoint <file> --device auto

API (every POST takes JSON, every image comes back as image/png with headers X-Seed, X-Seconds, X-Size):
  GET  /health   -> {"ok": true, "loaded": bool, "device", "checkpoint", "face_adapter"}
  POST /txt2img  {"prompt", "width"?, "height"?, "steps"?, "guidance"?, "seed"?, "face"?: base64, "face_scale"?}
  POST /img2img  {"prompt", "image": base64, "strength"? (0.2-0.95), ..., "face"?: base64 | true}
  POST /inpaint  {"prompt", "image": base64, "mask"?: base64 (white = repaint) | "box"?: [x0, y0, x1, y1] fractions, ...}
  POST /unload   -> frees the GPU (the process stays)     POST /shutdown -> exits
The prompt alone decides the picture: there is no negative prompt unless a request sends one.
The checkpoint loads on the first render, so /health answers at once.
"""
import argparse
import base64
import gc
import io
import json
import os
import random
import re
import shutil
import sys
import urllib.parse
import urllib.request
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DEFAULT_CKPT = ""
FACE_REPO, FACE_SUBFOLDER, FACE_WEIGHTS = "h94/IP-Adapter", "models", "ip-adapter-plus-face_sd15.safetensors"

NEG = ""  # no negative prompt: describe what you want, never what you do not

CKPT = DEFAULT_CKPT
DEVICE_ARG = "auto"
OFFLOAD = True

_lock = threading.Lock()
_state = {"t2i": None, "i2i": None, "inpaint": None, "device": None, "face": False, "loaded_at": 0.0}


# --------------------------------------------------------------------------- helpers
def _log(msg):
    print(time.strftime("%H:%M:%S"), "image:", msg, flush=True)


def _device():
    if _state["device"]:
        return _state["device"]
    import torch
    dev = DEVICE_ARG
    if dev == "auto":
        dev = "cuda" if torch.cuda.is_available() else "cpu"
    _state["device"] = dev
    return dev


def _round8(n, lo=256, hi=1024):
    n = int(n)
    n = max(lo, min(hi, n))
    return n - (n % 8)


def _fit(img, max_side):
    """Resize keeping aspect so the long side is max_side, both sides multiples of 8."""
    w, h = img.size
    scale = float(max_side) / float(max(w, h))
    nw, nh = _round8(w * scale), _round8(h * scale)
    return img.resize((nw, nh)) if (nw, nh) != (w, h) else img


def _b64_image(s):
    from PIL import Image
    raw = base64.b64decode(s.split(",", 1)[1] if isinstance(s, str) and s.startswith("data:") else s)
    return Image.open(io.BytesIO(raw)).convert("RGB")


def _png(img):
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


def _dummy_face():
    from PIL import Image
    return Image.new("RGB", (224, 224), (127, 127, 127))


# --------------------------------------------------------------------------- pipelines
def _load():
    if _state["t2i"] is not None:
        return
    import torch
    from diffusers import StableDiffusionPipeline, DPMSolverMultistepScheduler
    if not os.path.exists(CKPT):
        raise FileNotFoundError("checkpoint not found: %s" % CKPT)
    dev = _device()
    t0 = time.time()
    _log("loading %s on %s%s" % (os.path.basename(CKPT), dev, " (cpu offload)" if dev == "cuda" and OFFLOAD else ""))
    dtype = torch.float16 if dev == "cuda" else torch.float32
    t2i = StableDiffusionPipeline.from_single_file(CKPT, torch_dtype=dtype, safety_checker=None)
    t2i.scheduler = DPMSolverMultistepScheduler.from_config(t2i.scheduler.config)
    if dev == "cuda":
        # the pipeline lives in system RAM and only the layer being executed rides the GPU:
        # the brain and the voice keep their room on the card
        if OFFLOAD:
            t2i.enable_model_cpu_offload()
        else:
            t2i.to("cuda")
    else:
        t2i.to("cpu")
    _state["t2i"] = t2i
    _state["face"] = False
    _rebuild_siblings()
    _state["loaded_at"] = time.time()
    _log("loaded in %.1fs" % (time.time() - t0))


def _rebuild_siblings():
    from diffusers import StableDiffusionImg2ImgPipeline, StableDiffusionInpaintPipeline
    comps = _state["t2i"].components
    _state["i2i"] = StableDiffusionImg2ImgPipeline(**comps)
    _state["inpaint"] = StableDiffusionInpaintPipeline(**comps)


def _load_face():
    """IP-Adapter Plus Face: the person in the reference image stays themselves while the
    picture changes around them. Loaded on the first request that asks for it."""
    if _state["face"]:
        return
    _load()
    t2i = _state["t2i"]
    t0 = time.time()
    _log("loading the face adapter")
    t2i.load_ip_adapter(FACE_REPO, subfolder=FACE_SUBFOLDER, weight_name=FACE_WEIGHTS)
    if getattr(t2i, "feature_extractor", None) is None:
        from transformers import CLIPImageProcessor
        t2i.feature_extractor = CLIPImageProcessor()
    if _device() == "cuda" and OFFLOAD:
        t2i.enable_model_cpu_offload()   # again: the adapter's image encoder came after the first set-up
    _state["face"] = True
    _rebuild_siblings()                  # the siblings must share the adapter-bearing unet and encoder
    _log("face adapter ready in %.1fs" % (time.time() - t0))


def _face_kwargs(req, source_img=None):
    """ip_adapter kwargs for this request. Once the adapter is loaded every call must carry an
    image, so a request without a face sends a grey square at scale 0."""
    face = req.get("face")
    scale = float(req.get("face_scale") or 0.6)
    if face:
        _load_face()
        img = source_img if face is True and source_img is not None else _b64_image(face)
        for p in ("t2i", "i2i", "inpaint"):
            _state[p].set_ip_adapter_scale(scale)
        return {"ip_adapter_image": img.resize((224, 224))}
    if _state["face"]:
        for p in ("t2i", "i2i", "inpaint"):
            _state[p].set_ip_adapter_scale(0.0)
        return {"ip_adapter_image": _dummy_face()}
    return {}


def _common(req):
    import torch
    dev = _device()
    steps = int(req.get("steps") or (25 if dev == "cuda" else 12))
    steps = max(4, min(60, steps))
    seed = req.get("seed")
    seed = int(seed) if seed not in (None, "", 0) else random.randint(1, 2 ** 31 - 1)
    gen = torch.Generator(dev).manual_seed(seed)
    guidance = float(req.get("guidance") or 6.5)
    negative = str(req.get("negative") or NEG)
    return steps, seed, gen, guidance, negative


def _embeds(pipe, prompt, negative):
    """No prompt length cap: CLIP reads 77 tokens per pass, so a long prompt is cut into 75-token
    pieces on token boundaries, each piece is encoded, and the pieces are joined. The negative side
    is encoded to the same number of pieces (empty unless a request sends one)."""
    import torch
    tok = pipe.tokenizer
    ids = tok(prompt or "", add_special_tokens=False).input_ids
    chunks = [tok.decode(ids[i:i + 75]) for i in range(0, len(ids), 75)] or [""]
    neg_ids = tok(negative or "", add_special_tokens=False).input_ids
    neg_chunks = [tok.decode(neg_ids[i:i + 75]) for i in range(0, len(neg_ids), 75)]
    neg_chunks = (neg_chunks + [""] * len(chunks))[:max(len(chunks), len(neg_chunks))]
    chunks = (chunks + [""] * len(neg_chunks))[:len(neg_chunks)]
    dev = pipe._execution_device
    pos, neg = [], []
    for c, n in zip(chunks, neg_chunks):
        pe, ne = pipe.encode_prompt(c, dev, 1, True, negative_prompt=n)
        pos.append(pe); neg.append(ne)
    return {"prompt_embeds": torch.cat(pos, dim=1), "negative_prompt_embeds": torch.cat(neg, dim=1)}


def render_txt2img(req):
    import torch
    _load()
    dev = _device()
    big = dev == "cuda"
    w = _round8(req.get("width") or (768 if big else 512))
    h = _round8(req.get("height") or (768 if big else 512))
    steps, seed, gen, guidance, negative = _common(req)
    extra = _face_kwargs(req)
    t0 = time.time()
    if dev == "cuda":
        torch.cuda.empty_cache()
    out = _state["t2i"](**_embeds(_state["t2i"], str(req.get("prompt") or ""), negative),
                        width=w, height=h, num_inference_steps=steps, guidance_scale=guidance,
                        generator=gen, **extra).images[0]
    return out, seed, time.time() - t0


def render_img2img(req):
    import torch
    _load()
    dev = _device()
    src = _b64_image(req["image"])
    src = _fit(src, int(req.get("max_side") or (768 if dev == "cuda" else 512)))
    strength = float(req.get("strength") or 0.55)
    strength = max(0.15, min(0.95, strength))
    steps, seed, gen, guidance, negative = _common(req)
    extra = _face_kwargs(req, src)
    t0 = time.time()
    if dev == "cuda":
        torch.cuda.empty_cache()
    out = _state["i2i"](**_embeds(_state["i2i"], str(req.get("prompt") or ""), negative),
                        image=src, strength=strength, num_inference_steps=steps, guidance_scale=guidance,
                        generator=gen, **extra).images[0]
    return out, seed, time.time() - t0


def _mask_for(req, size):
    from PIL import Image, ImageDraw, ImageFilter
    w, h = size
    if req.get("mask"):
        m = _b64_image(req["mask"]).convert("L").resize((w, h))
        return m
    box = req.get("box")
    if not box or len(box) != 4:
        raise ValueError("inpaint needs a mask (base64, white = repaint) or a box [x0, y0, x1, y1] in fractions")
    x0, y0, x1, y1 = [max(0.0, min(1.0, float(v))) for v in box]
    m = Image.new("L", (w, h), 0)
    ImageDraw.Draw(m).rectangle((int(x0 * w), int(y0 * h), int(x1 * w), int(y1 * h)), fill=255)
    return m.filter(ImageFilter.GaussianBlur(6))


def render_inpaint(req):
    import torch
    _load()
    dev = _device()
    src = _b64_image(req["image"])
    src = _fit(src, int(req.get("max_side") or (768 if dev == "cuda" else 512)))
    mask = _mask_for(req, src.size)
    strength = float(req.get("strength") or 0.9)
    strength = max(0.2, min(1.0, strength))
    steps, seed, gen, guidance, negative = _common(req)
    extra = _face_kwargs(req, src)
    t0 = time.time()
    if dev == "cuda":
        torch.cuda.empty_cache()
    out = _state["inpaint"](**_embeds(_state["inpaint"], str(req.get("prompt") or ""), negative),
                            image=src, mask_image=mask, width=src.size[0], height=src.size[1], strength=strength,
                            num_inference_steps=steps, guidance_scale=guidance, generator=gen, **extra).images[0]
    return out, seed, time.time() - t0


# --------------------------------------------------------------------------- marker on skin
# The model cannot spell or put a mark where it is told, so the ink is drawn (a handwriting font or strokes),
# as a multiply over her skin with grain, then blended by the model at close range (see render_mark). Ink alone looked
# like stickers; a blend at picture scale respelled the words.
INK = {"black": (18, 16, 20), "red": (150, 18, 24), "pink": (205, 40, 120), "blue": (20, 40, 140),
       "purple": (90, 30, 120), "green": (20, 100, 40), "white": (235, 235, 235), "lipstick": (172, 22, 58)}
FONT = next((f for f in [os.path.join(os.environ.get("WINDIR", "C:\Windows"), "Fonts", "Inkfree.ttf"), "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"] if os.path.exists(f)), "")


def _stroke(th, item):
    """Letter line weight, varied per request. Lipstick tops out at 1/12 of the letter height: 1/9 made "MINE"
    hard to read."""
    if item.get("tool") == "lipstick":
        return max(1, int(th / random.uniform(12, 16)))
    if item.get("tool") == "paint":
        return max(1, int(th / random.uniform(11, 14)))
    return max(1, int(th / random.uniform(20, 30)))


def _skin_span(img, cy, band, cx=None):
    """(x0, x1) of the run of light pixels (skin or fabric, not her black hair or a dark background) in the rows
    around cy that holds cx, else the widest one; None when there is nothing usable."""
    if img is None:
        return None
    import numpy as np
    a = np.asarray(img).astype(np.int16)
    h = a.shape[0]
    rows = a[max(0, cy - band // 2):min(h, cy + band // 2 + 1)]
    if not len(rows):
        return None
    light = (rows.mean(axis=2) > 75).mean(axis=0) > 0.6
    runs, start = [], None
    for x, s in enumerate(list(light) + [False]):
        if s and start is None:
            start = x
        elif not s and start is not None:
            runs.append((start, x))
            start = None
    runs = [r for r in runs if r[1] - r[0] > band]
    if not runs:
        return None
    if cx is not None:
        hit = [r for r in runs if r[0] <= cx <= r[1]]
        if hit:
            return hit[0]
    return max(runs, key=lambda r: r[1] - r[0])


def _mark_layer(size, item, fh):
    from PIL import Image, ImageDraw, ImageFont, ImageFilter
    w, h = size
    cx, cy = item["x"] * w, item["y"] * h
    lay = Image.new("L", size, 0)
    shape = item.get("shape")
    angle = float(item.get("angle", 0))
    if item.get("text"):
        text = str(item["text"])[:24]
        th = max(14, int(fh * (0.42 if len(text) <= 5 else 0.3)))
        try:
            f = ImageFont.truetype(FONT, th)
        except Exception:
            f = ImageFont.load_default()
        tmp = Image.new("L", (int(th * (len(text) + 2) * 1.1), int(th * 2)), 0)
        ImageDraw.Draw(tmp).text((th // 2, 0), text, font=f, fill=255, stroke_width=_stroke(th, item), stroke_fill=255)
        tmp = tmp.crop(tmp.getbbox()).rotate(angle, expand=True, resample=Image.BICUBIC)
        skin = _skin_span(item.get("_img"), int(cy), tmp.height, cx)
        if skin:   # ink on her black hair vanishes: centre the words on the bare skin at that height, shrink to fit
            s0, s1 = skin
            if tmp.width > (s1 - s0) * 0.92:
                k = (s1 - s0) * 0.92 / tmp.width
                tmp = tmp.resize((max(1, int(tmp.width * k)), max(1, int(tmp.height * k))), Image.LANCZOS)
            if not (s0 <= cx - tmp.width / 2 and cx + tmp.width / 2 <= s1):
                cx = (s0 + s1) / 2
        lay.paste(tmp, (int(cx - tmp.width / 2), int(cy - tmp.height / 2)), tmp)
        return lay.filter(ImageFilter.GaussianBlur(0.8))
    d = ImageDraw.Draw(lay)
    tool = item.get("tool")
    lo, hi = {"lipstick": (0.05, 0.075), "paint": (0.06, 0.085)}.get(tool, (0.045, 0.065))
    sw = max(3, int(fh * random.uniform(lo, hi)))
    L = fh * 0.5
    if shape == "arrow":
        top, bot = (cx, cy - L), (cx - 2, cy + L)
        d.line([top, bot], fill=255, width=sw, joint="curve")
        for sgn in (-1, 1):
            d.line([bot, (bot[0] + sgn * L * 0.32, bot[1] - L * 0.45)], fill=255, width=sw)
    elif shape == "heart":
        import math
        pts = [(cx + L * 0.55 * 16 * math.sin(t) ** 3 / 16, cy - L * 0.55 * (13 * math.cos(t) - 5 * math.cos(2 * t) - 2 * math.cos(3 * t) - math.cos(4 * t)) / 16)
               for t in [i * 2 * math.pi / 60 for i in range(61)]]
        d.line(pts, fill=255, width=sw, joint="curve")
    elif shape == "x":
        r = L * 0.45
        d.line([(cx - r, cy - r), (cx + r, cy + r)], fill=255, width=sw)
        d.line([(cx + r, cy - r), (cx - r, cy + r)], fill=255, width=sw)
    elif shape == "star":
        import math
        pts = [(cx + L * 0.5 * math.sin(i * 4 * math.pi / 5), cy - L * 0.5 * math.cos(i * 4 * math.pi / 5)) for i in range(6)]
        d.line(pts, fill=255, width=sw, joint="curve")
    elif shape == "smiley":
        r = L * 0.5
        d.ellipse((cx - r, cy - r, cx + r, cy + r), outline=255, width=sw)
        d.arc((cx - r * 0.55, cy - r * 0.5, cx + r * 0.55, cy + r * 0.55), 20, 160, fill=255, width=sw)
        for sgn in (-1, 1):
            d.ellipse((cx + sgn * r * 0.35 - sw, cy - r * 0.3 - sw, cx + sgn * r * 0.35 + sw, cy - r * 0.3 + sw), fill=255)
    elif shape == "tally":
        for i in range(4):
            x = cx - L * 0.4 + i * L * 0.22
            d.line([(x, cy - L * 0.4), (x + 2, cy + L * 0.4)], fill=255, width=sw)
        d.line([(cx - L * 0.55, cy + L * 0.3), (cx + L * 0.45, cy - L * 0.3)], fill=255, width=sw)
    return lay.filter(ImageFilter.GaussianBlur(1.2))


def _ink(img, lay, color, opacity=0.88):
    """Ink as a multiply over her skin: the skin's light, shadow and sheen show through it like real marker or
    lipstick, and a little grain breaks the edges so it is not a flat decal."""
    import numpy as np
    from PIL import Image
    base = np.asarray(img).astype(np.float32) / 255.0
    a = np.asarray(lay).astype(np.float32)[..., None] / 255.0
    rng = np.random.default_rng(len(color) * 7919 + int(a.sum()) % 9973)
    grain = 0.82 + 0.18 * rng.random(a.shape[:2])[..., None]
    a = np.clip(a * grain * opacity, 0, 1)
    tint = np.array(color, dtype=np.float32) / 255.0
    lum = base.mean(axis=2, keepdims=True)
    inked = base * tint * 0.55 + tint * lum * 0.45   # multiply, with some of the skin's brightness carried through
    out = base * (1 - a) + inked * a
    return Image.fromarray((np.clip(out, 0, 1) * 255).astype(np.uint8))


# Where things are on her: a clothes-parsing model labels every pixel (hair, face, upper clothes, pants, skirt,
# dress, arms, legs). It files bare torso skin under "Face", so torso skin = "Face" below her chin. It runs on the
# CPU (~1 s) so it never takes VRAM from the image and video engines.
def _seg_dir():
    return os.environ.get("MILLS_SEG_DIR") or os.path.join(os.path.dirname(CKPT or "."), "segformer-clothes")
_seg = {"model": None, "proc": None}
SEG = {"bg": 0, "hat": 1, "hair": 2, "upper": 4, "skirt": 5, "pants": 6, "dress": 7, "face": 11,
       "lleg": 12, "rleg": 13, "larm": 14, "rarm": 15, "scarf": 17}
# body parts in face heights below the chin (band) and which labels they live on; "left" is HER left
PARTS = {
    "forehead": ((-1.0, -0.6), ["face"]), "cheek": ((-0.6, -0.1), ["face"]), "neck": ((0.0, 0.8), ["face"]),
    "chest": ((0.5, 2.0), ["face"]), "stomach": ((1.5, 3.2), ["face"]), "lower belly": ((2.4, 3.6), ["face"]),
    "left shoulder": ((0.2, 1.2), ["larm", "face"]), "right shoulder": ((0.2, 1.2), ["rarm", "face"]),
    "left arm": ((0.5, 4.0), ["larm"]), "right arm": ((0.5, 4.0), ["rarm"]),
    "left thigh": ((2.5, 8.0), ["lleg"]), "right thigh": ((2.5, 8.0), ["rleg"]),
    "top": ((0.0, 4.0), ["upper", "dress"]), "bottoms": ((1.5, 8.0), ["pants", "skirt"]), "dress": ((0.0, 8.0), ["dress"]),
}


def _parse(img):
    import numpy as np
    import torch
    if _seg["model"] is None:
        from transformers import SegformerImageProcessor, AutoModelForSemanticSegmentation
        _seg["proc"] = SegformerImageProcessor.from_pretrained(_seg_dir())
        _seg["model"] = AutoModelForSemanticSegmentation.from_pretrained(_seg_dir()).eval()
    with torch.no_grad():
        lo = _seg["model"](**_seg["proc"](images=img, return_tensors="pt")).logits
        return torch.nn.functional.interpolate(lo, size=img.size[::-1], mode="bilinear").argmax(1)[0].numpy().astype(np.uint8)


def _erode(m, n):
    from PIL import Image, ImageFilter
    import numpy as np
    im = Image.fromarray((m * 255).astype(np.uint8)).filter(ImageFilter.MinFilter(n * 2 + 1))
    return np.asarray(im) > 127


def _area(seg, part, chin, fh):
    """Boolean mask of where `part` is in the picture, or None if it is not in it."""
    import numpy as np
    band, labels = PARTS.get(part, PARTS["stomach"])
    m = np.isin(seg, [SEG[l] for l in labels])
    rows = np.arange(seg.shape[0])[:, None]
    if "face" in labels:   # torso skin: "Face" below the chin only, never the face itself
        m &= ~((seg == SEG["face"]) & (rows < chin + 0.15 * fh))
    m &= (rows >= chin + band[0] * fh) & (rows <= chin + band[1] * fh)
    m = _erode(m, max(2, int(fh * 0.04)))
    return m if m.sum() > (fh * 0.25) ** 2 else None


def _fit_box(mask, bw, bh, target):
    """Top-left (x, y) of a bw x bh box inside mask, nearest to target; None if none fits."""
    import numpy as np
    H, W = mask.shape
    if bw >= W or bh >= H:
        return None
    ii = np.pad(mask.astype(np.int32).cumsum(0).cumsum(1), ((1, 0), (1, 0)))
    full = ii[bh:, bw:] - ii[:-bh, bw:] - ii[bh:, :-bw] + ii[:-bh, :-bw]
    ok = full >= bw * bh * 0.97
    if not ok.any():
        return None
    ys, xs = np.nonzero(ok)
    d = (xs + bw / 2 - target[0]) ** 2 + ((ys + bh / 2 - target[1]) * 1.5) ** 2
    i = int(d.argmin())
    return int(xs[i]), int(ys[i])


def _word_img(text, th, item):
    from PIL import Image, ImageDraw, ImageFont
    try:
        f = ImageFont.truetype(FONT, th)
    except Exception:
        f = ImageFont.load_default()
    tmp = Image.new("L", (int(th * (len(text) + 2) * 1.1), int(th * 2)), 0)
    ImageDraw.Draw(tmp).text((th // 2, 0), text, font=f, fill=255, stroke_width=_stroke(th, item), stroke_fill=255)
    tmp = tmp.crop(tmp.getbbox())
    return tmp.rotate(float(item.get("angle", 0)), expand=True, resample=Image.BICUBIC)


def _place(src, seg, item, chin, fh, taken, prev=None):
    """The ink layer for one item, sized to fit the part it was asked on: (layer, on_cloth, size), or raises."""
    import numpy as np
    from PIL import Image, ImageFilter
    part = item.get("part") or "stomach"
    m = _area(seg, part, chin, fh)
    if m is None:
        raise ValueError("her %s is not in the picture" % part)
    gap = max(4, int(fh * 0.08))
    m &= _erode(~taken, gap)   # keep clear of what is already drawn, with a gap
    if not m.any():
        raise ValueError("no room left on her %s" % part)
    ys, xs = np.nonzero(m)
    target = (float(np.median(xs)), float(np.median(ys)))
    if prev is not None:
        # a second mark on the same part goes under the first ("hole" with a down arrow): same centre line, below it,
        # and only if nothing fits below does it go wherever there is room
        below = m.copy()
        below[:prev[3] + gap] = False
        if below.sum() > (fh * 0.2) ** 2:
            m = below
            target = ((prev[0] + prev[2]) / 2.0, prev[3] + gap)
    if "shoulder" in part or "arm" in part:
        # her side only: the arm's own pixels when they show, else the outer edge of her torso on that side
        arm = m & (seg == SEG["larm" if part.startswith("left") else "rarm"])
        if arm.sum() > (fh * 0.25) ** 2:
            m = arm
            ys, xs = np.nonzero(m)
            target = (float(np.median(xs)), float(ys.min() + (np.median(ys) - ys.min()) * 0.4))
        else:
            target = (float(np.percentile(xs, 90 if part.startswith("left") else 10)), float(np.median(ys)))
    cloth = part in ("top", "bottoms", "dress")
    big = fh * 0.45
    if item.get("text"):
        text = str(item["text"])[:24]
        th = int(big if len(text) <= 5 else big * 0.8)
        while th >= max(10, int(fh * 0.12)):
            w = _word_img(text, th, item)
            at = _fit_box(m, w.width, w.height, (target[0], target[1] + (w.height / 2 if prev is not None else 0)))
            if at:
                lay = Image.new("L", src.size, 0)
                lay.paste(w, at, w)
                return lay.filter(ImageFilter.GaussianBlur(0.8)), cloth, th
            th = int(th * 0.88)
        raise ValueError("no room for \"%s\" on her %s" % (text, part))
    side = int(fh * (1.1 if item.get("shape") == "arrow" else 0.8))
    while side >= int(fh * 0.25):
        at = _fit_box(m, side, side, (target[0], target[1] + (side / 2 if prev is not None else 0)))
        if at:
            it = {**item, "x": (at[0] + side / 2) / src.size[0], "y": (at[1] + side / 2) / src.size[1]}
            return _mark_layer(src.size, it, side), cloth, side
        side = int(side * 0.88)
    raise ValueError("no room for a %s on her %s" % (item.get("shape"), part))


def render_mark(req):
    """POST /mark {"image", "items": [{"text"|"shape", "part", "color"?, "tool"?, "angle"?}], "chin", "face_h"
    (fractions of the picture height)}. Each item lands on the part it was asked on (skin or the named piece of
    clothing), sized to fit there; a part that is not in the picture is an error, not a guess."""
    import numpy as np
    from PIL import Image, ImageFilter
    src = _fit(_b64_image(req["image"]), int(req.get("max_side") or 768)).convert("RGB")
    H = src.size[1]
    t0 = time.time()
    seg = _parse(src)
    fh, chin = _face_from_seg(seg, req)
    taken = np.zeros(seg.shape, bool)
    out, placed, last = src, [], {}
    for it in req.get("items") or []:
        part = it.get("part") or "stomach"
        lay, cloth, size = _place(src, seg, it, chin, fh, taken, last.get(part))
        last[part] = lay.getbbox()
        taken |= np.asarray(lay) > 20
        color = INK.get(str(it.get("color") or "black").lower(), INK["black"])
        opacity = {"lipstick": 0.8, "paint": 0.95}.get(it.get("tool"), 0.88) * (0.85 if cloth else 1.0)
        out = _ink(out, lay, color, opacity=opacity)
        placed.append((lay, it, cloth, size))
    if req.get("blend") is False:
        return out, 0, time.time() - t0
    # Blend shapes at close range: cut out, blown up, blended on and around the strokes, shrunk back and feathered in.
    # Words keep the ink as drawn: the model works on a grid 8x coarser than the picture and respells them even
    # blown up ("kiss me" -> "Blss Me", "hole" -> "holo").
    _load()
    final = out
    for lay, it, cloth, size in placed:
        if it.get("text"):
            continue
        b = lay.getbbox()
        pad = max(16, int(0.35 * max(b[2] - b[0], b[3] - b[1])))
        box = (max(0, b[0] - pad), max(0, b[1] - pad), min(src.size[0], b[2] + pad), min(H, b[3] + pad))
        cw, ch = box[2] - box[0], box[3] - box[1]
        k = 640.0 / max(cw, ch)
        W2, H2 = max(64, int(cw * k) // 8 * 8), max(64, int(ch * k) // 8 * 8)
        crop = final.crop(box).resize((W2, H2), Image.LANCZOS)
        m = lay.crop(box).resize((W2, H2), Image.LANCZOS).point(lambda v: 255 if v > 40 else 0)
        m = m.filter(ImageFilter.MaxFilter(int(max(3, k * 3)) // 2 * 2 + 1)).filter(ImageFilter.GaussianBlur(max(2, k * 1.5)))
        steps, seed, gen, guidance, negative = _common({**req, "guidance": req.get("guidance") or 6})
        what = "handwriting" if it.get("text") else "a drawing"
        where = "on fabric, cloth texture" if cloth else "on skin, skin texture"
        prompt = "close-up photo of %s in %s %s, natural light" % (what, it.get("tool") or "marker", where)
        done = _state["inpaint"](prompt=prompt, negative_prompt=negative, image=crop, mask_image=m,
                                 width=W2, height=H2, strength=float(req.get("blend_strength") or 0.3),
                                 num_inference_steps=steps, guidance_scale=guidance, generator=gen,
                                 **_face_kwargs({}, crop)).images[0]
        done = done.convert("RGB").resize((cw, ch), Image.LANCZOS)
        feather = m.resize((cw, ch), Image.LANCZOS).filter(ImageFilter.GaussianBlur(2))
        final = final.copy()
        final.paste(Image.composite(done, final.crop(box), feather), box[:2])
    return final, 0, time.time() - t0

def _face_from_seg(seg, req):
    """Face height and chin row in pixels: from the request when given (fractions of the height), else from the
    parse (no face detector here): her face's top rows give its width, a face is ~1.35 widths tall."""
    import numpy as np
    H = seg.shape[0]
    if req.get("face_h") and req.get("chin"):
        return float(req["face_h"]) * H, float(req["chin"]) * H
    rows = np.nonzero((seg == SEG["face"]).any(axis=1))[0]
    if not len(rows):
        return 0.18 * H, 0.3 * H
    y0 = int(rows[0])
    widths = [(seg[y] == SEG["face"]).sum() for y in range(y0, min(H, y0 + max(8, H // 4)))]
    fh = max(H * 0.04, 1.3 * float(max(widths)))   # a face is ~1.3 of its widest row tall
    return fh, y0 + fh


CAPTION_AT = {"top": 0.1, "center": 0.5, "middle": 0.5, "bottom": 0.88}


def render_caption(req):
    """POST /caption {"image", "text", "where"?: top|center|bottom, "color"?}: words on any picture, meme style
    (bold, outlined so they read on anything), sized to fit the width."""
    from PIL import ImageDraw, ImageFont
    t0 = time.time()
    img = _fit(_b64_image(req["image"]), int(req.get("max_side") or 1024)).convert("RGB")
    text = str(req.get("text") or "").strip()[:120]
    if not text:
        raise ValueError("no text to write")
    W, H = img.size
    fonts = [os.path.join(os.environ.get("WINDIR", "C:\Windows"), "Fonts", "impact.ttf"), "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", FONT]
    path = next((f for f in fonts if f and os.path.exists(f)), "")
    d = ImageDraw.Draw(img)
    size = int(H * 0.11)
    while size > 10:
        f = ImageFont.truetype(path, size) if path else ImageFont.load_default()
        b = d.textbbox((0, 0), text, font=f, stroke_width=max(1, size // 14))
        if b[2] - b[0] <= W * 0.92:
            break
        size = int(size * 0.9)
    tw, th = b[2] - b[0], b[3] - b[1]
    y = CAPTION_AT.get(str(req.get("where") or "bottom").lower(), 0.88) * H - th / 2
    fill = INK.get(str(req.get("color") or "").lower(), (255, 255, 255))
    d.text(((W - tw) / 2 - b[0], max(0, min(H - th, y)) - b[1]), text, font=f, fill=fill, stroke_width=max(1, size // 14), stroke_fill=(0, 0, 0))
    return img, 0, time.time() - t0


def unload():
    had = _state["t2i"] is not None
    for k in ("t2i", "i2i", "inpaint"):
        _state[k] = None
    _state["face"] = False
    gc.collect()
    try:
        import torch
        if torch.cuda.is_available():
            torch.cuda.empty_cache()
    except Exception:
        pass
    if had:
        _log("unloaded")
    return had


# --------------------------------------------------------------------------- Desktop Unity's looks for the website
# The desktop app films her looks itself; for her on the website the hub asks this engine: the same scripts
# (companion-look.py: draw her, film her moves, cut them out) run here, one look at a time (a newer one stops the
# one being filmed), into a folder per person; the hub then fetches the clips.
import subprocess
_COMP_SRC = os.environ.get("MILLS_COMPANION_SRC") or os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "native", "crates", "vr-desktop"))
_COMP_OUT = os.environ.get("MILLS_COMPANION_OUT") or os.path.join(os.path.expanduser("~"), ".viberoller", "companion-web")
_comp = {"proc": None, "user": ""}
_COMP_CLIPS = ("idle", "walk", "sit", "wave", "dance")


def _comp_user(u):
    u = re.sub(r"[^a-z0-9-]", "", str(u or "").lower())[:40]
    if not u:
        raise ValueError("no user")
    return u


def comp_look(req):
    user, look = _comp_user(req.get("user")), " ".join(str(req.get("look") or "").split())[:300]
    if not look:
        raise ValueError("what should she look like?")
    p = _comp["proc"]
    if p and p.poll() is None and _comp["user"] == user and _comp.get("look") == look:  # the same order again (another tab)
        return {"ok": True, "user": user, "look": look, "already": True}
    if p and p.poll() is None:  # an order always wins: the look being filmed stops
        try:
            if os.name == "nt":
                subprocess.run(["taskkill", "/F", "/T", "/PID", str(p.pid)], capture_output=True, creationflags=0x08000000)
            else:
                p.kill()
        except Exception:
            pass
        try:
            urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:5127/abort", data=b"{}", method="POST"), timeout=5).read()
        except Exception:
            pass
    out = os.path.join(_COMP_OUT, user)
    os.makedirs(out, exist_ok=True)
    base = os.path.join(out, "base.png")
    if not os.path.exists(base):
        shutil.copyfile(os.path.join(_COMP_SRC, "ui", "companion", "base.png"), base)
    for f in ("companion-look.py", "companion-clips.py"):
        shutil.copyfile(os.path.join(_COMP_SRC, f), os.path.join(out, f))
    log = open(os.path.join(out, "look.log"), "w")
    kw = {"creationflags": 0x08000000} if os.name == "nt" else {}
    _comp["proc"] = subprocess.Popen([sys.executable, os.path.join(out, "companion-look.py"), "--look", look, "--base", base, "--out", out],
                                     cwd=out, stdout=subprocess.DEVNULL, stderr=log, **kw)
    _comp["user"], _comp["look"] = user, look
    _log("companion look for %s: %s" % (user, look[:80]))
    return {"ok": True, "user": user, "look": look}


def comp_status(user):
    out = os.path.join(_COMP_OUT, user)
    st = {}
    try:
        st = json.load(open(os.path.join(out, "status.json")))
    except Exception:
        pass
    p = _comp["proc"]
    busy = bool(p and p.poll() is None and _comp["user"] == user)
    err = ""
    try:
        err = "\n".join(open(os.path.join(out, "look.log")).read().strip().splitlines()[-3:])
    except Exception:
        pass
    clips = [n for n in _COMP_CLIPS if os.path.exists(os.path.join(out, n + ".webp"))]
    stamp = max([os.path.getmtime(os.path.join(out, n + ".webp")) for n in clips] or [0])
    return {"ok": True, "busy": busy, "status": st, "clips": clips, "stamp": int(stamp), "log": "" if busy else err}


# --------------------------------------------------------------------------- HTTP
class Handler(BaseHTTPRequestHandler):
    server_version = "Mills-image/1.0"

    def log_message(self, fmt, *args):
        pass

    def _json(self, obj, status=200):
        body = json.dumps(obj).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path, _, qs = self.path.partition("?")
        if path in ("/companion/status", "/companion/clip"):
            q = {k: v[0] for k, v in urllib.parse.parse_qs(qs).items()}
            try:
                user = _comp_user(q.get("user"))
            except ValueError as e:
                return self._json({"error": str(e)}, 422)
            if path == "/companion/status":
                return self._json(comp_status(user))
            name = q.get("name", "")
            if name not in _COMP_CLIPS:
                return self._json({"error": "no such clip"}, 404)
            f = os.path.join(_COMP_OUT, user, name + ".webp")
            if not os.path.exists(f):
                return self._json({"error": "no clip yet"}, 404)
            body = open(f, "rb").read()
            self.send_response(200)
            self.send_header("Content-Type", "image/webp")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path.split("?")[0] == "/health":
            return self._json({"ok": True, "loaded": _state["t2i"] is not None, "device": _state["device"] or DEVICE_ARG,
                               "checkpoint": os.path.basename(CKPT), "checkpoint_present": os.path.exists(CKPT),
                               "face_adapter": _state["face"], "busy": _lock.locked()})
        self._json({"error": "unknown endpoint"}, 404)

    def do_POST(self):
        path = self.path.split("?")[0]
        try:
            n = int(self.headers.get("Content-Length") or 0)
            req = json.loads(self.rfile.read(n).decode("utf-8")) if n else {}
        except Exception as e:
            return self._json({"error": "bad json: %s" % e}, 400)
        if path == "/unload":
            with _lock:
                return self._json({"ok": True, "unloaded": unload()})
        if path == "/companion/look":
            try:
                return self._json(comp_look(req))
            except (ValueError, OSError) as e:
                return self._json({"error": str(e)}, 422)
        if path == "/shutdown":
            self._json({"ok": True})
            threading.Timer(0.3, lambda: os._exit(0)).start()
            return
        fn = {"/txt2img": render_txt2img, "/img2img": render_img2img, "/inpaint": render_inpaint, "/mark": render_mark, "/caption": render_caption}.get(path)
        if fn is None:
            return self._json({"error": "unknown endpoint"}, 404)
        if not str(req.get("prompt") or "").strip() and path == "/txt2img":
            return self._json({"error": "prompt is empty"}, 422)
        try:
            with _lock:
                img, seed, secs = fn(req)
        except FileNotFoundError as e:
            return self._json({"error": str(e)}, 503)
        except (KeyError, ValueError) as e:
            return self._json({"error": str(e)}, 422)
        except Exception as e:
            _log("render failed: %r" % (e,))
            return self._json({"error": "%s: %s" % (type(e).__name__, str(e)[:300])}, 500)
        body = _png(img)
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("X-Seed", str(seed))
        self.send_header("X-Seconds", "%.2f" % secs)
        self.send_header("X-Size", "%dx%d" % img.size)
        self.end_headers()
        self.wfile.write(body)
        _log("%s %dx%d seed %d in %.1fs" % (path[1:], img.size[0], img.size[1], seed, secs))


def main():
    global CKPT, DEVICE_ARG, OFFLOAD
    ap = argparse.ArgumentParser(description="Mills image engine")
    ap.add_argument("--port", type=int, default=int(os.environ.get("MILLS_IMAGE_PORT", "5124")))
    ap.add_argument("--checkpoint", default=os.environ.get("MILLS_IMAGE_CHECKPOINT", DEFAULT_CKPT))
    ap.add_argument("--device", default=os.environ.get("MILLS_IMAGE_DEVICE", "auto"), choices=["auto", "cuda", "cpu"])
    ap.add_argument("--no-offload", action="store_true", help="keep the whole model on the GPU (faster, ~3 GB more VRAM)")
    ap.add_argument("--preload", action="store_true", help="load the checkpoint at start instead of on the first render")
    a = ap.parse_args()
    CKPT, DEVICE_ARG, OFFLOAD = a.checkpoint, a.device, not a.no_offload
    if a.preload:
        _load()
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    _log("serving on 127.0.0.1:%d  checkpoint=%s  present=%s" % (a.port, os.path.basename(CKPT), os.path.exists(CKPT)))
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
