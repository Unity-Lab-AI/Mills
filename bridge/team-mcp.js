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

const SESSION = process.env.MILLS_SESSION || ''; // the Work session of this run (not the first one): its page gets the question

// ask_user (T-245; team note plans/question-protocol.md): the person answers on their page; the answers come back as this tool's result.
const OPTION = { anyOf: [{ type: 'string' }, { type: 'object', properties: { id: { type: 'string' }, label: { type: 'string' }, hint: { type: 'string', description: 'A short explanation shown under the label' } }, required: ['label'] }] };
const ASK_USER = { name: 'ask_user', description: `Ask ${USER} one question or a series of questions and wait for the answers (a choice, several choices, a checkbox list, or free text). Use it when a decision is theirs and guessing would waste work: an owner, a scope, a name, which of several designs. Do NOT ask what you can find out yourself or what a rule already settles. Each question has a kind: "single" (pick one), "multi" (pick several), "checklist" (a long list to tick, for features or items) or "text" (their own words); single and multi also offer "my own answer" by default. Give each question the answer you recommend as "default": if ${USER} does not answer within the timeout (10 minutes unless timeoutSec says otherwise; 0 = wait until answered), the defaults are used and the result says so (by: "default", timedOut: true). Several questions are shown one after the other (Next, then Done) and come back together. The result is {answered, by: "user" | "default" | "mixed", answers: [{id, question, kind, picked: [option ids], labels, other?, text?, by}]}.`,
  inputSchema: { type: 'object', properties: {
    title: { type: 'string', description: 'A short heading for the series' },
    questions: { type: 'array', description: 'The questions, in the order to ask them (at most 30)', items: { type: 'object', properties: {
      id: { type: 'string', description: 'A short name to find the answer by (default q1, q2, ...)' },
      text: { type: 'string', description: 'The question' },
      kind: { type: 'string', enum: ['single', 'multi', 'checklist', 'text'], description: 'Default: single when there are options, else text' },
      options: { type: 'array', items: OPTION, description: 'The choices: strings, or {id, label, hint}' },
      other: { description: 'true / false / a label: offer "my own answer" (default: on for single and multi, off for checklist)', anyOf: [{ type: 'boolean' }, { type: 'string' }] },
      default: { description: 'Your recommended answer: an option id (single), a list of option ids (multi, checklist), or the text (text)', anyOf: [{ type: 'string' }, { type: 'array', items: { type: 'string' } }] },
      required: { type: 'boolean' } }, required: ['text'] } },
    timeoutSec: { type: 'number', description: 'Seconds to wait before the defaults are used (default 600; 0 = until answered)' } }, required: ['questions'] } };

// plan_draft (T-279; plans/planner-mode.md § 6): a planner session shows its whole plan as a draft and waits; the person approves all / some, asks for a change or rejects; the HUB writes it (source note, parents, sub-tasks, changes) and the result names the new task ids.
const PLAN_DRAFT = { name: 'plan_draft', description: `Show ${USER} the whole plan as a draft and wait for the answer. In a planner session this is the ONLY way tasks reach the board: task_create / task_update / task_move / task_delete are refused. ${USER} approves all, approves some parents, asks for a change (you get their text back: revise and call plan_draft again) or rejects. On approval the hub writes, in this order, the source note (sourceText word for word under plans/), the parents, their sub-tasks (named "T-<parent> · n/m: ..." with the parent id as a label), the changes to existing tasks, then posts the plan summary to the team chat; the result lists the new task ids and what each owner got. Every parent needs an owner (a username).`,
  inputSchema: { type: 'object', properties: {
    title: { type: 'string', description: 'The plan\'s topic (names the notes plans/<topic>-original.md and plans/<topic>-plan.md)' },
    parents: { type: 'array', description: 'One per idea', items: { type: 'object', properties: {
      title: { type: 'string' }, body: { type: 'string', description: 'Markdown: what, must, already there, keep in mind, done when' }, owner: { type: 'string', description: 'username of who gets it' },
      labels: { type: 'array', items: { type: 'string' } }, priority: { type: 'number', description: '0 most urgent .. 5 someday (default 2)' }, status: { type: 'string', enum: ['todo', 'backlog'] },
      blockedOn: { type: 'string', description: 'The title (or T-id) of what it is based on; makes it backlog + the label blocked' },
      subtasks: { type: 'array', items: { type: 'object', properties: { title: { type: 'string' }, body: { type: 'string' }, owner: { type: 'string' }, labels: { type: 'array', items: { type: 'string' } }, priority: { type: 'number' } }, required: ['title'] } } }, required: ['title', 'owner'] } },
    existingChanges: { type: 'array', description: 'Changes to tasks that already exist (never delete or complete): applied only when everything is approved, or with their parent (parent = index)', items: { type: 'object', properties: { id: { type: 'string', description: 'T-12' }, change: { description: 'An object of fields to update (title, body, appendNote, labels, priority, status, assignee, due) or a note text', anyOf: [{ type: 'object' }, { type: 'string' }] }, reason: { type: 'string' }, parent: { type: 'number', description: 'index (from 0) of the parent it goes with' } }, required: ['id', 'change'] } },
    notPlanned: { type: 'array', description: 'What of the input is not planned, and why (nothing is dropped silently)', items: { type: 'object', properties: { text: { type: 'string' }, why: { type: 'string' } }, required: ['text'] } },
    sourceText: { type: 'string', description: 'The person\'s input, word for word (kept in a team note)' },
    sourceNote: { type: 'string', description: 'A plans/ note to append it to (with the date) instead of plans/<topic>-original.md' } }, required: ['parents'] } };

