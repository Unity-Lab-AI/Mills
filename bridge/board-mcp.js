#!/usr/bin/env node
// MCP stdio server launched by Claude Code in a Work run: the Mills team board. Claude acts on the
// board as "<person> · Claude" (the person whose Work run this is). Before working a task, claim it;
// renew the claim while working; release it (or complete the task) when done. A task someone else
// holds cannot be edited until they release it or the claim lapses.
'use strict';

const BRIDGE = process.env.MILLS_BRIDGE || 'http://127.0.0.1:7777';
const SECRET = process.env.MILLS_INTERNAL || '';
const USER = process.env.MILLS_USER || 'the user';
const ACCOUNT = process.env.MILLS_ACCOUNT || '';

const id = { id: { type: 'string', description: 'Task id, e.g. "T-12".' } };
const TOOLS = [
  { name: 'board_list', description: `List the Mills team board's tasks (no descriptions). Filter by board, status (backlog/todo/doing/review/done), or mine=true for tasks assigned to ${USER} or claimed by you.`,
    inputSchema: { type: 'object', properties: { board: { type: 'string' }, status: { type: 'string' }, mine: { type: 'boolean' } } } },
  { name: 'task_get', description: 'Read one task with its markdown description and recent activity.', inputSchema: { type: 'object', properties: id, required: ['id'] } },
  { name: 'task_create', description: 'Create a task on the team board (a task with a due date is a calendar entry).', inputSchema: { type: 'object', properties: { title: { type: 'string' }, body: { type: 'string', description: 'Markdown description' }, status: { type: 'string' }, board: { type: 'string' }, labels: { type: 'array', items: { type: 'string' } }, priority: { type: 'integer', minimum: 0, maximum: 5 }, due: { type: 'string', description: 'Due date as text: YYYY-MM-DD, today, tomorrow, +3d, next friday' }, assignee: { type: 'string', description: 'username' }, recur: { type: 'string', description: 'daily, weekly, biweekly, monthly, quarterly, half_year, yearly' } }, required: ['title'] } },
  { name: 'task_claim', description: 'Claim a task before working on it (time-limited, default 15 minutes, max 240). Moves a to-do task to "doing". Refused if someone else holds it.',
    inputSchema: { type: 'object', properties: { ...id, minutes: { type: 'integer' } }, required: ['id'] } },
  { name: 'task_renew', description: 'Extend your claim while still working (same as claiming again).', inputSchema: { type: 'object', properties: { ...id, minutes: { type: 'integer' } }, required: ['id'] } },
  { name: 'task_update', description: 'Change a task: status (backlog/todo/doing/review/done), title, body, appendNote (adds a signed progress note to the description), labels, priority 0-5, due (text: YYYY-MM-DD, today, tomorrow, +3d, next friday, none) or dueAt (ms), recur, assignee (username, "" to unassign). Refused while someone else holds a claim.',
    inputSchema: { type: 'object', properties: { ...id, status: { type: 'string' }, title: { type: 'string' }, body: { type: 'string' }, appendNote: { type: 'string' }, labels: { type: 'array', items: { type: 'string' } }, priority: { type: 'integer' }, due: { type: 'string' }, dueAt: { type: 'number' }, recur: { type: 'string' }, assignee: { type: 'string' } }, required: ['id'] } },
  { name: 'task_complete', description: 'Mark a task done (a repeating task re-arms to its next due date). Releases your claim.', inputSchema: { type: 'object', properties: id, required: ['id'] } },
  { name: 'task_release', description: 'Release your claim without completing (optionally with a note why).', inputSchema: { type: 'object', properties: { ...id, note: { type: 'string' } }, required: ['id'] } },
  { name: 'task_move', description: 'Move a task to a column (backlog/todo/doing/review/done) and position (index from the top, 0 = first). Moving to done completes it.', inputSchema: { type: 'object', properties: { ...id, status: { type: 'string' }, index: { type: 'integer' } }, required: ['id', 'status'] } },
  { name: 'task_delete', description: 'Delete a task (leaves a tombstone in the board file; refused while someone else holds it). Prefer completing or moving to backlog unless the person asked to delete.', inputSchema: { type: 'object', properties: id, required: ['id'] } },
  { name: 'board_new', description: 'Make another board (e.g. per project).', inputSchema: { type: 'object', properties: { name: { type: 'string' } }, required: ['name'] } },
  { name: 'board_calendar', description: 'The team calendar: tasks with a due date grouped by day for a range (default today → +14 days), plus everything overdue. Use it to plan: then task_create / task_update with due to put things on days.', inputSchema: { type: 'object', properties: { from: { type: 'string', description: 'YYYY-MM-DD, today, +Nd …' }, to: { type: 'string' }, board: { type: 'string' } } } },
];

function reply(rid, result) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: rid, result }) + '\n'); }
function fail(rid, code, message) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: rid, error: { code, message } }) + '\n'); }

async function callTool(name, args) {
  const res = await fetch(BRIDGE + '/internal/board', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': SECRET, 'X-Mills-Account': ACCOUNT },
    body: JSON.stringify({ name, args: args || {} }),
  });
  return res.json();
}

async function handle(msg) {
  const { id: rid, method, params } = msg;
  if (rid === undefined) return;
  if (method === 'initialize') return reply(rid, { protocolVersion: (params && params.protocolVersion) || '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'mills-board', version: '1.0.0' } });
  if (method === 'ping') return reply(rid, {});
  if (method === 'tools/list') return reply(rid, { tools: TOOLS });
  if (method === 'tools/call') {
    try {
      const out = await callTool(params.name, params.arguments);
      return reply(rid, { content: [{ type: 'text', text: out.ok ? JSON.stringify(out.result, null, 2) : 'Error: ' + out.error }], isError: !out.ok });
    } catch (e) {
      return reply(rid, { content: [{ type: 'text', text: 'Board unreachable: ' + e.message }], isError: true });
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
