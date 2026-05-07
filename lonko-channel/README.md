# lonko-channel

Claude Code [channel plugin](https://code.claude.com/docs/en/channels-reference) that
bridges chat between the lonko TUI daemon and a running Claude Code
session over MCP. Inbound `chat.send` frames from lonko become
`<channel source="lonko" agent_id="...">` events in the session;
outbound `reply` tool calls become `chat.reply` frames back to lonko.

This is the v1 implementation: local-only, one host. The cross-tailnet
transport sketched in `docs/proposals/channels.md` is deferred to v2.

## Wire format

JSON-lines on `~/.claude/lonko-chat.sock` (override with
`LONKO_CHAT_SOCKET` for tests).

| direction       | frame                                                                       |
| --------------- | --------------------------------------------------------------------------- |
| plugin → daemon | `{kind: "chat.online", ppid, pid}`                                          |
| plugin → daemon | `{kind: "chat.reply", in_reply_to, text, agent_id}`                         |
| plugin → daemon | `{kind: "chat.ack", msg_id, status}`                                        |
| daemon → plugin | `{kind: "chat.send", msg_id, text}`                                         |

`agent_id` in v1 is the parent Claude Code PID, stringified — it
matches lonko's existing `Session::pid`, so no extra handshake is
needed beyond the `chat.online` announcement.

## Build

```sh
npm install
npx tsc          # emits dist/index.js
```

## Run with a real Claude Code session

The plugin is loaded as a development channel during the
research preview:

```sh
claude --dangerously-load-development-channels server:lonko
```

The `.mcp.json` in this directory tells Claude Code how to spawn the
plugin (via `node ./dist/index.js`). Run from this directory so the
project-level `.mcp.json` is picked up.

## Spike harnesses

Both scripts bind a fake daemon socket so you can drive the plugin
without standing up the real lonko TUI.

* `node ./scripts/spike.mjs` — self-contained smoke test that spawns
  the plugin as a subprocess, verifies the `chat.online` announcement,
  and round-trips a `chat.send` → `chat.ack` cycle. Good for CI.

* `node ./scripts/poke.mjs` — interactive REPL. With a real
  `claude --dangerously-load-development-channels server:lonko`
  session running in another terminal, type messages and see Claude's
  replies. Useful for manual end-to-end checks.

* `node ./scripts/poke-once.mjs "<message>"` — one-shot variant: send
  one message, wait for one reply, exit. Useful for scripted spikes
  against a live session.

## Debug logs

Set `LONKO_CHANNEL_DEBUG=1` for connect/handshake traces on stderr.
Quiet by default to avoid corrupting the MCP stdio protocol.
