#!/usr/bin/env node
// Daemon-fake: binds `~/.claude/lonko-chat.sock` so a real running
// `lonko-channel` plugin (spawned by `claude --dangerously-load-development-
// channels server:lonko`) can connect to us. Exercises the end-to-end
// chat path against a live Claude Code session without needing the
// real lonko TUI on the box.
//
// Usage:
//   1. Start a Claude session in another terminal:
//        cd lonko-channel && claude --dangerously-load-development-channels server:lonko
//   2. Run this script:
//        node ./scripts/poke.mjs
//   3. Type a message and hit Enter. It arrives in the Claude session
//      as <channel source="lonko" agent_id="...">. When Claude calls
//      the `reply` tool, the response prints back here.
//   4. Ctrl-D / Ctrl-C to exit. Cleans up the socket on the way out.

import { createServer } from 'node:net';
import { unlinkSync, existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import readline from 'node:readline';

const socketPath = process.env.LONKO_CHAT_SOCKET ?? join(homedir(), '.claude', 'lonko-chat.sock');

if (existsSync(socketPath)) {
  console.error(`[poke] socket already exists at ${socketPath} — refusing to clobber`);
  console.error(`[poke] (probably another lonko/poke running — check with: lsof ${socketPath})`);
  process.exit(1);
}

let conn = null;
let buf = '';
let nextId = 1;

const server = createServer((c) => {
  if (conn) {
    console.error('[poke] second connection arrived — closing the old one');
    conn.destroy();
  }
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
          console.log(`\n[poke] plugin online: ppid=${frame.ppid} pid=${frame.pid}`);
          rl.prompt();
          break;
        case 'chat.reply':
          console.log(`\n[claude] ${frame.text}`);
          rl.prompt();
          break;
        case 'chat.ack':
          // delivery confirmation; quiet by default
          break;
        case 'chat.offline':
          console.log(`\n[poke] plugin offline ppid=${frame.ppid}`);
          break;
        default:
          console.log(`\n[poke] unknown frame: ${JSON.stringify(frame)}`);
      }
    }
  });
  c.on('close', () => {
    if (conn === c) {
      console.log('\n[poke] plugin disconnected');
      conn = null;
    }
  });
  c.on('error', (err) => {
    console.error(`\n[poke] socket error: ${err.message}`);
  });
});

server.listen(socketPath, () => {
  console.log(`[poke] listener bound at ${socketPath}`);
  console.log('[poke] waiting for plugin to connect (it retries every 1s)...');
});

const rl = readline.createInterface({ input: process.stdin, output: process.stdout, prompt: '> ' });
rl.on('line', (line) => {
  if (!line) { rl.prompt(); return; }
  if (!conn) { console.log('[poke] no plugin connected yet'); rl.prompt(); return; }
  const msg_id = `poke-${nextId++}`;
  conn.write(JSON.stringify({ kind: 'chat.send', msg_id, text: line }) + '\n');
});

function shutdown() {
  console.log('\n[poke] shutting down');
  try { server.close(); } catch {}
  if (conn) { try { conn.destroy(); } catch {} }
  try { unlinkSync(socketPath); } catch {}
  process.exit(0);
}
rl.on('close', shutdown);
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
