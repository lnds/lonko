# Proposal: cross-tailnet chat with Claude Code agents via Channels

## Goal

From any lonko instance on the tailnet, view the live transcript of any
Claude Code agent (local or on a peer host) and send a chat message into
it. Replies arrive back in lonko's chat view.

## Background

- Claude Code v2.1.80+ ships [Channels](https://code.claude.com/docs/en/channels):
  an MCP server, registered with `--channels plugin:<name>@<marketplace>`,
  that **pushes** events into a running session. Claude can reply via the
  channel's `reply` tool. Telegram/Discord/iMessage are reference plugins.
- Today's lonko already has a **one-way remote bridge** (LONKO-48) that
  forwards hook events from a tailnet peer into the local lonko Unix
  socket via `ssh -R`, plus a permission-response path that uses
  `ssh host tmux send-keys` (out-of-band).
- The chat feature requires a **second direction** (lonko local → agent
  remote) plus an in-process delivery mechanism inside the agent's
  Claude Code session. Channels is the only documented mechanism for
  that delivery.

## Scope

In scope:
- Send-message-to-agent and receive-reply-from-agent across the tailnet.
- A "chat view" alongside the existing detail view, scoped to one agent.
- Custom channel plugin shipped with lonko.

Out of scope (for v1):
- Live tool-call / sub-prompt visibility from non-active agents
  (this stays on JSONL transcripts).
- Permission relay over the channel
  (existing tmux send-keys path keeps working).
- Reverse direction without an active hook stream
  (an agent must already be discoverable by lonko).

## Architecture

```
┌─ host A (TUI active) ────────┐         ┌─ host B (agent) ─────────────┐
│                              │         │                              │
│  lonko TUI                   │         │  Claude Code (agent X)       │
│       ↕ unix sock (existing) │         │       ↕ MCP stdio (channel)  │
│  lonko daemon ────────tailnet (mTLS via Tailscale) ─→ lonko daemon    │
│                              │         │       ↕ unix sock            │
│                              │         │  lonko-channel  (Bun script) │
└──────────────────────────────┘         └──────────────────────────────┘
```

Components:

1. **`lonko-channel`** (new artifact, Bun/TypeScript):
   - A pre-built channel plugin that Claude Code loads via
     `claude --channels plugin:lonko@lonko-marketplace`.
   - Connects to the **local** lonko daemon over `~/.claude/lonko.sock`
     (the same socket lonko-hook already uses, with a new framed message
     type).
   - Forwards inbound `chat.send` from the daemon as
     `<channel source="lonko">` events into the session.
   - Implements the `reply` tool: when Claude calls it, the plugin sends
     a `chat.reply` frame over the socket back to the daemon.

2. **lonko daemon (`lonko` binary, existing)**:
   - Already owns the Unix socket. We add a small **router** module that
     fans `chat.*` frames between:
     - the local channel plugin (one connection per local agent), and
     - peer daemons over the tailnet.
   - The router keys messages by `(host, agent_id)` — the same identity
     pair we already use to address remote agents in the agents list.

3. **Tailnet transport** (new):
   - Each daemon listens on a tailnet-only TCP port
     (`100.x.y.z:<port>`, ACL-restricted by Tailscale) for peer traffic.
   - Length-prefixed frames over TCP, MessagePack or JSON-lines (decision
     deferred — see open questions).
   - Authentication: machine identity is the tailnet itself. We trust
     any peer reachable on the tailnet TCP port. No app-level token.

## Message types

```rust
// Sent across the tailnet AND on the local socket between daemon and
// channel plugin. The wire format is identical for both hops.
enum ChatFrame {
    Send  { host: String, agent_id: String, text: String, msg_id: Uuid },
    Reply { host: String, agent_id: String, text: String, in_reply_to: Uuid },
    Ack   { msg_id: Uuid, status: AckStatus },
}

enum AckStatus { Delivered, AgentNotFound, ChannelOffline }
```

Routing rules in the daemon:
- `Send { host, .. }` arriving on local socket from TUI:
  - if `host == self`: dispatch to the local channel plugin for that
    `agent_id`.
  - else: forward to the peer daemon at `host`.
- `Send` arriving on tailnet TCP from a peer daemon:
  - dispatch to the local channel plugin for `agent_id`.
- `Reply` mirrors the same logic in reverse.

## Discovery

- Each daemon exports its tailnet listen port via the existing tailscale
  hostname (no new service registration). Default port + per-host
  override in `~/.config/lonko/config.toml`.
- An agent becomes "chat-capable" when its lonko-channel plugin connects
  to the local daemon and announces `(agent_id, session_id)`. The local
  daemon publishes a small `agent.online` frame to peers, which makes
  the chat icon light up in their TUIs.
- An agent without `lonko-channel` (e.g. running on a host that doesn't
  have lonko's channel plugin installed) shows a "no chat" indicator —
  the agents list still works, only chat is disabled for it.

## Plugin lifecycle

Today the user starts an agent with a `lonko new-agent ...` flow. We add
an opt-in flag (eventually the default) that wraps the spawned Claude
Code with `--channels plugin:lonko@lonko-marketplace`.

Initial release:
- A user-installed marketplace
  (`/plugin marketplace add lnds/lonko-marketplace`) that exposes the
  `lonko` channel plugin.
- During research preview, we document the
  `--dangerously-load-development-channels` escape hatch as a fallback
  if the marketplace path is unavailable.

## TUI changes

- New `c` keybinding on a selected agent in the agents tab: opens the
  **chat view**.
- Chat view: scrollback of `Send`/`Reply` frames for that agent
  (in-memory ring, persisted to `~/.cache/lonko/chat/<host>/<agent>.jsonl`
  for survival across restarts).
- Footer: input line + send key. Input is plain text; future work to
  allow file attachments, but Channels payload is text-only in v1.
- An indicator in the agents list when a chat-capable agent has unread
  replies (`*` badge similar to `WaitingForUser`).

## Code changes (rough)

```
crates/lonko/src/
  sources/
    hooks.rs          # extend to recognise chat.* frames coming from
                      # the local channel plugin, route to chat module
    chat.rs    (new)  # Chat router: routes Send/Reply between local
                      # plugin connections, peer daemons, and the TUI
  net/         (new)
    peer.rs           # TCP server: accept, handshake, framed read/write
    client.rs         # TCP client pool: one connection per peer host
  ui/
    chat.rs    (new)  # Chat view widget, input handling
  state.rs     # Add ChatState per (host, agent_id)
crates/lonko-channel/   (new, TypeScript/Bun)
  index.ts              # MCP server: stdio for Claude Code, unix sock
                        # for daemon, reply tool implementation
  package.json
  plugin.json           # channel plugin manifest
```

The existing remote bridge (`sources/remote_bridge.rs`) **stays as-is**.
It carries hook events. The new TCP transport is **separate** and
carries only chat frames. Mixing them is tempting but rejected: the
ssh -R bridge is one-way by design, and reusing it would require
either flipping its direction per-host (race-prone) or layering a
multiplex on top (complexity that buys nothing).

## Failure modes

| Scenario | Behaviour |
|---|---|
| Peer daemon down | `Send` returns `Ack(ChannelOffline)`; TUI shows error inline |
| Channel plugin not running on target | `Send` returns `Ack(ChannelOffline)`; chat icon stays grey |
| Tailnet down | Same as peer down; bridge already has reconnect logic to model after |
| Agent crashed mid-reply | Channel plugin dies; daemon notices socket EOF; emits `agent.offline` |
| Two messages in flight | Both delivered in send order; replies may interleave (Channels offers no ordering guarantee per docs — verify) |

## Migration / compat

- Existing single-host users: nothing changes. The chat feature is
  gated behind installing the channel plugin and starting agents with
  `--channels`.
- Existing remote-bridge users: the SSH bridge keeps working. The new
  TCP listener is bound only when peers are configured (or auto-detected
  from tailnet) and idle otherwise.

## Open questions

1. **Wire format**: JSON-lines (matches existing socket) or
   MessagePack (smaller, but new dep)? Lean toward JSON-lines for
   debuggability; chat traffic is low volume.
2. **Marketplace publishing**: do we publish `lonko-marketplace` early
   (so users get the plugin via `/plugin install`), or ship the plugin
   inside the lonko repo and require `--dangerously-load-development-channels`
   for v1? The latter is simpler but visibly "research-y".
3. **claude.ai login on every host**: Channels requires it. We should
   document this prominently and detect-and-warn at install time.
4. **Identity & abuse**: do we want any app-level signature beyond
   tailnet trust? My read: no, but worth confirming.
5. **Permission relay**: punted to v2, but the channel reference docs
   suggest the plugin can opt in. Confirm scope.

## Phases

- **v1**: local-only chat (one host). Validates the plugin and the
  daemon-router design. No tailnet TCP transport yet.
- **v2**: tailnet TCP transport, peer daemon discovery, multi-host chat.
- **v3**: permission relay over the channel (replace `tmux send-keys`
  for permissions where the channel is available).
