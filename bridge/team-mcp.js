#!/usr/bin/env node
// MCP stdio server launched by Claude Code in a Work run: the team itself. Claude can post into the
// end-to-end encrypted team chat (shown to everyone as "<person> · Claude", or privately to one
// person) and see who is online. Every call is relayed to the hub (/internal/team), which signs the
// message with the room agent's key so every member can read it.
'use strict';

const BRIDGE = process.env.MILLS_BRIDGE || 'http://127.0.0.1:7777';
const SECRET = process.env.MILLS_INTERNAL || '';
const USER = process.env.MILLS_USER || 'the user';
const ACCOUNT = process.env.MILLS_ACCOUNT || '';

const TOOLS = [
  { name: 'team_say', description: `Post a message into the Mills team chat as "${USER} · Claude": to # Everyone (default), privately to one person (to = their username), or into a server channel / group (conv = its id from team_channels, e.g. "ch:mills/dev" or "grp:ab12cd"). Use it to tell the team what you are doing, what landed, and BEFORE any hub restart. Markdown is rendered.`,
    inputSchema: { type: 'object', properties: { text: { type: 'string', description: 'The message (markdown)' }, to: { type: 'string', description: 'Username for a private message; leave out for everyone' }, conv: { type: 'string', description: 'A channel or group conversation id (see team_channels)' } }, required: ['text'] } },
  { name: 'team_presence', description: 'Who is in Mills right now: every account with online / away / offline, where they are and when they were last seen.', inputSchema: { type: 'object', properties: {} } },
  { name: 'team_channels', description: `The servers, channels and groups ${USER} is in, with the conversation ids team_say accepts.`, inputSchema: { type: 'object', properties: {} } },
];

function reply(rid, result) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: rid, result }) + '\n'); }
function fail(rid, code, message) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: rid, error: { code, message } }) + '\n'); }

async function callTool(name, args) {
  const res = await fetch(BRIDGE + '/internal/team', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': SECRET, 'X-Mills-Account': ACCOUNT },
    body: JSON.stringify({ name, args: args || {} }),
  });
  return res.json();
}

async function handle(msg) {
  const { id: rid, method, params } = msg;
  if (rid === undefined) return;
  if (method === 'initialize') return reply(rid, { protocolVersion: (params && params.protocolVersion) || '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'mills-team', version: '1.0.0' } });
  if (method === 'ping') return reply(rid, {});
  if (method === 'tools/list') return reply(rid, { tools: TOOLS });
  if (method === 'tools/call') {
    try {
      const out = await callTool(params.name, params.arguments);
      return reply(rid, { content: [{ type: 'text', text: out.ok ? JSON.stringify(out.result, null, 2) : 'Error: ' + out.error }], isError: !out.ok });
    } catch (e) {
      return reply(rid, { content: [{ type: 'text', text: 'Hub unreachable: ' + e.message }], isError: true });
    }
  }
  return fail(rid, -32601, 'Method not found: ' + method);
}

let buf = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  buf += chunk;
  let i;
  while ((i = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
    if (line) { try { handle(JSON.parse(line)); } catch {} }
  }
});
