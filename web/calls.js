// Mills voice / video calls, page side (hub side: bridge/calls.js). Loaded right after the main page
// script, so it may use the page's helpers ($, json, store, prefs, me, nickOf, colorOf, ic, label, layout,
// sysRoom, playTone, speak, chat keys …) at load and at call time; the main script calls into
// placeCall / leaveCall / renderCallBanner / callPollLoop and reads `call`.
//
// WebRTC between the browsers in a call (a small mesh: everyone connects to everyone). Media is
// DTLS-SRTP encrypted end to end; the hub only relays the set-up messages, and those are encrypted
// with the per-pair chat keys, so the hub cannot read them or slip in its own DTLS fingerprints.
// Codecs: Opus for voice (in-band FEC, DTX for silence, ~32 kb/s), VP9 → AV1 → VP8 → H.264 for video,
// all decoded natively by the browser. Camera always starts OFF; the microphone uses the noise
// suppression / echo cancellation / auto level settings (on by default). A shared screen travels as a
// third sender on every connection; Theater gives the call most of the window with the chat on the left.
const call = { conv: null, peers: new Map(), mic: null, cam: null, screen: null, ice: [], after: 0, ver: -1, polling: false, state: {}, gone: new Map(),
  local: typeof MediaStream === 'function' ? new MediaStream() : null, // the one stream id our audio + video are sent under (lip-sync)
  screenStream: typeof MediaStream === 'function' ? new MediaStream() : null }; // a second, constant stream id for the shared screen (3rd sender on every connection)
let callInit = false;
const VQ = { low: [320, 180, 15, 250], medium: [640, 360, 24, 700], high: [1280, 720, 30, 1800] }; // w, h, fps, kb/s
function audioConstraints() { const a = prefs().audio; return { noiseSuppression: a.noise, echoCancellation: a.echo, autoGainControl: a.agc, channelCount: 1 }; }
const callMsg = (t) => { $('callMsg').textContent = t || ''; };
const inCallList = (c) => call.state[c] || [];

