#!/usr/bin/env node
// Spike harness: bind a fake daemon socket, launch the compiled plugin
// as a subprocess, drive an MCP `initialize` so the channel plugin
// finishes its boot, and verify the handshake against the daemon side
// that the real lonko Rust listener implements.
//
// What this validates:
//   1. plugin opens a Unix socket connection to LONKO_CHAT_SOCKET
//   2. plugin emits a `chat.online` JSON-line with its PPID
//   3. plugin reacts to a `chat.send` frame (acks back via `chat.ack`)
//
// What this does NOT validate:
//   - the MCP `notifications/claude/channel` push reaching Claude — for
//     that you need a real Claude Code session and Channels enabled.
//   - the `reply` tool path — same reason; that requires Claude calling
//     into the MCP server.

import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const dir = mkdtempSync(join(tmpdir(), 'lonko-channel-spike-'));
const socketPath = join(dir, 'chat.sock');

let buffered = '';
const onlineDeferred = defer();
const ackDeferred = defer();

const server = createServer((conn) => {
  console.log(`[spike] plugin connected from PID ${conn.localPort ?? '?'}`);
  conn.setEncoding('utf8');
  conn.on('data', (chunk) => {
    buffered += chunk;
    let nl;
    while ((nl = buffered.indexOf('\n')) !== -1) {
      const line = buffered.slice(0, nl);
      buffered = buffered.slice(nl + 1);
      if (!line) continue;
      let frame;
      try {
        frame = JSON.parse(line);
      } catch {
        console.error(`[spike] bad frame: ${line}`);
        continue;
      }
      console.log(`[spike] inbound: ${JSON.stringify(frame)}`);
      if (frame.kind === 'chat.online') {
        onlineDeferred.resolve(frame);
        // push a chat.send back to exercise the inbound path
        conn.write(JSON.stringify({ kind: 'chat.send', msg_id: 'spike-1', text: 'hello from spike' }) + '\n');
      } else if (frame.kind === 'chat.ack') {
        ackDeferred.resolve(frame);
      }
    }
  });
});

server.listen(socketPath, () => {
  console.log(`[spike] listener bound at ${socketPath}`);
});

const child = spawn('node', ['./dist/index.js'], {
  env: { ...process.env, LONKO_CHAT_SOCKET: socketPath },
  stdio: ['pipe', 'pipe', 'inherit'],
});
child.on('exit', (code, sig) => {
  console.log(`[spike] plugin exited code=${code} signal=${sig}`);
});

// Drive a minimal MCP `initialize` so the plugin's `await mcp.connect()`
// settles. Without this, `connectDaemon()` still runs (it's invoked
// before the await), but we exercise the post-connect behavior too.
const initialize = {
  jsonrpc: '2.0',
  id: 1,
  method: 'initialize',
  params: {
    protocolVersion: '2024-11-05',
    capabilities: {},
    clientInfo: { name: 'spike', version: '0.0.1' },
  },
};
child.stdin.write(JSON.stringify(initialize) + '\n');

// Read MCP responses from plugin stdout — log them for visibility but
// the spike's success criteria is on the daemon socket side.
let stdoutBuf = '';
child.stdout.setEncoding('utf8');
child.stdout.on('data', (chunk) => {
  stdoutBuf += chunk;
  let nl;
  while ((nl = stdoutBuf.indexOf('\n')) !== -1) {
    const line = stdoutBuf.slice(0, nl).trim();
    stdoutBuf = stdoutBuf.slice(nl + 1);
    if (line) console.log(`[plugin→host] ${line}`);
  }
});

const timeout = setTimeout(() => {
  console.error('[spike] timeout waiting for handshake');
  child.kill('SIGTERM');
  process.exit(1);
}, 5000);

try {
  const online = await onlineDeferred.promise;
  console.log(`[spike] OK chat.online received: ppid=${online.ppid} pid=${online.pid}`);
  if (online.ppid !== process.pid) {
    console.error(`[spike] FAIL: expected ppid=${process.pid}, got ${online.ppid}`);
    process.exitCode = 1;
  }

  const ack = await ackDeferred.promise;
  console.log(`[spike] OK chat.ack received: msg_id=${ack.msg_id} status=${ack.status}`);
  if (ack.msg_id !== 'spike-1') {
    console.error(`[spike] FAIL: expected msg_id=spike-1, got ${ack.msg_id}`);
    process.exitCode = 1;
  }

  console.log('[spike] handshake validated end-to-end');
} finally {
  clearTimeout(timeout);
  child.kill('SIGTERM');
  server.close();
  rmSync(dir, { recursive: true, force: true });
}

function defer() {
  let resolve, reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}
