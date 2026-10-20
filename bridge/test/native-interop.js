// The Rust core (native/crates/vr-core) against the JavaScript side, both ways, for R163 of the regression suite:
// the AI host session key + envelope (as bridge/ai-nodes.js does it), and the chat's end-to-end encryption exactly as
// the page does it (web/index.html sealFor / decryptMsg / sealOwn / sealChatFiles, WebCrypto here too).
// run(interopBinary) → { ok, parts: { name: true/false }, why }
'use strict';
const crypto = require('crypto');
const zlib = require('zlib');
const { spawnSync } = require('child_process');
const subtle = crypto.webcrypto.subtle;
const enc = new TextEncoder(), dec = new TextDecoder();
const b64 = (b) => Buffer.from(b).toString('base64');
const unb64 = (s) => new Uint8Array(Buffer.from(s, 'base64'));
const ECDH = { name: 'ECDH', namedCurve: 'P-256' };

function padBucket(n) { if (n > 65536) return Math.ceil(n / 65536) * 65536; let b = 256; while (b < n) b *= 2; return b; }
function frame(bytes) { const out = new Uint8Array(padBucket(4 + bytes.length)); new DataView(out.buffer).setUint32(0, bytes.length); out.set(bytes, 4); return out; }
function unframe(b) { const n = new DataView(b.buffer, b.byteOffset).getUint32(0); return b.subarray(4, 4 + n); }
const bodyAad = (from, to, mid, v) => 'mills-v' + v + '|' + from + '|' + (to || '') + '|' + mid;
async function wrapKey(priv, pubJwk) {
  const bits = await subtle.deriveBits({ name: 'ECDH', public: await subtle.importKey('jwk', { kty: 'EC', crv: 'P-256', x: pubJwk.x, y: pubJwk.y }, ECDH, false, []) }, priv, 256);
  const base = await subtle.importKey('raw', bits, 'HKDF', false, ['deriveKey']);
  return subtle.deriveKey({ name: 'HKDF', hash: 'SHA-256', salt: new Uint8Array(0), info: enc.encode('mills-v3 wrap') }, base, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
}
async function person() {
  const kp = await subtle.generateKey(ECDH, true, ['deriveKey', 'deriveBits']);
  const jwk = await subtle.exportKey('jwk', kp.privateKey), pub = await subtle.exportKey('jwk', kp.publicKey);
  return { priv: kp.privateKey, jwk, pub: { kty: 'EC', crv: 'P-256', x: pub.x, y: pub.y } };
}
async function pageSeal(sender, from, readers, plain) { // sealFor, format 4
  const mid = crypto.randomBytes(16).toString('base64url'), aad = bodyAad(from, '', mid, 4);
  const raw = crypto.randomBytes(32), iv = crypto.randomBytes(12);
  const k = await subtle.importKey('raw', raw, 'AES-GCM', false, ['encrypt']);
  const ct = await subtle.encrypt({ name: 'AES-GCM', iv, additionalData: enc.encode(aad) }, k, frame(new Uint8Array(zlib.deflateRawSync(Buffer.from(plain)))));
  const keys = {};
  for (const [u, pub] of readers) { const kiv = crypto.randomBytes(12); keys[u] = { iv: b64(kiv), wk: b64(await subtle.encrypt({ name: 'AES-GCM', iv: kiv, additionalData: enc.encode(aad + '|' + u) }, await wrapKey(sender.priv, pub), raw)) }; }
  return { v: 4, mid, iv: b64(iv), ct: b64(ct), keys };
}
async function pageOpen(reader, me, m, fromPub) { // decryptMsg, formats 3 + 4
  const aad = bodyAad(m.from, m.to, m.mid, m.v);
  const raw = await subtle.decrypt({ name: 'AES-GCM', iv: unb64(m.keys[me].iv), additionalData: enc.encode(aad + '|' + me) }, await wrapKey(reader.priv, fromPub), unb64(m.keys[me].wk));
  const k = await subtle.importKey('raw', raw, 'AES-GCM', false, ['decrypt']);
  const body = new Uint8Array(await subtle.decrypt({ name: 'AES-GCM', iv: unb64(m.iv), additionalData: enc.encode(aad) }, k, unb64(m.ct)));
  return m.v === 4 ? zlib.inflateRawSync(Buffer.from(unframe(body))).toString('utf8') : dec.decode(body).trimEnd();
}

async function run(bin) {
  const rust = (req) => { const r = spawnSync(bin, [], { input: JSON.stringify(req), encoding: 'utf8', timeout: 20000 }); try { return JSON.parse(r.stdout); } catch { return { error: (r.stderr || r.stdout || String(r.error)).slice(0, 200) }; } };
  const parts = {}; const why = [];
  const note = (k, ok, detail) => { parts[k] = !!ok; if (!ok) why.push(`${k}: ${JSON.stringify(detail).slice(0, 200)}`); };
  // the AI host session (bridge/ai-nodes.js derive / seal / keyId)
  const secret = crypto.randomBytes(24).toString('hex'), hn = crypto.randomBytes(16).toString('hex'), sn = crypto.randomBytes(16).toString('hex');
  const key = Buffer.from(crypto.hkdfSync('sha256', Buffer.from(secret, 'utf8'), Buffer.from(hn + sn, 'hex'), 'viberoller-ai-host-v1', 32));
  const kid = 'vr-' + crypto.createHash('sha256').update(key).digest('hex').slice(0, 24);
  const hk = rust({ op: 'host-key', secret, hostNonce: hn, serverNonce: sn });
  note('host kid', hk.kid === kid, hk);
  let opened = null; try { const iv = Buffer.from(hk.sealed.iv, 'base64'), ct = Buffer.from(hk.sealed.ct, 'base64'); const d = crypto.createDecipheriv('aes-256-gcm', key, iv); d.setAuthTag(ct.subarray(ct.length - 16)); opened = JSON.parse(Buffer.concat([d.update(ct.subarray(0, ct.length - 16)), d.final()]).toString('utf8')); } catch {}
  note('host Rust seals, Node opens', opened && opened.x === 1, opened);
  const iv = crypto.randomBytes(12), c = crypto.createCipheriv('aes-256-gcm', key, iv); const ct = Buffer.concat([c.update(JSON.stringify({ job: 'r163' }), 'utf8'), c.final(), c.getAuthTag()]);
  const ho = rust({ op: 'host-open', secret, hostNonce: hn, serverNonce: sn, env: { iv: iv.toString('base64'), ct: ct.toString('base64') } });
  note('host Node seals, Rust opens', ho.value && ho.value.job === 'r163', ho);
  const msg = JSON.stringify({ version: '1', commit: 'c', files: [] });
  note('hmac', rust({ op: 'hmac', key: secret, msg }).sig === crypto.createHmac('sha256', secret).update(msg).digest('hex'), 'differs');
  // the chat
  const alice = await person(), bob = await person();
  const plain = JSON.stringify({ v: 2, t: 'hello from the page r163 ' + 'x'.repeat(300) });
  const fromPage = await pageSeal(alice, 'alice', [['alice', alice.pub], ['bob', bob.pub]], plain);
  const ro = rust({ op: 'chat-open', jwk: bob.jwk, me: 'bob', msg: { v: 4, mid: fromPage.mid, from: 'alice', iv: fromPage.iv, ct: fromPage.ct, key: fromPage.keys.bob, fromPub: alice.pub } });
  note('chat page seals, Rust opens', ro.plain === plain, ro);
  for (const v4 of [true, false]) {
    const rs = rust({ op: 'chat-seal', jwk: bob.jwk, from: 'bob', to: '', readers: [['alice', alice.pub], ['bob', bob.pub]], plain, v4 });
    let back = null; try { back = await pageOpen(alice, 'alice', { ...rs, from: 'bob', to: '' }, bob.pub); } catch (e) { back = String(e); }
    note(`chat Rust seals v${v4 ? 4 : 3}, page opens`, back === plain, { rs, back });
  }
  // the uploader's own seal + a chat file
  const shared = await subtle.deriveKey({ name: 'ECDH', public: await subtle.importKey('jwk', { ...alice.pub }, ECDH, false, []) }, alice.priv, { name: 'AES-GCM', length: 256 }, false, ['encrypt']);
  const oiv = crypto.randomBytes(12), info = { n: 'notes r163.md', m: 'text/markdown', s: 5, k: 'k', i: 'i' };
  const seal = b64(oiv) + '.' + b64(await subtle.encrypt({ name: 'AES-GCM', iv: oiv, additionalData: enc.encode('vr-own-seal-v1') }, shared, enc.encode(JSON.stringify(info))));
  const oo = rust({ op: 'own-open', jwk: alice.jwk, seal });
  note('own seal page → Rust', oo.info && oo.info.n === 'notes r163.md', oo);
  const fk = crypto.randomBytes(32), fiv = crypto.randomBytes(12);
  const fct = await subtle.encrypt({ name: 'AES-GCM', iv: fiv, additionalData: enc.encode('vr-file-v1') }, await subtle.importKey('raw', fk, 'AES-GCM', false, ['encrypt']), enc.encode('file body r163'));
  note('file page → Rust', rust({ op: 'file-open', ct: b64(fct), k: b64(fk), i: b64(fiv) }).plain === 'file body r163', 'differs');
  return { ok: Object.values(parts).every(Boolean), parts, why: why.join('; ') };
}
module.exports = { run };
if (require.main === module) run(process.argv[2]).then((r) => { console.log(JSON.stringify(r, null, 1)); process.exit(r.ok ? 0 : 1); });