async function callPollLoop() {
  if (call.polling) return;
  call.polling = true;
  while (me) {
    try {
      const d = await json(`/api/call/poll?after=${call.after}&ver=${call.ver}`);
      call.after = d.after; call.ver = d.ver;
      await onCallState(d.calls || {});
      for (const s of d.signals || []) await onSignal(s);
    } catch { await sleep(3000); }
  }
  call.polling = false;
}
async function onCallState(state) {
  const prev = call.state; call.state = state;
  for (const [c, ps] of Object.entries(state)) for (const p of ps) { // "X is in a call" — a notification, never a ringtone
    if (p.user === me.username || call.conv === c) continue;
    if (callInit && !(prev[c] || []).some((x) => x.user === p.user)) callNotify(c, p.user);
  }
  callInit = true;
  if (call.conv) {
    const list = inCallList(call.conv);
    const restarted = !list.some((p) => p.user === me.username);
    if (restarted) { // the hub restarted (or dropped us): quietly join again, and keep the existing connections meanwhile
      try { await json('/api/call/join', { method: 'POST', body: JSON.stringify({ conv: call.conv, cam: !!call.cam, mic: !!(call.mic && call.mic.enabled) }) }); } catch {}
    }
    // server permissions (bridge/guilds.js via the hub): a moderator's mute / no "speak" turns my mic off, no "stream" my camera + screen
    const self = list.find((p) => p.user === me.username);
    if (self && self.held && call.mic && call.mic.enabled) { call.mic.enabled = false; updateCallButtons(); callMsg(self.held === 'muted' ? 'A moderator muted you in calls on this server.' : 'You may not talk in calls here.'); }
    if (self && self.nostream && (call.cam || call.screen)) { if (call.cam) $('callCam').click(); if (call.screen) $('callScreen').click(); callMsg('You may not use the camera or share your screen here.'); }
    call.held = self ? self.held || '' : ''; call.nostream = !!(self && self.nostream);
    const others = list.filter((p) => p.user !== me.username);
    for (const p of others) if (!call.peers.has(p.user)) connectPeer(p.user);
    if (!restarted) for (const u of [...call.peers.keys()]) if (!others.some((p) => p.user === u)) dropPeer(u);
    for (const p of others) { const peer = call.peers.get(p.user); if (peer) { peer.tile.classList.toggle('nocam', !p.cam); const l = peer.tile.querySelector('.lbl'); l.textContent = nickOf(p.user); if (!p.mic) l.append(ic('mic-off', 'trail')); if (p.screen) l.append(ic('share', 'trail')); } }
    $('callInfo').textContent = convLabel(call.conv) + ' · ' + list.length + ' in call';
  }
  renderCallBanner();
}
function callNotify(conv, user) {
  const P = prefs();
  if (P.sound.on) playTone('mention', P.sound.mention);
  const t = document.createElement('div'); t.className = 'toast mention'; t.style.borderLeftColor = colorOf(user);
  const b = document.createElement('b'); b.style.color = colorOf(user); label(b, 'phone', nickOf(user));
  t.append(b, ' is in a ' + (conv === 'room' ? 'call in # Everyone' : 'private call with you') + ' — click to join');
  t.style.setProperty('--toast-s', '12s');
  t.onclick = () => { t.remove(); joinCall(conv); };
  t.addEventListener('animationend', () => t.remove());
  $('toasts').append(t);
  if (P.tts.on) speak(nickOf(user) + ' is in a call');
}
function renderCallBanner() {
  if (!me || !$('callBanner')) return;
  const list = inCallList(currentConv), b = $('callBanner'), btn = $('callBtn');
  label(btn, 'phone', call.conv === currentConv ? 'In call' : list.length ? 'Join call (' + list.length + ')' : 'Call');
  if (!list.length || call.conv === currentConv) { b.hidden = true; return; }
  b.textContent = '';
  const s = document.createElement('span'); s.append(ic('speaker', 'lead'));
  list.forEach((p, k) => { s.append((k ? ', ' : '') + nickOf(p.user)); if (p.cam) s.append(ic('camera', 'trail')); if (p.screen) s.append(ic('share', 'trail')); });
  s.append((list.length === 1 ? ' is' : ' are') + ' in a call here');
  const j = document.createElement('button'); j.type = 'button'; j.textContent = 'Join'; j.onclick = () => joinCall(currentConv);
  b.append(s, j); b.hidden = false;
}
$('callBtn').onclick = () => { if (call.conv === currentConv) placeCall(); else joinCall(currentConv); };

async function joinCall(conv) {
  if (call.conv === conv) return;
  if (call.conv) await leaveCall();
  if (!chatPriv) return sysRoom('Your chat key is locked in this browser. Sign out and sign in again to call.');
  if (!window.RTCPeerConnection) return sysRoom('This browser cannot make calls.');
  try { call.mic = (await navigator.mediaDevices.getUserMedia({ audio: audioConstraints() })).getAudioTracks()[0]; }
  catch { call.mic = null; }
  call.cam = null; call.conv = conv; call.gone.clear();
  try {
    const d = await json('/api/call/join', { method: 'POST', body: JSON.stringify({ conv, cam: false, mic: !!call.mic }) });
    call.ice = d.iceServers || [];
    $('callTiles').textContent = '';
    $('callTiles').append(makeTile(me.username, true));
    updateCallButtons();
    callMsg(call.mic ? '' : 'Microphone not available or not allowed: you are listening only. Allow it in the address bar and rejoin to talk.');
    placeCall();
    await onCallState(d.calls || {});
  } catch (err) { await leaveCall(true); sysRoom('Could not join the call: ' + err.message); }
}
async function leaveCall(quiet) {
  if (!call.conv) return;
  for (const u of [...call.peers.keys()]) dropPeer(u);
  if (me) removeMeter(me.username);
  if (call.mic) call.mic.stop();
  if (call.cam) call.cam.stop(); // camera light goes off
  if (call.screen) { call.screen.onended = null; call.screen.stop(); if (call.screenStream) call.screenStream.removeTrack(call.screen); }
  call.mic = call.cam = call.screen = null; call.conv = null;
  placeCall(); renderCallBanner();
  if (!quiet || me) json('/api/call/leave', { method: 'POST', body: '{}' }).catch(() => {});
}
$('callLeave').onclick = () => leaveCall();

