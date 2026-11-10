"""A new look for Unity on the desktop: its base still redrawn in what it was told to wear (the image engine, img2img),
then its moves rendered from it (Agent Gen's video engine, image to video) and cut out (companion-clips.py), saved over
<out>/<move>.webp. The desktop app runs this on this computer (it has the GPU and the models) and swaps its clips when
it is done; while it runs it keeps the look it has.

    python companion-look.py --look "work clothes: white blouse, black pencil skirt" --base base.png --out <folder>
        [--image http://127.0.0.1:5124] [--video http://127.0.0.1:5127] [--agentgen "D:\\Agent Gen"]
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
ap.add_argument("--look", required=True)
ap.add_argument("--base", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--image", default="http://127.0.0.1:5124")
ap.add_argument("--video", default="http://127.0.0.1:5127")
ap.add_argument("--agentgen", default=os.environ.get("AGENTGEN_DIR") or r"D:\Agent Gen")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
os.makedirs(a.out, exist_ok=True)
LOOK = " ".join(a.look.split())[:200]


def say(msg):
    print(msg, flush=True)
    with open(os.path.join(a.out, "status.json"), "w", encoding="utf-8") as f:
        json.dump({"look": LOOK, "step": msg, "at": time.time()}, f)


def post(url, body, timeout):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read(), r.headers.get("Content-Type", "")


def up(url):
    try:
        with urllib.request.urlopen(url + "/health", timeout=3) as r:
            return r.status == 200
    except Exception:
        return False


def ensure_video():
    if up(a.video):
        return
    server = os.path.join(a.agentgen, "resources", "sidecars", "video", "server.py")
    models = os.path.join(a.agentgen, "data", "models")
    if not os.path.exists(server):
        raise SystemExit("Agent Gen's video engine is not installed (%s)" % server)
    flags = 0x08000000 if os.name == "nt" else 0   # no window
    subprocess.Popen([sys.executable, "-u", server, "--model-dir", os.path.join(models, "ltx-video"), "--wan-dir", os.path.join(models, "wan2.2-ti2v-5b"),
                      "--out", os.path.join(a.out, "_render")], creationflags=flags, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(90):
        time.sleep(2)
        if up(a.video):
            return
    raise SystemExit("the video engine did not start")


say("drawing the new look")
base = base64.b64encode(open(a.base, "rb").read()).decode()
# whatever it is told to be: the words are the picture (a goth girl in a blouse, a gorilla in a suit, anything)
png, ty = post(a.image + "/txt2img", {
    "width": 512, "height": 1024, "seed": int(time.time()) % 100000,
    "prompt": "full body photo head to toe of %s, standing facing the camera, whole body visible from head to feet, feet visible, "
              "plain light grey studio background, even soft light, photorealistic" % LOOK,
    "negative": "cropped, cut off feet, cut off head, close-up, portrait crop, several people",
}, 300)
if "image" not in ty:
    raise SystemExit("the image engine said: %s" % png[:200])
look_png = os.path.join(a.out, "_look.png")
open(look_png, "wb").write(png)
try:
    urllib.request.urlopen(urllib.request.Request(a.image + "/unload", data=b"{}", method="POST"), timeout=20).read()   # the GPU for the video engine
except Exception:
    pass

ensure_video()
img = base64.b64encode(png).decode()
B = "realistic video of the same character, %s, full body head to toe, plain light grey studio background, " % LOOK[:120]
MOVES = {
    "idle": (B + "it stands relaxed in place, shifts its weight from foot to foot, breathes, tilts its head and smiles, small natural movements, static camera", 7, 0, False),
    "walk": (B + "side view profile of its walking to the right with a natural walking cycle, it faces right the whole time, legs stepping and arms swinging, "
             "the camera tracks sideways with its so it stays in the center of the frame, smooth tracking shot", 3, 16, False),
    "sit": (B + "it sits down on the floor and stays sitting with its legs crossed, smiling at the camera, static camera", 7, 0, True),
    "wave": (B + "it smiles and waves at the camera with one hand, then lowers it, static camera", 7, 0, False),
    "dance": (B + "it dances playfully in place, swaying its hips and moving its arms, static camera", 7, 0, False),
}
done = 0
for name, (prompt, seed, start, once) in MOVES.items():
    say("filming the %s (%d of %d)" % (name, done + 1, len(MOVES)))
    body, _ = post(a.video + "/i2v", {"image_b64": img, "prompt": prompt, "engine": "wan", "width": 480, "height": 960, "seed": seed}, 900)
    j = json.loads(body)
    if not j.get("path"):
        raise SystemExit("the video engine said: %s" % j)
    args = [sys.executable, os.path.join(HERE, "companion-clips.py"), j["path"], name + ".next", "--out", a.out, "--from", str(start)]
    if once:
        args.append("--once")
    subprocess.run(args, check=True, creationflags=0x08000000 if os.name == "nt" else 0)
    done += 1
for name in MOVES:   # all at once, so it never mixes two looks
    os.replace(os.path.join(a.out, name + ".next.webp"), os.path.join(a.out, name + ".webp"))
try:
    urllib.request.urlopen(urllib.request.Request(a.video + "/unload", data=b"{}", method="POST"), timeout=20).read()
except Exception:
    pass
say("done")
