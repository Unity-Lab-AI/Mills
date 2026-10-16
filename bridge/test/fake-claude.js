#!/usr/bin/env node
// Stand-in for `claude -p --output-format stream-json` used ONLY by the regression runner's
// throwaway hub. Speaks the same NDJSON events the web app reads, costs nothing, touches nothing.
//   message contains "SLOW"   → waits 4 s before answering (busy / 409 / stop / deferred restart)
//   message contains "PCTOOL" → calls the pc "list" tool through the hub, like the real MCP relay
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
  out({ type: 'assistant', session_id: session, message: { content: [{ type: 'text', text: `echo: ${input} | model=${model} | cwd=${process.cwd()}` }] } });
  out({ type: 'result', subtype: 'success', is_error: false, duration_ms: 1, session_id: session, result: `echo: ${input.slice(-200)} | model=${model}` });
});