// Per-person volume and mute: only in this browser (they change what YOU hear, nobody else), kept
// per username so they stick across calls. Effective volume = Settings call volume × this person's slider.
const personVol = (u) => { const v = Number(store.get('mills.vol.' + u)); return Number.isFinite(v) && store.get('mills.vol.' + u) !== null ? v : 100; };
const personMuted = (u) => store.get('mills.mute.' + u) === '1';
function applyPersonAudio(tile) {
  const u = tile.dataset.user, v = tile.querySelector('video');
  v.volume = Math.max(0, Math.min(1, (prefs().call.volume / 100) * (personVol(u) / 100)));
  v.muted = personMuted(u);
  const b = tile.querySelector('.pMute'), r = tile.querySelector('.pVol');
  if (b) { label(b, personMuted(u) ? 'speaker-off' : 'speaker'); b.title = (personMuted(u) ? 'Unmute ' : 'Mute ') + nickOf(u) + ' (only for you)'; b.classList.toggle('off', personMuted(u)); }
  if (r) { r.value = personVol(u); r.title = nickOf(u) + ': ' + personVol(u) + '% (only for you)'; }
}
function makeTile(user, self) {
  const t = document.createElement('div'); t.className = 'tile nocam' + (self ? ' self' : ''); t.dataset.user = user;
  t.style.borderColor = colorOf(user);
  const v = document.createElement('video'); v.autoplay = true; v.playsInline = true; if (self) v.muted = true;
  const ava = document.createElement('div'); ava.className = 'ava'; ava.style.background = colorOf(user); ava.textContent = nickOf(user).slice(0, 1).toUpperCase();
  const lbl = document.createElement('div'); lbl.className = 'lbl'; lbl.textContent = self ? 'You' : nickOf(user);
  t.append(v, ava, lbl);
  if (!self) {
    const ctl = document.createElement('div'); ctl.className = 'pCtl';
    const mute = document.createElement('button'); mute.type = 'button'; mute.className = 'pMute';
    mute.onclick = (e) => { e.stopPropagation(); store.set('mills.mute.' + user, personMuted(user) ? '' : '1'); applyPersonAudio(t); };
    const vol = document.createElement('input'); vol.type = 'range'; vol.className = 'pVol'; vol.min = 0; vol.max = 100;
    vol.oninput = () => { store.set('mills.vol.' + user, vol.value); if (personMuted(user) && Number(vol.value) > 0) store.set('mills.mute.' + user, ''); applyPersonAudio(t); };
    ctl.append(mute, vol); t.append(ctl);
    applyPersonAudio(t);
  }
  return t;
}

