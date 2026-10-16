#!/usr/bin/env python3
"""
Mills speech engine: speech to text on the host (faster-whisper). Started and supervised by the Mills
hub; listens on 127.0.0.1 only. People's browsers record their mic and the signed-in hub forwards
the audio here; nothing leaves the host.

    python bridge/speech-engine.py --port 5125 --model <whisper model folder or name> [--device cpu|cuda|auto]

API:
  GET  /health      -> {"ok": true, "loaded": bool, "device"}
  POST /transcribe  body = the recording (webm / ogg / wav / mp3 bytes) -> {"text", "language", "seconds"}
The model loads on the first request.
"""
import argparse
import json
import os
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODEL = "small"
DEVICE = "cpu"
_lock = threading.Lock()
_state = {"model": None, "device": None}


def _load():
    if _state["model"] is not None:
        return _state["model"]
    from faster_whisper import WhisperModel
    dev = DEVICE
    if dev == "auto":
        try:
            import torch
            dev = "cuda" if torch.cuda.is_available() else "cpu"
        except Exception:
            dev = "cpu"
    _state["model"] = WhisperModel(MODEL, device=dev, compute_type="float16" if dev == "cuda" else "int8")
    _state["device"] = dev
    print(time.strftime("%H:%M:%S"), "speech: loaded", MODEL, "on", dev, flush=True)
    return _state["model"]


class Handler(BaseHTTPRequestHandler):
    server_version = "Mills-speech/1.0"

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
            return self._json({"ok": True, "loaded": _state["model"] is not None, "device": _state["device"] or DEVICE})
        self._json({"error": "unknown endpoint"}, 404)

    def do_POST(self):
        if self.path.split("?")[0] != "/transcribe":
            return self._json({"error": "unknown endpoint"}, 404)
        n = int(self.headers.get("Content-Length") or 0)
        if n <= 0 or n > 25 * 1024 * 1024:
            return self._json({"error": "send 1 byte to 25 MB of audio"}, 422)
        audio = self.rfile.read(n)
        fd, tmp = tempfile.mkstemp(suffix=".audio")
        try:
            with os.fdopen(fd, "wb") as f:
                f.write(audio)
            t0 = time.time()
            with _lock:
                segments, info = _load().transcribe(tmp, beam_size=5, vad_filter=True)
                text = " ".join(s.text.strip() for s in segments).strip()
            self._json({"text": text, "language": info.language, "seconds": round(time.time() - t0, 2)})
        except Exception as e:
            self._json({"error": "%s: %s" % (type(e).__name__, str(e)[:300])}, 500)
        finally:
            try:
                os.remove(tmp)
            except OSError:
                pass


def main():
    global MODEL, DEVICE
    ap = argparse.ArgumentParser(description="Mills speech engine")
    ap.add_argument("--port", type=int, default=5125)
    ap.add_argument("--model", default="small")
    # cpu by default: whisper-small is quick there and the GPU stays free for the brain and images
    # (its GPU runtime needs CUDA 12 libraries, which a CUDA 11 torch install does not ship)
    ap.add_argument("--device", default="cpu", choices=["auto", "cuda", "cpu"])
    a = ap.parse_args()
    MODEL, DEVICE = a.model, a.device
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    print(time.strftime("%H:%M:%S"), "speech: serving on 127.0.0.1:%d model=%s" % (a.port, os.path.basename(MODEL)), flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
