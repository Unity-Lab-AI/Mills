"""One new move for Unity on the desktop, from the words she was given ("do a backflip", "blow a kiss"): filmed from
her current look (Agent Gen's video engine, image to video), cut out (companion-clips.py) and saved as
<out>/<name>.webp, played once by the desktop window. Uses her current look's still (_look.png) or the built-in one.

    python companion-do.py --do "blow a kiss to the camera" --name do-1a2b --out <folder> [--video ...] [--agentgen ...]
"""
import argparse
import base64
import json
import os
import subprocess
import sys
import time
import urllib.request

ap = argparse.ArgumentParser()
ap.add_argument("--do", dest="what", required=True)
ap.add_argument("--name", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--video", default="http://127.0.0.1:5127")
ap.add_argument("--image", default="http://127.0.0.1:5124")
ap.add_argument("--agentgen", default=os.environ.get("AGENTGEN_DIR") or r"D:\Agent Gen")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
WHAT = " ".join(a.what.split())[:300]


def up(url):
    try:
        with urllib.request.urlopen(url + "/health", timeout=3) as r:
            return r.status == 200
    except Exception:
        return False


def i2v_retry(body, tries=8):
    """Film a clip; when her brain took the card meanwhile (the AI host stops the video engine for her answer), wait
    until she is done and film it again."""
    last = None
    for n in range(tries):
        try:
            ensure_video()
            req = urllib.request.Request(a.video + "/i2v", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=900) as r:
                j = json.loads(r.read())
            if j.get("path"):
                return j
            last = j
        except Exception as e:
            last = e
        time.sleep(25)
    raise SystemExit("the video engine kept stopping (%s)" % (last,))


def ensure_video():
    if up(a.video):
        return
    server = os.path.join(a.agentgen, "resources", "sidecars", "video", "server.py")
    models = os.path.join(a.agentgen, "data", "models")
    if not os.path.exists(server):
        raise SystemExit("Agent Gen's video engine is not installed (%s)" % server)
    subprocess.Popen([sys.executable, "-u", server, "--model-dir", os.path.join(models, "ltx-video"), "--wan-dir", os.path.join(models, "wan2.2-ti2v-5b"),
                      "--out", os.path.join(a.out, "_render")], creationflags=0x08000000 if os.name == "nt" else 0, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(90):
        time.sleep(2)
        if up(a.video):
            return
    raise SystemExit("the video engine did not start")


still = os.path.join(a.out, "_look.png")
if not os.path.exists(still):
    still = os.path.join(a.out, "base.png")
look = ""
try:
    look = json.load(open(os.path.join(a.out, "status.json"), encoding="utf-8")).get("look", "")
except Exception:
    pass
try:   # the GPU for the video engine
    urllib.request.urlopen(urllib.request.Request(a.image + "/unload", data=b"{}", method="POST"), timeout=20).read()
except Exception:
    pass
prompt = "realistic video of the same character%s, full body head to toe, plain light grey studio background, %s, static camera" % ((", " + look[:120]) if look else "", WHAT)
j = i2v_retry({"image_b64": base64.b64encode(open(still, "rb").read()).decode(), "prompt": prompt, "engine": "wan", "width": 480, "height": 960, "seed": int(time.time()) % 100000})
subprocess.run([sys.executable, os.path.join(HERE, "companion-clips.py"), j["path"], a.name, "--out", a.out, "--once"], check=True,
               creationflags=0x08000000 if os.name == "nt" else 0)
print("done", a.name)
