#!/usr/bin/env node
// MCP stdio server launched by Claude Code. Each tool call is relayed to the VibeRoller
// bridge, which forwards it to the user's browser; the browser performs it on the
// folder they opened on their PC and posts the result back.
'use strict';

const BRIDGE = process.env.MILLS_BRIDGE || 'http://127.0.0.1:7777';
const SECRET = process.env.MILLS_INTERNAL || '';
const USER = process.env.MILLS_USER || 'the user'; // display name of the person whose browser (and PC) this run is wired to
const ACCOUNT = process.env.MILLS_ACCOUNT || ''; // their account username: tells the hub which browser to relay to
const SESSION = process.env.MILLS_SESSION || ''; // the Work session of this run (not the first one): its page gets the call

const pathProp = { path: { type: 'string', description: `Path relative to the folder ${USER} opened. Use "" for the folder itself.` } };
const TOOLS = [
  { name: 'list', description: `List files and folders in a directory on ${USER}'s PC.`, inputSchema: { type: 'object', properties: pathProp } },
  { name: 'read', description: `Read a text file on ${USER}'s PC.`, inputSchema: { type: 'object', properties: pathProp, required: ['path'] } },
  { name: 'write', description: `Create or overwrite a text file on ${USER}'s PC (parent folders are created).`, inputSchema: { type: 'object', properties: { ...pathProp, content: { type: 'string' } }, required: ['path', 'content'] } },
  { name: 'delete', description: `Delete a file or folder (recursively) on ${USER}'s PC.`, inputSchema: { type: 'object', properties: pathProp, required: ['path'] } },
  { name: 'mkdir', description: `Create a folder on ${USER}'s PC.`, inputSchema: { type: 'object', properties: pathProp, required: ['path'] } },
  // T-311: the tools of a LINKED MACHINE (the native app on the person's computer). The Rust hub answers them; this frozen JavaScript
  // hub has no machines, so a call here says so. The names are listed because the Rust host and this server offer the same tools.
  { name: 'machine_info', description: `What ${USER}'s linked machine (the VibeRoller app on their computer) allows: its scope in words, the folder the file tools start in, which tools it offers, the shell's limits (advisory confinement, never elevated), whether a container engine is there. Call it before the machine tools; needs a linked machine.`, inputSchema: { type: 'object', properties: {} } },
  { name: 'shell_run', description: `Run one command on ${USER}'s linked machine and wait for it: \`cmd\` runs in \`cwd\` (a folder inside the machine's scope, default the opened folder), never as administrator / root. Returns exit, stdout, stderr (output is capped; \`truncated\` says so). The confinement is advisory: the working folder is confined, but a command can reach any path ${USER}'s account can, so keep to the folder; untrusted binaries belong in the RE containers (re_run). For anything long use job_start. Refused when the machine's scope has no shell.`, inputSchema: { type: 'object', properties: { cmd: { type: 'string', description: 'The command line.' }, cwd: { type: 'string', description: `Working folder, relative to the folder ${USER} opened (default: that folder).` }, timeoutSec: { type: 'integer', description: "Wall clock in seconds (never more than the machine's own limit)." } }, required: ['cmd'] } },
  { name: 'job_start', description: `Start a long command on ${USER}'s linked machine and return a jobId at once. The job keeps running on the machine if the page reloads or this run ends (kept 30 minutes after it finishes) and ends when the machine is unlinked or stopped. Read it with job_read.`, inputSchema: { type: 'object', properties: { cmd: { type: 'string' }, cwd: { type: 'string', description: `Working folder, relative to the folder ${USER} opened.` }, timeoutSec: { type: 'integer', description: 'Wall clock for the job (default one hour).' } }, required: ['cmd'] } },
  { name: 'job_read', description: `New output of a job on ${USER}'s linked machine since the byte offset \`since\` (use the \`next\` of the last answer), and whether it is still running or its exit code. With no jobId it lists the jobs on the machine, so a new run can find the ones an earlier run started.`, inputSchema: { type: 'object', properties: { jobId: { type: 'string' }, since: { type: 'integer' } } } },
  { name: 'job_stop', description: `Stop a job on ${USER}'s linked machine (its whole process tree).`, inputSchema: { type: 'object', properties: { jobId: { type: 'string' } }, required: ['jobId'] } },
  { name: 'proc_list', description: `List processes on ${USER}'s linked machine, marking the ones a tool there started (ownChild).`, inputSchema: { type: 'object', properties: {} } },
  { name: 'proc_kill', description: `Kill a process on ${USER}'s linked machine. With the usual scope only a process a tool there started (a job, a shell command) can be killed, never an unrelated one.`, inputSchema: { type: 'object', properties: { pid: { type: 'integer' } }, required: ['pid'] } },
  { name: 're_list', description: `The reverse-engineering containers on ${USER}'s linked machine (its own Podman / Docker, not the hub's re_* team tools). Only when the machine's scope allows containers and an engine is there.`, inputSchema: { type: 'object', properties: {} } },
  { name: 're_run', description: `Run a command in a throw-away RE container on ${USER}'s linked machine (it starts one if needed, within the machine's own limit of 3 plus a queue); the answer says the container id. Use it for untrusted binaries instead of shell_run.`, inputSchema: { type: 'object', properties: { cmd: { type: 'string' }, id: { type: 'string', description: 'An existing container of this machine; omit to start a new one.' }, purpose: { type: 'string' }, timeoutSec: { type: 'integer' } }, required: ['cmd'] } },
  { name: 're_logs', description: `Status and files of one RE container on ${USER}'s linked machine.`, inputSchema: { type: 'object', properties: { id: { type: 'string' } }, required: ['id'] } },
  { name: 're_stop', description: `Stop and delete one RE container on ${USER}'s linked machine (with its exchange folder).`, inputSchema: { type: 'object', properties: { id: { type: 'string' } }, required: ['id'] } },
  { name: 'transfer_get', description: `Move a big file FROM ${USER}'s linked machine to the hub's transfer area (the other tools carry text only). The hub issues a one-shot ticket (5 minutes, this account, this machine, this path) and the machine streams the file in chunks with a sha256 check; the answer names it (\`id\`, \`download\`). If a call ends with complete:false, call it again with the same path: it resumes. \`maxBytes\` limits one call.`, inputSchema: { type: 'object', properties: { path: { type: 'string', description: `The file, relative to the folder ${USER} opened.` }, maxBytes: { type: 'integer' } }, required: ['path'] } },
  { name: 'transfer_put', description: `Move a big file TO ${USER}'s linked machine: \`from\` is the id of a file ${USER} uploaded to the hub's transfer area (POST /api/transfers); it is pulled into \`path\` in chunks, resumed if a call stops early, verified by sha256 and only then put in place.`, inputSchema: { type: 'object', properties: { path: { type: 'string', description: `Where to put it, relative to the folder ${USER} opened.` }, from: { type: 'string' }, maxBytes: { type: 'integer' } }, required: ['path', 'from'] } },
];

function reply(id, result) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n'); }
function fail(id, code, message) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } }) + '\n'); }

async function callTool(name, args) {
  const res = await fetch(BRIDGE + '/internal/pc-tool', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': SECRET, 'X-Mills-Account': ACCOUNT },
    body: JSON.stringify({ name, args: args || {}, ...(SESSION ? { session: SESSION } : {}) }),
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
