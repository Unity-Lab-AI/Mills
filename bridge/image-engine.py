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
import sys
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
        if path == "/shutdown":
            self._json({"ok": True})
            threading.Timer(0.3, lambda: os._exit(0)).start()
            return
        fn = {"/txt2img": render_txt2img, "/img2img": render_img2img, "/inpaint": render_inpaint}.get(path)
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