const TOOLS = [
  ASK_USER,
  PLAN_DRAFT,
  { name: 'team_say', description: `Post a message into the VibeRoller team chat as "${USER} · Claude": to # Everyone (default), privately to one person (to = their username), or into a server channel / group (conv = its id from team_channels, e.g. "ch:mills/dev" or "grp:ab12cd"). Use it to tell the team what you are doing, what landed, and BEFORE any hub restart. Markdown is rendered.`,
    inputSchema: { type: 'object', properties: { text: { type: 'string', description: 'The message (markdown)' }, to: { type: 'string', description: 'Username for a private message; leave out for everyone' }, conv: { type: 'string', description: 'A channel or group conversation id (see team_channels)' } }, required: ['text'] } },
  { name: 'team_presence', description: 'Who is in VibeRoller right now: every account with online / away / offline, where they are and when they were last seen. Each person also has presence, label (Working + available / Working / Away / Available for work / Online / Offline), working (+ workingSessions), acceptingWork, dnd, text, since and where (project, branch, task), as far as they let the team see it.', inputSchema: { type: 'object', properties: {} } },
  { name: 'notify_when_available', description: 'Tell me when a person is available: your Claude is told in a private chat message as soon as their status matches (what: "available" = accepting work and not offline, "back" = online again after being away / offline, "online" = online or away, "free" = not working). Remembered up to 24 hours, 5 at most; if it already holds you are told at once. Appear offline hides the change.', inputSchema: { type: 'object', properties: { user: { type: 'string', description: 'username' }, what: { type: 'string', enum: ['available', 'back', 'online', 'free'], description: 'default "available"' }, message: { type: 'string', description: 'a note to add when it is told (max 200)' } }, required: ['user'] } },
  { name: 'notify_list', description: 'The "tell me when" requests that are waiting (notify_when_available), with their ids.', inputSchema: { type: 'object', properties: {} } },
  { name: 'notify_cancel', description: 'Cancel a waiting "tell me when" request: by id (from notify_list), by user, or all: true.', inputSchema: { type: 'object', properties: { id: { type: 'string' }, user: { type: 'string' }, all: { type: 'boolean' } } } },
  { name: 'team_channels', description: `The servers, channels and groups ${USER} is in, with the conversation ids team_say accepts.`, inputSchema: { type: 'object', properties: {} } },
  { name: 'team_channel_create', description: `Make a text channel in a server ${USER} is in (default: the VibeRoller server everyone is in). Returns its conv id for team_say.`, inputSchema: { type: 'object', properties: { name: { type: 'string', description: 'letters, digits, dashes' }, topic: { type: 'string' }, server: { type: 'string', description: 'server id, default "mills"' } }, required: ['name'] } },
  { name: 'team_group_create', description: `Make a group conversation with the people named (${USER} is included). Returns its conv id for team_say.`, inputSchema: { type: 'object', properties: { members: { type: 'array', items: { type: 'string' }, description: 'usernames' }, name: { type: 'string' } }, required: ['members'] } },
  { name: 'hub_status', description: 'The wider picture of VibeRoller right now: who is online / away / offline and where, who is in a call (cam / mic / screen), who is sharing a Work session, and the host account\'s 5-hour / 7-day Claude usage windows plus this session\'s context size.', inputSchema: { type: 'object', properties: {} } },
  { name: 'sessions_search', description: `Search ${USER}'s own saved Claude sessions (Past sessions): every word must match the title or the transcript; snippets say who said it.`, inputSchema: { type: 'object', properties: { q: { type: 'string' } }, required: ['q'] } },
  { name: 'my_providers', description: `The AIs ${USER} may use in VibeRoller (the host's Claude, their own keys / logins / endpoints, the team's shared ones) and which one answers right now.`, inputSchema: { type: 'object', properties: {} } },
  // Team notes: markdown files in folders on the host, shared by everyone (and visible in the Notes screen).
  { name: 'notes_list', description: 'List the team notes (paths like "plans/next-week.md", who saved them last, when); with q, only notes whose path or text contains every word, with a snippet.', inputSchema: { type: 'object', properties: { q: { type: 'string' } } } },
  { name: 'notes_read', description: 'Read a team note (markdown).', inputSchema: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] } },
  { name: 'notes_write', description: `Create or replace a team note with the whole text (saved as "${USER} · Claude"). Folders are made as needed; names are case-insensitive.`, inputSchema: { type: 'object', properties: { path: { type: 'string', description: 'e.g. "meetings/2026-10-17.md"' }, text: { type: 'string' } }, required: ['path', 'text'] } },
  { name: 'notes_delete', description: 'Delete a team note (or an empty folder). Only when the person asked.', inputSchema: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] } },
];

function reply(rid, result) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: rid, result }) + '\n'); }
function fail(rid, code, message) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: rid, error: { code, message } }) + '\n'); }

async function callTool(name, args) {
  const once = async (a) => (await fetch(BRIDGE + '/internal/team', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Mills-Internal': SECRET, 'X-Mills-Account': ACCOUNT },
    body: JSON.stringify({ name, args: a || {}, ...(SESSION ? { session: SESSION } : {}) }),
  })).json();
  let out = await once(args);
  // a question may wait longer than one HTTP call is held: the hub answers {pending, id} every few minutes, we ask again
  if (name === 'ask_user' || name === 'plan_draft') while (out.ok && out.result && out.result.pending && out.result.id) out = await once({ poll: out.result.id, ...(args && args.pollSec ? { pollSec: args.pollSec } : {}) });
  return out;
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
