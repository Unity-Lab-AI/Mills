// Mills usage: the host account's 5-hour / 7-day Claude limits (shared by every Work run on it) and
// each account's context window. Claude Code reports the limits in its stream (rate-limit events:
// status, utilization, resetsAt); until one has been seen, the windows are estimated from when the host
// started using Claude (a 5-hour window opens with the first message after the previous one ran out).
// The context window is per account and per Claude session: input + cache tokens of the latest reply
// out of the model's window. State: bridge/usage.json (gitignored).
'use strict';

const path = require('path');

const FIVE_H = 5 * 3600e3, SEVEN_D = 7 * 86400e3;
const toMs = (t) => { const n = typeof t === 'number' ? (t < 1e12 ? t * 1000 : t) : Date.parse(t); return Number.isFinite(n) ? n : null; };

// Several sources of Claude usage: this hub's own login ("hub", the first host's) and every AI host's Claude
// ("h:<hostId>/<provider>", Gee's PC after the move). Each keeps its own 5h / 7d windows; a person sees the windows of
// the source their current provider uses.
module.exports = function createUsage({ ROOT, config, readJson, writeJson }) {
  const FILE = path.join(ROOT, 'usage.json');
  const usage = readJson(FILE, {}); // { limits: { five_hour, seven_day }, fiveStart, weekStart, sources: { <source>: { limits, fiveStart, weekStart } } }
  if (!usage.sources || typeof usage.sources !== 'object') usage.sources = {};
  let timer = null;
  const save = () => { clearTimeout(timer); timer = setTimeout(() => writeJson(FILE, usage), 1000); };
  const bucket = (source) => (!source || source === 'hub' ? usage : (usage.sources[source] = usage.sources[source] || {}));

  function noteActivity(source) { // a Work run on that source's Claude account started
    const b = bucket(source), now = Date.now();
    if (!b.fiveStart || now - b.fiveStart >= FIVE_H) b.fiveStart = now;
    if (!b.weekStart || now - b.weekStart >= SEVEN_D) b.weekStart = now;
    save();
  }
  function noteUsage(acct, ev, source) { // one stream event of a Work run
    const b = bucket(source);
    if (/rate_limit/.test(String(ev.type || ''))) { // e.g. { type: "rate_limit_event", rate_limit_info: { rateLimitType, status, utilization, resetsAt } }
      const info = ev.rate_limit_info || ev.rateLimitInfo || ev;
      const kind = String(info.rateLimitType || info.rate_limit_type || info.type || '').toLowerCase();
      const key = /seven|week|7d/.test(kind) ? 'seven_day' : /five|5h|session/.test(kind) ? 'five_hour' : null;
      if (key) {
        // An event may carry only some fields: keep the last known reset time / utilization for the same
        // window instead of wiping them. Utilization is stored as a fraction 0–1 (a value above 1 is a percentage).
        const L = b.limits || (b.limits = {}), prev = L[key] || {};
        const resetsAt = toMs(info.resetsAt || info.resets_at) || (prev.resetsAt > Date.now() ? prev.resetsAt : null);
        const raw = info.utilization ?? info.usage ?? null, u = raw == null || raw === '' ? NaN : Number(raw);
        const sameWindow = prev.resetsAt && resetsAt === prev.resetsAt;
        if (prev.exact && prev.resetsAt > Date.now() && !Number.isFinite(u)) { // Anthropic's header numbers stand; an event without a number only updates the status
          if (info.status) { L[key] = { ...prev, status: String(info.status) }; save(); }
          return;
        }
        L[key] = {
          status: String(info.status || (sameWindow ? prev.status : '') || ''),
          utilization: Number.isFinite(u) ? Math.max(0, Math.min(1, u > 1 ? u / 100 : u)) : sameWindow ? prev.utilization ?? null : null,
          resetsAt, at: Date.now(),
        };
        save();
      }
    }
    const u = ev.type === 'assistant' && ev.message && ev.message.usage;
    if (u) {
      const tokens = (u.input_tokens || 0) + (u.cache_read_input_tokens || 0) + (u.cache_creation_input_tokens || 0);
      acct.state.ctx = { ...(acct.state.ctx || {}), session: acct.state.claudeSessionId || ev.session_id || '', tokens, model: ev.message.model || '', at: Date.now() };
    }
    if (ev.type === 'result' && ev.modelUsage && typeof ev.modelUsage === 'object') {
      const windows = Object.values(ev.modelUsage).map((m) => Number(m && m.contextWindow)).filter((n) => n > 0);
      if (windows.length && acct.state.ctx) acct.state.ctx.window = Math.max(...windows);
    }
    if (ev.type === 'result' && /rate.?limit|usage limit/i.test(String(ev.result || ''))) { // the run was refused: mark as out of usage
      (b.limits || (b.limits = {})).five_hour = { ...((b.limits || {}).five_hour || {}), status: 'rejected', at: Date.now() };
      save();
    }
  }
  // Anthropic's own numbers for one source (the anthropic-ratelimit-unified-5h / -7d headers of every relayed model call):
  // exact utilization and reset time, kept here (on disk) so they survive a restart and win over Claude Code's events,
  // which often carry no utilization at all.
  function noteWindows(source, w) {
    if (!w || typeof w !== 'object') return;
    const b = bucket(source), L = b.limits || (b.limits = {});
    let changed = false;
    for (const key of ['five_hour', 'seven_day']) {
      const x = w[key], u = x && x.utilization != null && x.utilization !== '' ? Number(x.utilization) : NaN, resetsAt = x ? toMs(x.resetsAt) : null;
      if (!Number.isFinite(u) || !resetsAt) continue;
      L[key] = { status: String(x.status || ''), utilization: Math.max(0, Math.min(1, u > 1 ? u / 100 : u)), resetsAt, at: Date.now(), exact: true };
      changed = true;
    }
    if (changed) save();
  }
  const windows = (source) => { const L = bucket(source).limits || {}, now = Date.now(), ok = (l) => (l && l.exact && l.resetsAt > now ? { utilization: l.utilization, resetsAt: l.resetsAt, status: l.status } : null); return { five_hour: ok(L.five_hour), seven_day: ok(L.seven_day) }; };
  function status(acct, source) { // the /api/usage answer for one account: the windows of the source its provider uses
    const b = bucket(source), now = Date.now(), L = b.limits || {};
    const win = (key, start, len) => { // official numbers when Claude Code reported them, otherwise the estimate
      const l = L[key];
      if (l && l.resetsAt && l.resetsAt > now) return { source: 'claude', status: l.status, utilization: l.utilization, resetsAt: l.resetsAt };
      const s = start && now - start < len ? start : null;
      return { source: 'estimate', status: l && l.status === 'rejected' && now - l.at < len ? 'rejected' : 'allowed', utilization: null, resetsAt: s ? s + len : null };
    };
    const ctx = acct.state.ctx && acct.state.ctx.session === (acct.state.claudeSessionId || '') ? acct.state.ctx : null;
    return { now, of: source || 'hub', five_hour: win('five_hour', b.fiveStart, FIVE_H), seven_day: win('seven_day', b.weekStart, SEVEN_D),
      context: ctx ? { tokens: ctx.tokens, window: ctx.window || config.contextWindow || 200000, model: ctx.model } : null };
  }
  return { noteActivity, noteUsage, noteWindows, windows, status };
};
