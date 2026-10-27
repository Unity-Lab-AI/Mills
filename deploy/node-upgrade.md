# Moving the team's Node.js (T-304)

Why: the 2026-10-21 Node security release (CVE-2026-105482 DNS NUL truncation, CVE-2026-105483 / -106069 / -106072 TLS
host-name checks, CVE-2026-111497 Brotli out-of-bounds write in `node:zlib`, all fixed in 22.23.4, 24.22.1, 26.12.1; the
server ran 22.23.3). Details: https://nodejs.org/en/blog/vulnerability/october-2026-security-releases

## The line

| Line | Release | Status (nodejs.org schedule) | End of life |
|---|---|---|---|
| 22 "Jod" | v22.23.4 | Maintenance LTS | 2027-04-30 |
| **24 "Krypton"** | **v24.22.1** | Maintenance LTS since 2026-10-20 (newest LTS today) | 2028-04-30 |
| 26 | v26.12.1 | Current; **becomes LTS on 2026-10-28**, Maintenance 2027-10-20 | 2029-04-30 |

Suite results (2026-10-25, throw-away hubs): Node 24.22.1 normal 196/0, --rust-hub 305/0, js-fallback OK; Node 26.12.1
normal 196/0; Node 22.23.3 (old) normal 196/0. Node 22 keeps working: nothing in the code needs a newer Node than 22 (no engines check, the
launchers only test that `node` exists), so AI hosts on 22 stay supported.

Downloaded and verified (linux-x64, SHASUMS256.txt from nodejs.org, and its GPG signature against the nodejs/release-keys
keyring: good signature by the release key 5BE8A3F6C8A5C01D 106C0AD820B1A390 B168D356):

| File | sha256 (in groups of 16: join them, compare with SHASUMS256.txt) |
|---|---|
| node-v24.22.1-linux-x64.tar.xz | 6c172a9ab6718f7b 801fb5a7bd486af2 ee5e32758086a04b 84fcce2aa77095e6 |
| node-v24.22.1-win-x64.zip | d2046d28c2c3dcd2 11ac5145c0bf30e2 047ef9dfb306693c 8c6e2eccd8b95fa5 |
| node-v26.12.1-linux-x64.tar.xz | 6170b2d474e1c626 f43cff3adc118df9 fe3b573258a4fc3d 850c8dbc6b079bbd |
| node-v26.12.1-win-x64.zip | b3efd5c70cba3308 ed46e7e699a9b8aa c075bbc6b027fcb3 9c84c31acc25e2b4 |

Unpacked under `/srv/apps/VibeRoller/node/`: `node-v24.22.1-linux-x64`, `node-v26.12.1-linux-x64`, next to
`node-v22.23.3-linux-x64`. **Done 2026-10-27 (Sponge's choice: 24 now, 26 later):** `current` points at
`node-v24.22.1-linux-x64`. The JavaScript fallback hub boots and serves on it (`js-fallback.js`). 22 stays as the rollback.

## Where Node is chosen

- `/srv/apps/VibeRoller/node/current` (a symlink) is the server's Node. The hub service (`deploy/viberoller-hub.service`)
  puts `current/bin` first on `PATH` and sets `VR_NODE=current/bin/node`; the auto-deploy service and `native/build.sh`
  use it too.
- The hub itself is Rust (`vr-run` starts `vr-hub`): it does not run on Node. Node runs only in processes started later:
  each Work run's MCP helpers (`pc-tools-mcp.js`, `board-mcp.js`, `team-mcp.js`) and the JavaScript hub when it is the
  last fallback. Each of them resolves the `current` link when it starts.
- `deploy/setup-server.sh`: for a NEW server only; takes a system Node from 22 up, else downloads the newest of the line
  `VR_NODE_LINE` (default 24) and verifies the tarball against the published SHASUMS256.txt.
- AI hosts and the PC launchers (`start*.sh`, `start*.bat`, the service installers) use whatever `node` is on the PATH.

## Switching the server's Node

1. Unpack and verify the new release next to the old one (as above).
2. Flip the link: `ln -sfn node-v24.22.1-linux-x64 /srv/apps/VibeRoller/node/current` (a relative target, as before).
   No restart is needed: every Node process started from then on (the next Work run, the fallback hub) uses it, and the
   Rust hub is not touched. A Work run already going keeps its old helpers until it ends.
3. Check: `/srv/apps/VibeRoller/node/current/bin/node --version`, then
   `/srv/apps/VibeRoller/node/current/bin/node bridge/test/js-fallback.js` (the fallback hub boots and serves on it), then
   start a Work run.
4. Rollback: `ln -sfn node-v22.23.3-linux-x64 /srv/apps/VibeRoller/node/current`. Delete the old folder only after a week
   of calm.
5. (Before vr-run, `run.js` re-spawned the hub with its own Node, so a full `systemctl --user restart viberoller-hub` was
   needed. With vr-run it is not.)

## AI hosts (PCs)

They run the node the owner installed and keep working on 22 or newer; nothing forces a change. To move a host: install
the same line from nodejs.org (the installer on Windows replaces the old one), then restart the service (the heart /
knife, or `install-ai-host-service`'s service restart). A host's own restart on a code change does not change its Node.

## Windows

Windows hosts and the Desktop launchers use the Node found on the PATH (`where node`). Install **node-v24.22.1-x64.msi**
from nodejs.org (or unzip `node-v24.22.1-win-x64.zip`, checksum above, and put its folder first on the PATH), then
restart the VibeRollerAIHost service (`sc stop VibeRollerAIHost` / `sc start VibeRollerAIHost`, or the knife and the
heart). The service installer stores the path of the Node it was installed with: after a move of the Node folder, run
`install-ai-host-service.cmd` again.