// Speaking ring: a tiny level meter per voice (Web Audio analyser, nothing leaves the browser).
// Someone counts as speaking while their level is above the threshold, held for a moment so the
// ring does not flicker between words.
const meters = new Map(); // username -> { an, src, buf, hold }
function addMeter(user, track) {
  removeMeter(user);
  try {
    audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
    if (audioCtx.state === 'suspended') audioCtx.resume();
    const src = audioCtx.createMediaStreamSource(new MediaStream([track]));
    const an = audioCtx.createAnalyser(); an.fftSize = 512; an.smoothingTimeConstant = 0.3;
    src.connect(an); // measured only, never played through here (the tile's <video> plays it)
    meters.set(user, { an, src, buf: new Uint8Array(an.fftSize), hold: 0, track });
  } catch {}
}
function removeMeter(user) { const m = meters.get(user); if (m) { try { m.src.disconnect(); } catch {} meters.delete(user); } }
setInterval(() => {
  if (!call.conv) return;
  const now = Date.now();
  for (const [user, m] of meters) {
    m.an.getByteTimeDomainData(m.buf);
    let sum = 0; for (const x of m.buf) { const d = (x - 128) / 128; sum += d * d; }
    const level = Math.sqrt(sum / m.buf.length);
    const self = user === me.username;
    const audible = self ? !!(call.mic && call.mic.enabled) : true; // others ring even if you muted them locally (you still see who talks)
    if (audible && m.track.enabled && m.track.readyState === 'live' && level > 0.035) m.hold = now + 350;
    const tile = self ? selfTile() : (call.peers.get(user) || {}).tile;
    if (tile) tile.classList.toggle('speaking', m.hold > now);
  }
}, 100);
function selfTile() { return $('callTiles').querySelector('.tile.self'); }

