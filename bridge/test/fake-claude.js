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
  // "LIMITPARTIAL": a 5h event with only a status (no utilization, no reset time), then a 7d event
  // reporting utilization as a percentage (55) — the hub must keep the 5h numbers and read 55 as 55 %.
  if (input.includes('LIMITPARTIAL')) {
    out({ type: 'rate_limit_event', session_id: session, rate_limit_info: { rateLimitType: 'five_hour', status: 'allowed' } });
    out({ type: 'rate_limit_event', session_id: session, rate_limit_info: { rateLimitType: 'seven_day', status: 'allowed', utilization: 55, resetsAt: Math.floor(Date.now() / 1000) + 86400 } });
  }
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
  out({ type: 'assistant', session_id: session, message: { model, content: [{ type: 'text', text: `echo: ${input} | model=${model} | cwd=${process.cwd()}` }],
    ...(usage ? { usage: { input_tokens: 4000, cache_read_input_tokens: 45000, cache_creation_input_tokens: 5000, output_tokens: 300 } } : {}) } });
  // "FULLECHO" in the prompt: answer with the whole prompt (lets the runner see exactly what the room agent was given)
  out({ type: 'result', subtype: 'success', is_error: false, duration_ms: 1, session_id: session, result: input.includes('FULLECHO') ? 'echo: ' + input : `echo: ${input.slice(-200)} | model=${model}`,
    ...(usage ? { modelUsage: { [model]: { inputTokens: 4000, outputTokens: 300, contextWindow: 200000 } } } : {}) });
});
