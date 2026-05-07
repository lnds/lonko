#!/usr/bin/env node
// One-shot variant of poke.mjs: binds the socket, sends a single
// message, waits for one chat.reply (or timeout), then exits.
//
// Usage:
//   node ./scripts/poke-once.mjs "your message here"
//
// Optional env:
//   LONKO_CHAT_SOCKET   override socket path (defaults to ~/.claude/lonko-chat.sock)
//   POKE_TIMEOUT_MS     timeout in ms (default 90000)

import { createServer } from 'node:net';
import { unlinkSync, existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const message = process.argv.slice(2).join(' ').trim();
if (!message) {
  console.error('usage: poke-once.mjs "<message>"');
  process.exit(2);
}

const socketPath = process.env.LONKO_CHAT_SOCKET ?? join(homedir(), '.claude', 'lonko-chat.sock');
const timeoutMs = Number(process.env.POKE_TIMEOUT_MS ?? 90000);

if (existsSync(socketPath)) {
  console.error(`[poke-once] socket already exists at ${socketPath} — refusing to clobber`);
  process.exit(1);
}

let buf = '';
let conn = null;
let exitCode = 1;
let replied = false;

const server = createServer((c) => {
  conn = c;
  c.setEncoding('utf8');
  c.on('data', (chunk) => {
    buf += chunk;
    let nl;
    while ((nl = buf.indexOf('\n')) !== -1) {
      const line = buf.slice(0, nl);
      buf = buf.slice(nl + 1);
      if (!line) continue;
      let frame;
      try { frame = JSON.parse(line); } catch { continue; }
      switch (frame.kind) {
        case 'chat.online':
          console.log(`[poke-once] plugin online: ppid=${frame.ppid} pid=${frame.pid}`);
          console.log(`[poke-once] sending: ${message}`);
          c.write(JSON.stringify({ kind: 'chat.send', msg_id: 'poke-once-1', text: message }) + '\n');
          break;
        case 'chat.ack':
          console.log(`[poke-once] delivery: ${frame.status}`);
          break;
        case 'chat.reply':
          console.log(`[poke-once] reply (${frame.text.length} chars):\n--- BEGIN REPLY ---\n${frame.text}\n--- END REPLY ---`);
          replied = true;
          exitCode = 0;
          shutdown();
          break;
      }
    }
  });
  c.on('error', () => {});
  c.on('close', () => {
    if (!replied) {
      console.log('[poke-once] plugin disconnected before reply');
      shutdown();
    }
  });
});

server.listen(socketPath, () => {
  console.log(`[poke-once] listener bound at ${socketPath}`);
  console.log(`[poke-once] timeout in ${(timeoutMs / 1000).toFixed(0)}s`);
});

const timer = setTimeout(() => {
  console.error('[poke-once] timeout');
  shutdown();
}, timeoutMs);

function shutdown() {
  clearTimeout(timer);
  try { server.close(); } catch {}
  if (conn) { try { conn.destroy(); } catch {} }
  try { unlinkSync(socketPath); } catch {}
  process.exit(exitCode);
}

process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