function preferCodecs(t, kind) {
  if (!t.setCodecPreferences || !RTCRtpReceiver.getCapabilities) return;
  const caps = RTCRtpReceiver.getCapabilities(kind);
  if (!caps) return;
  const order = kind === 'audio' ? ['audio/opus'] : ['video/VP9', 'video/AV1', 'video/VP8', 'video/H264'];
  const rank = (c) => { const i = order.indexOf(c.mimeType); return i < 0 ? order.length : i; };
  try { t.setCodecPreferences([...caps.codecs].sort((a, b) => rank(a) - rank(b))); } catch {}
}
function tuneSdp(desc) { // ask the other side for Opus with forward error correction + silence suppression
  let sdp = desc.sdp;
  const m = /a=rtpmap:(\d+) opus\/48000/i.exec(sdp);
  if (m) sdp = sdp.replace(new RegExp('a=fmtp:' + m[1] + ' ([^\\r\\n]*)'), (line, params) => {
    const add = ['useinbandfec=1', 'usedtx=1', 'maxaveragebitrate=32000'].filter((x) => !params.includes(x.split('=')[0]));
    return 'a=fmtp:' + m[1] + ' ' + params + (add.length ? ';' + add.join(';') : '');
  });
  return { type: desc.type, sdp };
}
// Every connection carries three senders in a fixed order: audio, camera, shared screen (the screen one
// sends nothing until someone shares). Older pages have two; the third is simply absent for them.
const videoTransceivers = (pc) => pc.getTransceivers().filter((t) => t.receiver.track.kind === 'video');
const isScreenTransceiver = (pc, t) => videoTransceivers(pc)[1] === t;
function tuneSenders(peer) {
  const [, , fps, kbps] = VQ[prefs().call.quality] || VQ.medium;
  for (const t of peer.pc.getTransceivers()) {
    const s = t.sender, kind = t.receiver.track.kind, params = s.getParameters(), screen = isScreenTransceiver(peer.pc, t);
    if (!params.encodings || !params.encodings.length) continue; // not negotiated yet; tried again once connected
    params.encodings[0].maxBitrate = (screen ? 2500 : kind === 'video' ? kbps : 32) * 1000; // a screen needs crisp text, not frames
    if (kind === 'video') params.encodings[0].maxFramerate = screen ? 15 : fps;
    s.setParameters(params).catch(() => {});
  }
}
// Lip-sync: browsers only synchronise audio and video that belong to the same stream (the SDP msid).
// Both senders are tied to one local stream id, and the receiving side plays the remote stream the
// tracks arrive in, so voice and picture stay in step instead of drifting apart.
async function attachLocal(peer) { // put our current mic / camera / shared screen on this connection's senders
  const ts = peer.pc.getTransceivers();
  for (const t of ts) { // synchronously, before any offer / answer is made
    t.direction = 'sendrecv'; preferCodecs(t, t.receiver.track.kind);
    if (t.sender.setStreams) try { t.sender.setStreams(isScreenTransceiver(peer.pc, t) ? call.screenStream : call.local); } catch {}
  }
  for (const t of ts) { try { await t.sender.replaceTrack(t.receiver.track.kind === 'audio' ? call.mic : isScreenTransceiver(peer.pc, t) ? call.screen : call.cam); } catch {} }
}
function connectPeer(user) {
  if (call.peers.has(user)) return call.peers.get(user);
  const pc = new RTCPeerConnection({ iceServers: call.ice });
  const peer = { pc, user, offerer: me.username < user, pendingIce: [], iceOut: [], iceTimer: null, stream: new MediaStream(), tile: makeTile(user, false) };
  call.peers.set(user, peer);
  $('callTiles').append(peer.tile);
  peer.tile.querySelector('video').srcObject = peer.stream;
  pc.ontrack = (e) => {
    if (e.track.kind === 'video' && isScreenTransceiver(pc, e.transceiver)) { // their shared screen: its own tile, shown while the track carries video
      const stream = (e.streams && e.streams[0]) || new MediaStream([e.track]);
      const show = () => showScreenTile(user, stream, false), hide = () => removeScreenTile(user);
      e.track.onunmute = show; e.track.onmute = hide; e.track.onended = hide;
      if (!e.track.muted) show();
      return;
    }
    const v = peer.tile.querySelector('video');
    if (e.streams && e.streams[0]) { // the sender's stream: audio + video in one, so the browser lip-syncs them
      if (v.srcObject !== e.streams[0]) { peer.stream = e.streams[0]; v.srcObject = peer.stream; }
    } else if (!peer.stream.getTracks().includes(e.track)) peer.stream.addTrack(e.track);
    v.play().catch(() => {});
    if (e.track.kind === 'audio') addMeter(user, e.track);
  };
  // ICE candidates arrive in quick bursts: gather them for 40 ms and send one signal per burst (each
  // signal is a round trip through the tunnel, so this cuts set-up time on slow links).
  pc.onicecandidate = (e) => {
    if (!e.candidate) return;
    peer.iceOut.push(e.candidate.toJSON());
    if (!peer.iceTimer) peer.iceTimer = setTimeout(() => { peer.iceTimer = null; sendSignal(user, { cs: peer.iceOut.splice(0) }); }, 40);
  };
  pc.onconnectionstatechange = () => {
    if (pc.connectionState === 'connected') tuneSenders(peer);
    if (pc.connectionState === 'failed' && peer.offerer) pc.restartIce();
    if (pc.connectionState === 'failed') callMsg('Could not connect to ' + nickOf(user) + ' directly (strict network). A TURN server in the hub config fixes this.');
  };
  if (peer.offerer) { // exactly one side offers, so there is never a collision
    pc.addTransceiver('audio', { direction: 'sendrecv', streams: [call.local] });
    pc.addTransceiver('video', { direction: 'sendrecv', streams: [call.local] });
    pc.addTransceiver('video', { direction: 'sendrecv', streams: [call.screenStream] }); // the shared screen (silent until someone shares)
    attachLocal(peer);
    pc.onnegotiationneeded = async () => {
      try { await pc.setLocalDescription(); await sendSignal(user, { d: tuneSdp(pc.localDescription) }); } catch {}
    };
  }
  return peer;
}
function dropPeer(user) {
  const peer = call.peers.get(user);
  if (!peer) return;
  clearTimeout(peer.iceTimer);
  try { peer.pc.close(); } catch {}
  removeMeter(user); removeScreenTile(user);
  peer.tile.remove(); call.peers.delete(user); call.gone.delete(user);
}
// ── Screen share: a window / tab / whole screen as an extra tile everyone in the call sees ──
const screenTile = (user) => $('callTiles').querySelector('.screenTile[data-user="' + CSS.escape(user) + '"]');
function showScreenTile(user, stream, self) {
  let t = screenTile(user);
  if (!t) {
    t = document.createElement('div'); t.className = 'tile screenTile'; t.dataset.user = user; t.style.borderColor = colorOf(user);
    const v = document.createElement('video'); v.autoplay = true; v.playsInline = true; v.muted = true;
    const l = document.createElement('div'); l.className = 'lbl'; l.append(ic('share', 'lead'), (self ? 'Your' : nickOf(user) + "'s") + ' screen');
    t.append(v, l); t.title = 'Click for theater view'; t.onclick = () => { if (!theaterOn()) setTheater(true); };
    $('callTiles').prepend(t);
  }
  const v = t.querySelector('video'); if (v.srcObject !== stream) v.srcObject = stream; v.play().catch(() => {});
}
function removeScreenTile(user) { const t = screenTile(user); if (t) t.remove(); }
async function stopScreen() {
  if (!call.screen) return;
  call.screen.onended = null; call.screen.stop();
  if (call.screenStream) call.screenStream.removeTrack(call.screen);
  call.screen = null;
  for (const p of call.peers.values()) await attachLocal(p); // the third sender goes quiet: their tile disappears
  if (me) removeScreenTile(me.username);
  updateCallButtons(); pushCallState();
}
$('callScreen').onclick = async () => {
  if (!call.conv) return;
  if (call.screen) return stopScreen();
  if (call.nostream) return callMsg('You may not share your screen here.');
  if (!navigator.mediaDevices || !navigator.mediaDevices.getDisplayMedia) return callMsg('This browser cannot share a screen (needs Chrome, Edge or Firefox on a computer).');
  let s;
  try { s = await navigator.mediaDevices.getDisplayMedia({ video: { frameRate: { ideal: 15, max: 30 } }, audio: false }); }
  catch (e) { return callMsg(e.name === 'NotAllowedError' ? 'Screen sharing was cancelled.' : 'Screen share: ' + e.message); }
  call.screen = s.getVideoTracks()[0]; call.screen.contentHint = 'detail';
  call.screen.onended = () => stopScreen(); // the browser's own "Stop sharing" bar
  call.screenStream.addTrack(call.screen); // same stream id as always, so no renegotiation is needed
  for (const p of call.peers.values()) { await attachLocal(p); tuneSenders(p); }
  showScreenTile(me.username, call.screenStream, true);
  updateCallButtons(); pushCallState();
};
// ── Theater: the call takes most of the screen, the chat stays on the left ──
const theaterOn = () => store.get('mills.theater') === '1';
function setTheater(on) { store.set('mills.theater', on ? '1' : ''); placeCall(); updateCallButtons(); }
$('callTheater').onclick = () => setTheater(!theaterOn());
document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && call.conv && theaterOn() && !e.target.closest('input, textarea, select')) setTheater(false); });
async function pairKey(user) { const m = memberOf(user); return m && m.pub && chatPriv && !changedKeys.has(user) && trustedPub(user, m.pub) ? sharedWith(m.pub) : null; }
async function sendSignal(user, body) {
  const k = await pairKey(user);
  if (!k || !call.conv) return;
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = await crypto.subtle.encrypt({ name: 'AES-GCM', iv }, k, enc.encode(JSON.stringify({ conv: call.conv, from: me.username, to: user, ...body })));
  await json('/api/call/signal', { method: 'POST', body: JSON.stringify({ to: user, conv: call.conv, iv: toB64(iv), ct: toB64(ct) }) }).catch(() => {});
}
async function onSignal(s) {
  if (!call.conv || s.conv !== call.conv) return;
  const k = await pairKey(s.from);
  if (!k) return;
  let msg;
  try { msg = JSON.parse(dec.decode(await crypto.subtle.decrypt({ name: 'AES-GCM', iv: fromB64(s.iv) }, k, fromB64(s.ct)))); } catch { return; } // not from them: ignored
  if (msg.conv !== call.conv || msg.from !== s.from || msg.to !== me.username) return; // bound to this call inside the encryption
  const peer = connectPeer(s.from), pc = peer.pc;
  try {
    if (msg.d) {
      await pc.setRemoteDescription(msg.d);
      if (msg.d.type === 'offer') { await attachLocal(peer); await pc.setLocalDescription(); await sendSignal(s.from, { d: tuneSdp(pc.localDescription) }); }
      for (const c of peer.pendingIce.splice(0)) { try { await pc.addIceCandidate(c); } catch {} }
    } else if (msg.c || Array.isArray(msg.cs)) { // one candidate (older pages) or a batch
      for (const c of msg.cs || [msg.c]) {
        if (pc.remoteDescription) { try { await pc.addIceCandidate(c); } catch {} } else peer.pendingIce.push(c);
      }
    }
  } catch (e) { callMsg('Call set-up problem with ' + nickOf(s.from) + ': ' + e.message); }
}

