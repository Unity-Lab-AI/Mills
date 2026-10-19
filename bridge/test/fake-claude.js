#!/usr/bin/env node
// Stand-in for `claude -p --output-format stream-json` used ONLY by the regression runner's
// throwaway hub. Speaks the same NDJSON events the web app reads, costs nothing, touches nothing.
//   message contains "SLOW"   → waits 4 s before answering (busy / 409 / stop / deferred restart)
//   message contains "ASSISTTOOL" → adds a task through the Unity section's tool relay
//   message contains "PCTOOL" → calls the pc "list" tool through the hub, like the real MCP relay
//   message contains "USAGE"  → reports token usage (54,000 of a 200,000 context window) and a
//                               5-hour rate-limit event (42% used, resets in 2 h), like the real stream
// The answer echoes the model and the working folder (cwd) so the runner can check both.
'use strict';
const fs = require('fs');
const crypto = require('crypto');

const argv = process.argv.slice(2);
const opt = (n) => { const i = argv.indexOf(n); return i >= 0 ? argv[i + 1] : ''; };
const model = opt('--model') || 'default';
const session = opt('--resume') || 'fake-' + crypto.randomBytes(6).toString('hex');
const out = (ev) => process.stdout.write(JSON.stringify(ev) + '\n');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let input = '';
process.stdin.on('data', (c) => { input += c; });
process.stdin.on('end', async () => {
  out({ type: 'system', subtype: 'init', session_id: session, model });
  if (input.includes('SLOW')) await sleep(4000);
  if (input.includes('PCTOOL')) {
    const env = JSON.parse(fs.readFileSync(opt('--mcp-config'), 'utf8')).mcpServers.pc.env;
    const r = await fetch(env.MILLS_BRIDGE + '/internal/pc-tool', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': env.MILLS_INTERNAL, 'X-Mills-Account': env.MILLS_ACCOUNT },
      body: JSON.stringify({ name: 'list', args: { path: '' } }),
    }).then((x) => x.json()).catch((e) => ({ ok: false, error: e.message }));
    out({ type: 'assistant', session_id: session, message: { content: [{ type: 'text', text: 'PC:' + JSON.stringify(r) }] } });
  }
  // "BOARDTOOL": act on the team board through the board MCP relay, like Claude in a Work run:
  // create a task, claim it, add a progress note.
  if (input.includes('BOARDTOOL')) {
    const env = JSON.parse(fs.readFileSync(opt('--mcp-config'), 'utf8')).mcpServers.board.env;
    const tool = (name, args) => fetch(env.MILLS_BRIDGE + '/internal/board', {
      method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': env.MILLS_INTERNAL, 'X-Mills-Account': env.MILLS_ACCOUNT },
      body: JSON.stringify({ name, args }),
    }).then((x) => x.json()).catch((e) => ({ ok: false, error: e.message }));
    const made = await tool('task_create', { title: 'agent task r70', labels: ['agent'] });
    const id = made.ok ? made.result.task.id : '';
    const claimed = await tool('task_claim', { id, minutes: 5 });
    const noted = await tool('task_update', { id, appendNote: 'progress from the agent r70' });
    out({ type: 'assistant', session_id: session, message: { content: [{ type: 'text', text: 'BOARD:' + JSON.stringify({ id, made: made.ok, claimed: claimed.ok, noted: noted.ok }) }] } });
  }
  // "BOARDPLAN": plan with the board like Claude would: create with a text due date, read the calendar,
  // move a task, delete one, make a board, and set / clear a due date by text.
  if (input.includes('BOARDPLAN')) {
    const env = JSON.parse(fs.readFileSync(opt('--mcp-config'), 'utf8')).mcpServers.board.env;
    const tool = (name, args) => fetch(env.MILLS_BRIDGE + '/internal/board', {
      method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': env.MILLS_INTERNAL, 'X-Mills-Account': env.MILLS_ACCOUNT },
      body: JSON.stringify({ name, args }),
    }).then((x) => x.json()).catch((e) => ({ ok: false, error: e.message }));
    const made = await tool('task_create', { title: 'plan r116', due: 'tomorrow', priority: 2 });
    const id = made.ok ? made.result.task.id : '';
    const cal = await tool('board_calendar', { from: 'today', to: '+7d' });
    const moved = await tool('task_move', { id, status: 'review', index: 0 });
    const redue = await tool('task_update', { id, due: 'next friday' });
    const undue = await tool('task_update', { id, due: 'none' });
    const badDue = await tool('task_update', { id, due: 'someday maybe' });
    const nb = await tool('board_new', { name: 'Plan Board r116' });
    const gone = await tool('task_delete', { id });
    const after = await tool('task_get', { id });
    out({ type: 'assistant', session_id: session, message: { content: [{ type: 'text', text: 'PLAN:' + JSON.stringify({ id, dueAt: made.ok ? made.result.task.dueAt : null, calDays: cal.ok ? cal.result.days.map((d) => d.day + ':' + d.tasks.length) : cal.error, movedTo: moved.ok ? moved.result.task.status : moved.error, redue: redue.ok ? redue.result.task.dueAt : redue.error, undue: undue.ok ? redue.result && undue.result.task.dueAt : undue.error, badDue: badDue.ok ? 'accepted' : badDue.error, board: nb.ok ? nb.result.boards.map((b) => b.id) : nb.error, deleted: gone.ok, afterGet: after.ok ? 'still there' : after.error }) }] } });
  }
  // "NOTESTOOL": team notes through the team MCP relay: write, list with search, read, delete.
  if (input.includes('NOTESTOOL')) {
    const env = JSON.parse(fs.readFileSync(opt('--mcp-config'), 'utf8')).mcpServers.team.env;
    const tool = (name, args) => fetch(env.MILLS_BRIDGE + '/internal/team', {
      method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': env.MILLS_INTERNAL, 'X-Mills-Account': env.MILLS_ACCOUNT },
      body: JSON.stringify({ name, args }),
    }).then((x) => x.json()).catch((e) => ({ ok: false, error: e.message }));
    const w = await tool('notes_write', { path: 'plans/Claude Plan.md', text: '# Plan\n\nneedle-notes-r118 by claude' });
    const l = await tool('notes_list', { q: 'needle-notes-r118' });
    const r = await tool('notes_read', { path: 'plans/claude plan.md' });
    const bad = await tool('notes_write', { path: '../escape.md', text: 'x' });
    const d = await tool('notes_delete', { path: 'plans/Claude Plan.md' });
    out({ type: 'assistant', session_id: session, message: { content: [{ type: 'text', text: 'NOTES:' + JSON.stringify({ wrote: w.ok && w.result.by, listed: l.ok ? l.result.notes.map((n) => n.path) : l.error, read: r.ok ? r.result.text.includes('needle-notes-r118') : r.error, bad: bad.ok, deleted: d.ok }) }] } });
  }
  // "TEAMMORE": the wider team tools: a channel + a group made by Claude, the hub picture, past-session search, the AIs available.
  if (input.includes('TEAMMORE')) {
    const env = JSON.parse(fs.readFileSync(opt('--mcp-config'), 'utf8')).mcpServers.team.env;
    const tool = (name, args) => fetch(env.MILLS_BRIDGE + '/internal/team', {
      method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': env.MILLS_INTERNAL, 'X-Mills-Account': env.MILLS_ACCOUNT },
      body: JSON.stringify({ name, args }),
    }).then((x) => x.json()).catch((e) => ({ ok: false, error: e.message }));
    const ch = await tool('team_channel_create', { name: 'Claude Made', topic: 'r116' });
    const said = ch.ok ? await tool('team_say', { text: 'hello channel r116', conv: ch.result.conv }) : ch;
    const grp = await tool('team_group_create', { members: ['t-user'], name: 'claude group r116' });
    const st = await tool('hub_status', {});
    const se = await tool('sessions_search', { q: 'needle-r116' });
    const pv = await tool('my_providers', {});
    out({ type: 'assistant', session_id: session, message: { content: [{ type: 'text', text: 'MORE:' + JSON.stringify({ conv: ch.ok ? ch.result.conv : ch.error, said: said.ok, grp: grp.ok ? grp.result.group.members : grp.error, people: st.ok ? st.result.people.length : st.error, usage: st.ok && st.result.usage ? Object.keys(st.result.usage) : null, hits: se.ok ? se.result.items.length : se.error, providers: pv.ok ? pv.result.providers.map((p) => p.id) : pv.error }) }] } });
  }
  // "TEAMSAY": post into the team chat through the team MCP relay, like Claude in a Work run: one
  // message for everyone, one private to t-user, one to a user that does not exist (must fail).
  if (input.includes('TEAMSAY')) {
    const env = JSON.parse(fs.readFileSync(opt('--mcp-config'), 'utf8')).mcpServers.team.env;
    const tool = (name, args) => fetch(env.MILLS_BRIDGE + '/internal/team', {
      method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': env.MILLS_INTERNAL, 'X-Mills-Account': env.MILLS_ACCOUNT },
      body: JSON.stringify({ name, args }),
    }).then((x) => x.json()).catch((e) => ({ ok: false, error: e.message }));
    const all = await tool('team_say', { text: 'hello team from a Work run r108' });
    const dm = await tool('team_say', { text: 'private note r108', to: 't-user' });
    const bad = await tool('team_say', { text: 'x', to: 'nobody-here' });
    const who = await tool('team_presence', {});
    out({ type: 'assistant', session_id: session, message: { content: [{ type: 'text', text: 'TEAM:' + JSON.stringify({ all: all.ok && all.result.as, dm: dm.ok && dm.result.to, bad: bad.ok, people: who.ok ? who.result.people.length : -1 }) }] } });
  }
  // "LIMITPARTIAL": a 5h event with only a status (no utilization, no reset time), then a 7d event
  // reporting utilization as a percentage (55) — the hub must keep the 5h numbers and read 55 as 55 %.
  if (input.includes('LIMITPARTIAL')) {
    out({ type: 'rate_limit_event', session_id: session, rate_limit_info: { rateLimitType: 'five_hour', status: 'allowed' } });
    out({ type: 'rate_limit_event', session_id: session, rate_limit_info: { rateLimitType: 'seven_day', status: 'allowed', utilization: 55, resetsAt: Math.floor(Date.now() / 1000) + 86400 } });
  }
  // Run with a relayed login (ANTHROPIC_BASE_URL = the hub's /llm/<token>): make one model call through it, like the real
  // Claude Code does, report what came back, and turn the usage headers into a rate_limit_event (as the real one does).
  let relay = '';
  if (process.env.ANTHROPIC_BASE_URL) {
    try {
      const r = await fetch(process.env.ANTHROPIC_BASE_URL + '/v1/messages?beta=true', { method: 'POST', headers: { 'content-type': 'application/json', authorization: 'Bearer ' + (process.env.ANTHROPIC_AUTH_TOKEN || ''), 'anthropic-version': '2023-06-01', 'anthropic-beta': 'claude-code-20250219' }, body: JSON.stringify({ model, max_tokens: 50, messages: [{ role: 'user', content: input.slice(0, 80) }] }) });
      const j = await r.json().catch(() => ({}));
      relay = `${r.status}:${((j.content || [])[0] || {}).text || (j.error || {}).message || ''}`;
      const u = r.headers.get('anthropic-ratelimit-unified-5h-utilization');
      if (u) out({ type: 'rate_limit_event', session_id: session, rate_limit_info: { rateLimitType: 'five_hour', status: 'allowed', utilization: Number(u), resetsAt: Number(r.headers.get('anthropic-ratelimit-unified-5h-reset')) } });
    } catch (e) { relay = 'error:' + e.message; }
  }
  // "CAPTEST": the 5-hour window is 95 % used and resets in 3 s (an AI host's cap check must refuse the next run until then)
  if (input.includes('CAPTEST') && !process.env.ANTHROPIC_BASE_URL) out({ type: 'rate_limit_event', session_id: session, rate_limit_info: { rateLimitType: 'five_hour', status: 'allowed', utilization: 0.95, resetsAt: Math.floor(Date.now() / 1000) + 3 } });
  const usage = input.includes('USAGE');
  if (usage) out({ type: 'rate_limit_event', session_id: session, rate_limit_info: { rateLimitType: 'five_hour', status: 'allowed', utilization: 0.42, resetsAt: Math.floor(Date.now() / 1000) + 7200 } });
  if (input.includes('ASSISTTOOL')) {
    const env = JSON.parse(fs.readFileSync(opt('--mcp-config'), 'utf8')).mcpServers.assistant.env;
    const r = await fetch(env.MILLS_BRIDGE + '/internal/assistant-tool', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': env.MILLS_INTERNAL, 'X-Mills-Account': env.MILLS_ACCOUNT },
      body: JSON.stringify({ name: 'add_task', args: { title: 'from claude brain' } }),
    }).then((x) => x.json()).catch((e) => ({ ok: false, error: e.message }));
    out({ type: 'assistant', session_id: session, message: { content: [{ type: 'text', text: 'TOOL:' + JSON.stringify(r) }] } });
  }
  // cfg / key: which Claude login / API key this run was handed (a person's own provider), '' = the host's
  // wt: the allowed-tools entries that name worktree.js (a run in another project may call the hub's copy by its full path)
  out({ type: 'assistant', session_id: session, message: { model, content: [{ type: 'text', text: `echo: ${input} | model=${model} | cwd=${process.cwd()} | cfg=${process.env.CLAUDE_CONFIG_DIR || ''} | key=${(process.env.ANTHROPIC_API_KEY || '').slice(0, 12)} | fj=${process.env.FORGEJO_LOGIN || ''}:${process.env.FORGEJO_TOKEN ? 'tok' : ''}:${process.env['GIT_CONFIG_KEY_' + ((Number(process.env.GIT_CONFIG_COUNT) || 1) - 1)] || ''} | relay=${relay} | wt=${(opt('--allowedTools') || '').split(',').filter((t) => /worktree\.js/.test(t) && !/node bridge\/worktree\.js/.test(t)).join(';')}` }],
    ...(usage ? { usage: { input_tokens: 4000, cache_read_input_tokens: 45000, cache_creation_input_tokens: 5000, output_tokens: 300 } } : {}) } });
  // "FULLECHO" in the prompt: answer with the whole prompt (lets the runner see exactly what the room agent was given)
  out({ type: 'result', subtype: 'success', is_error: false, duration_ms: 1, session_id: session, result: input.includes('FULLECHO') ? 'echo: ' + input : `echo: ${input.slice(-200)} | model=${model}`,
    ...(usage ? { modelUsage: { [model]: { inputTokens: 4000, outputTokens: 300, contextWindow: 200000 } } } : {}) });
});
