#!/usr/bin/env node
// MCP stdio server launched by Claude Code. Each tool call is relayed to the VibeRoller
// bridge, which forwards it to the user's browser; the browser performs it on the
// folder they opened on their PC and posts the result back.
'use strict';

const BRIDGE = process.env.MILLS_BRIDGE || 'http://127.0.0.1:7777';
const SECRET = process.env.MILLS_INTERNAL || '';
const USER = process.env.MILLS_USER || 'the user'; // display name of the person whose browser (and PC) this run is wired to
const ACCOUNT = process.env.MILLS_ACCOUNT || ''; // their account username: tells the hub which browser to relay to

const pathProp = { path: { type: 'string', description: `Path relative to the folder ${USER} opened. Use "" for the folder itself.` } };
const TOOLS = [
  { name: 'list', description: `List files and folders in a directory on ${USER}'s PC.`, inputSchema: { type: 'object', properties: pathProp } },
  { name: 'read', description: `Read a text file on ${USER}'s PC.`, inputSchema: { type: 'object', properties: pathProp, required: ['path'] } },
  { name: 'write', description: `Create or overwrite a text file on ${USER}'s PC (parent folders are created).`, inputSchema: { type: 'object', properties: { ...pathProp, content: { type: 'string' } }, required: ['path', 'content'] } },
  { name: 'delete', description: `Delete a file or folder (recursively) on ${USER}'s PC.`, inputSchema: { type: 'object', properties: pathProp, required: ['path'] } },
  { name: 'mkdir', description: `Create a folder on ${USER}'s PC.`, inputSchema: { type: 'object', properties: pathProp, required: ['path'] } },
];

function reply(id, result) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n'); }
function fail(id, code, message) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } }) + '\n'); }

async function callTool(name, args) {
  const res = await fetch(BRIDGE + '/internal/pc-tool', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': SECRET, 'X-Mills-Account': ACCOUNT },
    body: JSON.stringify({ name, args: args || {} }),
  });
  return res.json();
}

async function handle(msg) {
  const { id, method, params } = msg;
  if (id === undefined) return; // notification
  if (method === 'initialize') {
    return reply(id, {
      protocolVersion: (params && params.protocolVersion) || '2025-06-18',
      capabilities: { tools: {} },
      serverInfo: { name: 'mills-pc', version: '1.0.0' },
    });
  }
  if (method === 'ping') return reply(id, {});
  if (method === 'tools/list') return reply(id, { tools: TOOLS });
  if (method === 'tools/call') {
    try {
      const out = await callTool(params.name, params.arguments);
      const text = out.ok ? (typeof out.result === 'string' ? out.result : JSON.stringify(out.result, null, 2)) : 'Error: ' + out.error;
      return reply(id, { content: [{ type: 'text', text }], isError: !out.ok });
    } catch (e) {
      return reply(id, { content: [{ type: 'text', text: 'Bridge unreachable: ' + e.message }], isError: true });
    }
  }
  return fail(id, -32601, 'Method not found: ' + method);
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
