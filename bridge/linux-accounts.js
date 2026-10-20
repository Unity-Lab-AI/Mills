// VibeRoller accounts ↔ the server's Linux accounts (OVH: sponge, gee, mills; everyone also in the sharedwork group).
//
// Which Linux account belongs to which VibeRoller account: config "linuxUsers": { "<vibeRoller user>": "<linux user>" },
// else the Linux account with the same name, if the server has one. Nothing here needs root, and the hub (which runs as
// one Linux account) gets no access to anyone's home by itself:
//   each person links their own account once, as themselves (deploy/link-linux-account.sh, over SSH or by their AI
//   agent there): it makes ~/uploaded_files and grants the hub's account read / write on THAT folder only (an ACL),
//   plus passing through the home folder to reach it. Their uploads then land there, theirs to work on, edit, run.
// Until then (or where the server has no such account) uploads stay in the hub's own area for that person, as before.
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');

module.exports = function createLinuxAccounts({ config = {}, ROOT }) {
  const HUB_USER = (() => { try { return os.userInfo().username; } catch { return ''; } })();
  let passwd = null, readAt = 0;
  function users() { // name → { uid, home }, re-read every minute (accounts change rarely)
    if (passwd && Date.now() - readAt < 60000) return passwd;
    passwd = {}; readAt = Date.now();
    // (config linuxPasswdFile: the regression runner's stand-in for /etc/passwd)
    try { for (const l of fs.readFileSync(config.linuxPasswdFile || '/etc/passwd', 'utf8').split('\n')) { const f = l.split(':'); if (f.length >= 7 && Number(f[2]) >= 1000 && !/nologin|false$/.test(f[6])) passwd[f[0]] = { uid: Number(f[2]), home: f[5] }; } } catch {}
    return passwd;
  }
  const linuxUserOf = (username) => { const map = config.linuxUsers || {}; const lu = map[username] !== undefined ? map[username] : username; return lu && users()[lu] ? lu : ''; };
  const canWrite = (d) => { try { fs.accessSync(d, fs.constants.W_OK | fs.constants.X_OK); return fs.statSync(d).isDirectory(); } catch { return false; } };
  // where this person's uploads go: their linked ~/uploaded_files, else the hub's own area for them
  function uploadsDir(username, fallback) {
    const lu = linuxUserOf(username); if (!lu) return { dir: fallback, linked: false, linuxUser: '' };
    const d = path.join(users()[lu].home, 'uploaded_files');
    return canWrite(d) ? { dir: d, linked: true, linuxUser: lu } : { dir: fallback, linked: false, linuxUser: lu };
  }
  function status(username, fallback) {
    const u = uploadsDir(username, fallback), lu = u.linuxUser;
    return { linuxUser: lu, linked: u.linked, folder: u.linked ? u.dir : '', hubUser: HUB_USER, sharedwork: '/srv/apps/sharedwork',
      howToLink: lu && !u.linked ? `As ${lu} on the server (or your AI agent there): sh ${path.join(path.resolve(ROOT, '..'), 'deploy', 'link-linux-account.sh')}` : '' };
  }
  return { linuxUserOf, uploadsDir, status, HUB_USER };
};