function updateCallButtons() {
  const micOn = !!(call.mic && call.mic.enabled), camOn = !!call.cam;
  if (call.mic) label($('callMic'), micOn ? 'mic-off' : 'mic', micOn ? 'Mute' : 'Unmute'); else label($('callMic'), 'mic', 'Allow mic');
  $('callMic').title = call.mic ? (micOn ? 'Mute your microphone (M)' : 'Unmute your microphone (M) — you are muted') : 'Try to use your microphone again';
  if (call.mic && !meters.has(me.username)) addMeter(me.username, call.mic);
  $('callMic').classList.toggle('off', !micOn);
  label($('callCam'), 'camera', camOn ? 'Camera on' : 'Camera off');
  $('callCam').classList.toggle('off', !camOn);
  label($('callScreen'), 'share', call.screen ? 'Stop sharing' : 'Share screen'); $('callScreen').classList.toggle('on', !!call.screen);
  $('callScreen').hidden = !(navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia);
  label($('callTheater'), 'eye', theaterOn() ? 'Exit theater' : 'Theater'); $('callTheater').classList.toggle('on', theaterOn());
  const st = selfTile();
  if (st) { st.classList.toggle('nocam', !camOn); st.querySelector('video').srcObject = call.cam ? new MediaStream([call.cam]) : null; const l = st.querySelector('.lbl'); l.textContent = 'You'; if (!micOn) l.append(ic('mic-off', 'trail')); }
}
function pushCallState() {
  if (call.conv) json('/api/call/state', { method: 'POST', body: JSON.stringify({ conv: call.conv, cam: !!call.cam, mic: !!(call.mic && call.mic.enabled), screen: !!call.screen }) }).catch(() => {});
}
$('callMic').onclick = async () => {
  if (!call.conv) return;
  if (call.held && !(call.mic && call.mic.enabled)) return callMsg(call.held === 'muted' ? 'A moderator muted you in calls on this server.' : 'You may not talk in calls here.');
  if (!call.mic) { // permission was refused earlier: try again
    try { call.mic = (await navigator.mediaDevices.getUserMedia({ audio: audioConstraints() })).getAudioTracks()[0]; callMsg(''); }
    catch { return callMsg('Microphone still not allowed.'); }
    for (const p of call.peers.values()) await attachLocal(p);
  } else call.mic.enabled = !call.mic.enabled;
  updateCallButtons(); pushCallState();
};
document.addEventListener('keydown', (e) => { // M = mute / unmute yourself while in a call (not while typing)
  if (!call.conv || e.ctrlKey || e.metaKey || e.altKey || (e.key !== 'm' && e.key !== 'M')) return;
  if (e.target.closest('input, textarea, select, [contenteditable]')) return;
  e.preventDefault(); $('callMic').click();
});
$('callCam').onclick = async () => {
  if (!call.conv) return;
  if (call.cam) { call.cam.stop(); call.cam = null; }
  else if (call.nostream) return callMsg('You may not turn the camera on here.');
  else {
    const [w, h, fps] = VQ[prefs().call.quality] || VQ.medium;
    try { call.cam = (await navigator.mediaDevices.getUserMedia({ video: { width: { ideal: w }, height: { ideal: h }, frameRate: { ideal: fps, max: fps } } })).getVideoTracks()[0]; }
    catch (e) { return callMsg('Camera not available: ' + (e.name === 'NotAllowedError' ? 'permission was refused (allow it in the address bar).' : e.message)); }
    call.cam.contentHint = 'motion';
  }
  for (const p of call.peers.values()) { await attachLocal(p); tuneSenders(p); }
  updateCallButtons(); pushCallState();
};

