#!/usr/bin/env node
// Stand-in for `jcodepgm run --ndjson` used ONLY by the regression runner's throwaway hub. Speaks the NDJSON events
// jcode's one-shot run emits (start, text_delta, tool_start/tool_exec/tool_done, tokens, done / error), so the hub's
// jcode harness is exercised end to end without a jcode build. Costs nothing, touches nothing.
//   message contains "SLOW"   → waits 4 s before answering
//   message contains "FAIL"   → ends with an error event and exit code 1
//   message contains "TOOL"   → runs one fake tool (bash) before answering
//   message contains "MCP"    → reads $JCODE_HOME/mcp.json and calls the hub's pc "list" tool like jcode's MCP client would
// The answer echoes the model, the provider, the cwd and whether the prompt overlay was present, so the runner can
// check what the hub handed over.
'use strict';
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const argv = process.argv.slice(2);
const opt = (n, short) => { let i = argv.indexOf(n); if (i < 0 && short) i = argv.indexOf(short); return i >= 0 ? argv[i + 1] : ''; };
const model = opt('--model', '-m') || 'default';
const provider = opt('--provider', '-p') || 'auto';
const resume = opt('--resume');
const session = resume || 'fakej-' + crypto.randomBytes(6).toString('hex');
const message = argv.filter((a, i) => !a.startsWith('-') && !['--model', '-m', '--provider', '-p', '--resume', '--socket', '--tools', '--tool-profile'].includes(argv[i - 1]) && a !== 'run').pop() || '';
const out = (ev) => process.stdout.write(JSON.stringify(ev) + '\n');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const home = process.env.JCODE_HOME || '';

(async () => {
  out({ type: 'start', session_id: session, provider, model });
  if (message.includes('SLOW')) await sleep(4000);
  let extra = '';
  if (message.includes('MCP') && home) {
    try {
      const env = JSON.parse(fs.readFileSync(path.join(home, 'mcp.json'), 'utf8')).mcpServers.pc.env;
      const r = await fetch(env.MILLS_BRIDGE + '/internal/pc-tool', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': env.MILLS_INTERNAL, 'X-Mills-Account': env.MILLS_ACCOUNT },
        body: JSON.stringify({ name: 'list', args: { path: '' } }),
      }).then((x) => x.json()).catch((e) => ({ ok: false, error: e.message }));
      extra += ' MCP:' + JSON.stringify(r);
    } catch (e) { extra += ' MCP:' + JSON.stringify({ ok: false, error: e.message }); }
  }
  if (message.includes('TOOL')) {
    out({ type: 'tool_start', id: 't1', name: 'bash' });
    out({ type: 'tool_input', delta: '{"command":"echo hi"}' });
    out({ type: 'tool_exec', id: 't1', name: 'bash' });
    out({ type: 'tool_done', id: 't1', name: 'bash', output: 'hi\n', error: null });
  }
  if (message.includes('FAIL')) {
    out({ type: 'error', session_id: session, provider, model, message: 'fake failure' });
    process.exit(1);
  }
  const overlay = home && fs.existsSync(path.join(home, 'prompt-overlay.md')) ? 'overlay:yes' : 'overlay:no';
  const text = `jcode-answer model=${model} provider=${provider} cwd=${process.cwd()} resume=${resume ? 'yes' : 'no'} ${overlay}${extra}`;
  out({ type: 'text_delta', text });
  out({ type: 'tokens', input: 120, output: 30, cache_read_input: 0, cache_creation_input: 0 });
  out({ type: 'done', session_id: session, provider, model, text, usage: { input: 120, output: 30 } });
})();
