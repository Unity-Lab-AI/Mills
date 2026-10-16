#!/usr/bin/env node
// MCP stdio server for the Unity section when its brain is Claude: lists the Unity section tools and relays
// every call to the hub (/internal/assistant-tool), which runs it on the signed-in person's own data.
'use strict';

const BRIDGE = process.env.MILLS_BRIDGE || 'http://127.0.0.1:7777';
const SECRET = process.env.MILLS_INTERNAL || '';
const ACCOUNT = process.env.MILLS_ACCOUNT || '';

const hub = (body) => fetch(BRIDGE + '/internal/assistant-tool', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': SECRET, 'X-Mills-Account': ACCOUNT },
  body: JSON.stringify(body),
}).then((r) => r.json());

const reply = (id, result) => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n');
const fail = (id, code, message) => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } }) + '\n');

async function handle(msg) {
  const { id, method, params } = msg;
  if (id === undefined) return;
  if (method === 'initialize') return reply(id, { protocolVersion: (params && params.protocolVersion) || '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'mills-assistant', version: '1.0.0' } });
  if (method === 'ping') return reply(id, {});
  if (method === 'tools/list') {
    try { return reply(id, { tools: (await hub({ list: true })).tools || [] }); } catch (e) { return fail(id, -32000, 'Hub unreachable: ' + e.message); }
  }
  if (method === 'tools/call') {
    try {
      const out = await hub({ name: params.name, args: params.arguments || {} });
      const r = out.result || out;
      return reply(id, { content: [{ type: 'text', text: JSON.stringify(r, null, 2) }], isError: !out.ok || r.ok === false });
    } catch (e) { return reply(id, { content: [{ type: 'text', text: 'Hub unreachable: ' + e.message }], isError: true }); }
  }
  return fail(id, -32601, 'Method not found: ' + method);
}

let buf = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  buf += chunk; let i;
  while ((i = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
    if (line) { try { handle(JSON.parse(line)); } catch {} }
  }
});