// Where the call panel lives: in the chat's side bar while the chat is open (Chat screen, or the
// team chat shown in Work with its side bar visible); otherwise in its own window that snaps to
// the nearest edge or corner wherever it is dropped.
function chatOpen() {
  if (!me || $('roomCore').classList.contains('noside')) return false;
  if (!$('room').hidden) return true;
  return !$('shell').hidden && layout.show.roomPane;
}
const chatVisible = () => !!me && (!$('room').hidden || (!$('shell').hidden && layout.show.roomPane)); // the team chat is on screen (side list or not)
function placeCall() {
  const core = $('callCore'), stage = $('callStage'), rc = $('roomCore');
  const theater = !!call.conv && theaterOn() && chatVisible();
  rc.classList.toggle('theater', theater); stage.hidden = !theater;
  if (!call.conv) { $('callPark').append(core); $('callFloat').hidden = true; return; }
  if (theater) { stage.append(core); $('callFloat').hidden = true; }
  else if (chatOpen()) { $('callSideHost').append(core); $('callFloat').hidden = true; }
  else { $('callFloatBody').append(core); $('callFloat').hidden = false; snapCall(store.get('mills.callSnap') || 'br'); }
}
const SNAPS = { tl: [0, 0], tc: [0.5, 0], tr: [1, 0], ml: [0, 0.5], mr: [1, 0.5], bl: [0, 1], bc: [0.5, 1], br: [1, 1] };
function snapCall(anchor) {
  const win = $('callFloat'), m = 12, [fx, fy] = SNAPS[anchor] || SNAPS.br;
  const w = win.offsetWidth, h = win.offsetHeight;
  win.style.left = Math.round(m + fx * (innerWidth - w - 2 * m)) + 'px';
  win.style.top = Math.round(m + fy * (innerHeight - h - 2 * m)) + 'px';
}
(function bindCallFloat() {
  const win = $('callFloat'), bar = win.querySelector('.winBar');
  bar.addEventListener('pointerdown', (e) => {
    const r = win.getBoundingClientRect(), sx = e.clientX - r.left, sy = e.clientY - r.top;
    bar.setPointerCapture(e.pointerId); win.classList.add('dragging');
    const move = (ev) => { win.style.left = (ev.clientX - sx) + 'px'; win.style.top = (ev.clientY - sy) + 'px'; };
    const up = () => {
      bar.removeEventListener('pointermove', move); bar.removeEventListener('pointerup', up); win.classList.remove('dragging');
      const b = win.getBoundingClientRect(), cx = (b.left + b.width / 2) / innerWidth, cy = (b.top + b.height / 2) / innerHeight;
      let best = 'br', dist = Infinity; // snap to whichever of the 8 edge / corner spots is closest
      for (const [k, [fx, fy]] of Object.entries(SNAPS)) { const d = (fx - cx) ** 2 + (fy - cy) ** 2; if (d < dist) { dist = d; best = k; } }
      store.set('mills.callSnap', best); snapCall(best);
    };
    bar.addEventListener('pointermove', move); bar.addEventListener('pointerup', up);
  });
  window.addEventListener('resize', () => { if (!win.hidden) snapCall(store.get('mills.callSnap') || 'br'); });
})();
window.addEventListener('pagehide', () => { // closing the tab leaves the call right away
  if (call.conv && token && base) fetch(base + '/api/call/leave', { method: 'POST', keepalive: true, headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + token }, body: '{}' }).catch(() => {});
});
