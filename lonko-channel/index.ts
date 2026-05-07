#!/usr/bin/env bun
// lonko-channel: Claude Code channel plugin that bridges chat between
// the lonko TUI daemon and the running Claude Code session.
//
// On startup the plugin opens a persistent connection to the lonko
// daemon's chat Unix socket (`~/.claude/lonko-chat.sock`, separate
// from the hook socket so chat traffic stays full-duplex) and
// announces itself with a `chat.online` frame. The lonko daemon
// correlates this connection to a session by PPID (the PID of the
// parent Claude Code process). Frames are JSON-lines: one JSON
// object per line, terminated by `\n`.
//
// Override the socket location with `LONKO_CHAT_SOCKET=/tmp/...` for
// integration tests that bind the listener in a tempdir.
//
// Inbound (from daemon → plugin):
//   {kind: "chat.send", msg_id: string, text: string}
//
// Outbound (from plugin → daemon):
//   {kind: "chat.online", ppid: number, pid: number}
//   {kind: "chat.reply", in_reply_to: string, text: string, agent_id: string}
//   {kind: "chat.ack",   msg_id: string, status: "delivered" | "agent_not_found"}
//
// `agent_id` for v1 is the PPID as a string. The daemon already keys
// agents by their Claude Code PID, so this matches without further
// handshake. v2 will extend this to (host, agent_id) once the tailnet
// transport lands.

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  ListToolsRequestSchema,
  CallToolRequestSchema,
} from '@modelcontextprotocol/sdk/types.js';
import { createConnection, type Socket } from 'node:net';
import { homedir } from 'node:os';
import { join } from 'node:path';

const SOCKET_PATH =
  process.env.LONKO_CHAT_SOCKET ?? join(homedir(), '.claude', 'lonko-chat.sock');
const PPID = process.ppid;
const PID = process.pid;
const AGENT_ID = String(PPID);

type Frame =
  | { kind: 'chat.send'; msg_id: string; text: string }
  | { kind: 'chat.online'; ppid: number; pid: number }
  | { kind: 'chat.reply'; in_reply_to: string; text: string; agent_id: string }
  | { kind: 'chat.ack'; msg_id: string; status: 'delivered' | 'agent_not_found' };

const mcp = new Server(
  { name: 'lonko', version: '0.1.0' },
  {
    capabilities: {
      experimental: { 'claude/channel': {} },
      tools: {},
    },
    instructions:
      'Messages from the lonko TUI arrive as <channel source="lonko" agent_id="...">. ' +
      'Reply with the `reply` tool, passing back the agent_id from the tag and the text of your response.',
  },
);

mcp.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: [
    {
      name: 'reply',
      description: 'Send a reply back to the lonko TUI for this agent.',
      inputSchema: {
        type: 'object',
        properties: {
          agent_id: { type: 'string', description: 'agent_id from the inbound <channel> tag' },
          text: { type: 'string', description: 'reply text' },
          in_reply_to: { type: 'string', description: 'msg_id of the inbound message being replied to' },
        },
        required: ['agent_id', 'text'],
      },
    },
  ],
}));

let sock: Socket | null = null;
let pending = '';

function writeFrame(frame: Frame): void {
  if (!sock || sock.destroyed) return;
  sock.write(JSON.stringify(frame) + '\n');
}

function handleInbound(line: string): void {
  if (!line.trim()) return;
  let frame: Frame;
  try {
    frame = JSON.parse(line);
  } catch {
    return;
  }
  if (frame.kind !== 'chat.send') return;

  void mcp
    .notification({
      method: 'notifications/claude/channel',
      params: {
        content: frame.text,
        meta: {
          agent_id: AGENT_ID,
          msg_id: frame.msg_id,
        },
      },
    })
    .then(() => {
      writeFrame({ kind: 'chat.ack', msg_id: frame.msg_id, status: 'delivered' });
    })
    .catch(() => {
      writeFrame({ kind: 'chat.ack', msg_id: frame.msg_id, status: 'agent_not_found' });
    });
}

const DEBUG = process.env.LONKO_CHANNEL_DEBUG === '1';
function debug(msg: string): void {
  if (DEBUG) process.stderr.write(`[lonko-channel] ${msg}\n`);
}

function connectDaemon(): void {
  debug(`connecting to ${SOCKET_PATH}`);
  sock = createConnection(SOCKET_PATH);

  sock.on('connect', () => {
    debug(`connected, announcing chat.online ppid=${PPID}`);
    writeFrame({ kind: 'chat.online', ppid: PPID, pid: PID });
  });

  sock.on('data', (chunk: Buffer) => {
    pending += chunk.toString('utf8');
    let nl: number;
    while ((nl = pending.indexOf('\n')) !== -1) {
      const line = pending.slice(0, nl);
      pending = pending.slice(nl + 1);
      handleInbound(line);
    }
  });

  sock.on('error', (err: Error) => {
    debug(`socket error: ${err.message}`);
  });

  sock.on('close', () => {
    sock = null;
    pending = '';
    setTimeout(connectDaemon, 1000);
  });
}

mcp.setRequestHandler(CallToolRequestSchema, async (req) => {
  if (req.params.name !== 'reply') {
    throw new Error(`unknown tool: ${req.params.name}`);
  }
  const args = req.params.arguments as { agent_id: string; text: string; in_reply_to?: string };
  writeFrame({
    kind: 'chat.reply',
    in_reply_to: args.in_reply_to ?? '',
    text: args.text,
    agent_id: args.agent_id,
  });
  return { content: [{ type: 'text', text: 'sent' }] };
});

// Order matters: connect to the lonko daemon FIRST so `chat.online` is
// announced regardless of whether a real MCP host is talking to us on
// stdio. `mcp.connect()` blocks until the host sends `initialize`, so
// putting it first would gate socket connection behind the handshake.
connectDaemon();
await mcp.connect(new StdioServerTransport());
