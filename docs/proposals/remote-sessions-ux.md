# Remote Sessions UX

## Problem & current state

Lonko is a single-developer TUI. The user runs one workstation instance and one
or more notebook peers (e.g. `kayshon`) that forward hook events over `ssh -R`.
The Remote tab shows host headers and tmux-session cards, but the host model is
shallow: `HostStatus` is binary (`Online | Unreachable`) and is set by SSH
poll success, not by anything that reflects whether lonko-hook, lonko-channel,
or the correct binary version are actually running.

The result is a class of silent failures. Today's incident on kayshon hit all
of them at once:

- Binary on kayshon was v0.25.0; local was v0.26.0.
- `lonko-channel/dist/` had never been built, so the chat socket had no client.
- The repo was four commits behind (and local hadn't pushed, so it was four
  commits behind origin too).

None of these were visible from the workstation TUI. The user had to SSH in
and diagnose manually. The design below is aimed at making that class of
incident self-diagnosing — and ideally self-healing.

---

## Principles

**P1 — Absence of a signal is not silence; it is a yellow light.**
Every component that is expected to be present and is not present must
show a distinct, named status in the UI. "No news is good news" is wrong
for distributed systems on a dev tailnet.

**P2 — The provisioning contract is exhaustive or it is nothing.**
`lonko install-remote <host>` either leaves the host fully ready (binary +
hooks + channel plugin built) or it must report what is still missing.
A partial install is worse than no install because it produces misleading
`Online` status.

**P3 — Version skew is always visible, never assumed away.**
When local and remote binaries diverge, the TUI shows the skew immediately
on the host header. The user should never need to SSH in to discover it.

**P4 — One keypress to diagnose, one command to fix.**
The always-visible host header carries a compact health summary. A single
keypress (e.g. `i`) expands a detail panel with the exact delta and the
command to fix it. A second keypress (e.g. `U`) runs the fix.

**P5 — Chat affordances reflect real capability.**
The `c` keybinding to open chat is only shown and active when the target
agent's channel plugin is confirmed connected. When it is not, the TUI shows
_why_ (plugin unbuilt, daemon unreachable, version mismatch) rather than
opening a chat view that silently fails.

**P6 — Peer registry is explicit, not inferred.**
A host does not enter the managed fleet just because it appears in
`tailscale status`. It enters when the user runs `lonko install-remote` (or
the equivalent TUI action). Registered peers persist in config and carry
metadata (last-provisioned version, last-seen health). Unregistered tailnet
peers are discoverable but visually distinct.

---

## Information architecture

### Objects

#### Registered Peer
A host that the user has explicitly provisioned via `lonko install-remote` (or
the TUI action). Persisted in `~/.config/lonko/config.toml` under a `[[peer]]`
table. Fields:

| field | type | notes |
|---|---|---|
| `hostname` | String | matches `lonko-hook --remote-tag` stamp |
| `provisioned_version` | String | lonko version at last successful install |
| `plugin_built` | bool | whether `dist/index.js` was built during install |
| `last_seen` | timestamp | last tick on which a hook event arrived from this host |

Registered peers are shown in the Remote tab regardless of whether they are
currently online. This is important: a peer that has gone silent should not
disappear from the list.

#### Tailnet Peer (unregistered)
Any host visible in `tailscale status --json` that is _not_ in the peer
registry. Shown in the Remote tab in a dimmed "unregistered" section.
Displays hostname + OS. No health checks are run. The user can promote it
with `I` (install).

#### Host Health
A computed value derived from:
- SSH reachability (existing poll).
- Remote binary version (new: `ssh host lonko --version`, cached).
- Plugin build state (new: `ssh host test -f ~/.claude/lonko-channel/dist/index.js`).
- Chat socket liveness (new: `ssh host test -S ~/.claude/lonko-chat.sock`).
- Last-hook-event timestamp (already tracked via `last_activity` on sessions
  belonging to that host).

Health is not a boolean. It is a named enum with five levels:

```
Healthy        — all components present, version matches, hook seen recently
VersionSkew    — binary or plugin version diverges from local
PluginMissing  — lonko-hook present but dist/index.js not built
ChatDead       — plugin built but chat socket has no connected client
Unreachable    — SSH poll failed
```

#### Fleet View
A new top section of the Remote tab (rendered before the session cards) that
shows all registered peers as a compact row each, plus a "tailnet" section of
unregistered peers.

---

### Views

#### Remote tab — Fleet section (always visible at top)

Each registered peer gets one row (2 lines), regardless of whether it has
active sessions.

```
 ● kayshon  v0.26.0  󱘖 3s  ◉ chat      [↓]
  ─────────────────────────────────────────

 ⚠ thinkpad  v0.25.0 ≠ local  plugin: missing  31m
  ─────────────────────────────────────────

 ✕ oldbox    unreachable since 4h ago
  ─────────────────────────────────────────
```

Legend:
- `●` green = Healthy; `⚠` yellow = degraded (VersionSkew / PluginMissing /
  ChatDead); `✕` red = Unreachable.
- `v0.26.0` = remote binary version (dim when matches local, amber when skew).
- `≠ local` appears next to the version only when there is a skew.
- `󱘖 3s` = age of last hook event from this host.
- `◉ chat` = chat socket is live (plugin connected); `○ chat` = plugin absent
  or not connected; shown only when the host is Online.
- `[↓]` = an update is available (local > remote); pressing `U` on a
  `[↓]` host runs `lonko update-remote <host>`.

#### Remote tab — Host detail panel (one keypress: `i`)

When the user presses `i` on a selected host row, a detail panel expands
below the header (replacing the session list for that host). It shows the
full health breakdown:

```
┌─ kayshon ─────────────────────────────────────────────────────────────────┐
│  Binary     v0.26.0  (local v0.26.0)  OK                                  │
│  Hook cfg   ~/.claude/settings.json   OK                                  │
│  Plugin     dist/index.js             built  (last tsc: 2h ago)           │
│  Chat sock  ~/.claude/lonko-chat.sock connected  (agent PID 42891)        │
│  Last event 3s ago                                                         │
│                                                                            │
│  Actions:  U update   B rebuild-plugin   R reprovision   q close          │
└────────────────────────────────────────────────────────────────────────────┘
```

For a degraded host:

```
┌─ thinkpad ────────────────────────────────────────────────────────────────┐
│  Binary     v0.25.0  (local v0.26.0)  SKEW — run U to update             │
│  Hook cfg   ~/.claude/settings.json   OK                                  │
│  Plugin     dist/index.js             MISSING — run B to build            │
│  Chat sock  n/a (plugin not built)                                        │
│  Last event 31m ago                                                        │
│                                                                            │
│  Actions:  U update   B rebuild-plugin   R reprovision   q close          │
└────────────────────────────────────────────────────────────────────────────┘
```

The panel is modal-light: `q` or a second `i` closes it and returns cursor
focus to the session list. It does not replace any other view.

#### Agent card — chat affordance

In the Agents tab, the `c` keybinding and the chat icon on a session card are
conditional:

```
 ◉ lonko / feat/remote-ux  @kayshon  ◉ chat         c:chat
                                                            ^^^^ only shown when chat is live

 ◉ lonko / feat/remote-ux  @kayshon  ○ chat
                                     ^^^^ grayed when channel offline
```

Pressing `c` on an agent with `○ chat` does not open the chat view. Instead,
the footer shows a one-line explanation:

```
  chat offline — plugin not connected on kayshon  (i: details)
```

#### Chat overlay — remote header

When chat is opened for a remote agent, the overlay title bar extends to
show the target host:

```
┌─ chat: lonko/feat/remote-ux @ kayshon ──────────────── ○ live ─────────┐
```

States for the right-side indicator:
- `◉ live` (green) — channel connected, messages flowing.
- `○ live` (dim) — channel was connected; daemon received `chat.offline`
  (plugin process ended or Claude session terminated).
- `⚠ degraded` (amber) — tailnet reachable but last Send returned
  `Ack(ChannelOffline)`.
- `✕ offline` (red) — peer daemon unreachable; last attempt failed.

On `○ live` or worse, the input field is disabled and a banner appears
above it:

```
  [chat unavailable — kayshon channel offline.  i: diagnose  U: reconnect]
```

The user can still scroll the message history. They cannot send until the
channel is restored.

---

## Key flows

### 1. Provisioning a new peer

**Entry points:**
- CLI: `lonko install-remote <host>` (existing, augmented).
- TUI: from the "unregistered" tailnet peer row, press `I`.

**Steps (CLI, unchanged shell output contract, new steps added):**

```
==> Checking SSH connectivity to kayshon...    OK
==> Checking Rust toolchain on kayshon...      cargo 1.87.0
==> Installing lonko-hook v0.26.0 on kayshon...
    lonko-hook installed to ~/.cargo/bin/lonko-hook
==> Installing lonko binary v0.26.0 on kayshon...   [NEW]
    lonko installed to ~/.cargo/bin/lonko
==> Building lonko-channel plugin on kayshon...      [NEW]
    cd ~/.claude/lonko-channel && npm install && npx tsc
    dist/index.js built successfully
==> Configuring Claude hooks in kayshon:~/.claude/settings.json...
    SessionStart: added
    ...
==> Registering kayshon in ~/.config/lonko/config.toml...  [NEW]
    provisioned_version = "0.26.0"

Done. kayshon is fully provisioned.
  Hook events:  will flow once the SSH bridge is active
  Chat:         ready (plugin built, start claude with --dangerously-load-development-channels server:lonko)
```

The `--new` steps are the critical additions. The registration step writes
the `[[peer]]` entry. Without registration, the host is not tracked for
health checks or update flows.

**TUI flow (from unregistered peer):**

```
 ○ kayshon  linux  (unregistered)      I:install
```

Pressing `I` opens a small confirmation modal:

```
┌─ Install lonko on kayshon? ────────────────────────────────────────────────┐
│  This will:                                                                │
│    • cargo install lonko + lonko-hook v0.26.0 (may take several minutes)  │
│    • build lonko-channel/dist/index.js                                     │
│    • merge hook entries into ~/.claude/settings.json                       │
│    • register kayshon in your local lonko config                           │
│                                                                            │
│  Enter: confirm   Esc: cancel                                              │
└────────────────────────────────────────────────────────────────────────────┘
```

After confirmation the TUI shows a progress log (same output as the CLI) in a
scrollable overlay. On completion the peer moves from "unregistered" to the
fleet section.

### 2. Updating a peer after local advances

**Entry points:**
- TUI: `U` on a host row showing `[↓]` (VersionSkew or PluginMissing).
- CLI: `lonko update-remote <host>` (new subcommand).

**What "update" does:**
1. SSH → `cargo install --git ... --tag v<local_version> --locked lonko lonko-hook`.
2. SSH → `cd ~/.claude/lonko-channel && git pull && npm install && npx tsc`.
3. Update `provisioned_version` in `~/.config/lonko/config.toml`.
4. Trigger an immediate health re-check.

The TUI shows a progress overlay identical to provisioning. On success the host
header returns to `●` Healthy.

`lonko update-remote` without `<host>` updates all registered peers that are
out of date. This is the "I just pushed a new version, fix everything" command.

### 3. On-demand health check

Health data is gathered lazily:
- Binary version: checked once per lonko startup per registered peer, and
  again after any `update-remote`. Result cached in `RemoteHost` state.
- Plugin state: checked at startup and after any `update-remote`.
- Chat socket liveness: derived from `chat.online` / `chat.offline` events
  (no SSH poll needed).
- SSH reachability: existing periodic poll, unchanged.

The user can force a refresh with `r` on any host row. This runs all four
checks and updates the host header within ~5 seconds.

The detail panel (`i`) always shows the cached values plus a "checked Ns ago"
timestamp. The user knows whether to trust the cache.

### 4. Cross-host chat

**Opening chat for a remote agent:**

The flow is identical to local chat (press `c` on an agent card in the Agents
tab) with two preconditions checked before the overlay opens:

1. `chat_online` contains the agent's `agent_id`. This is set when the
   `chat.online` frame arrives from the lonko-channel plugin on the remote host
   via the tailnet TCP transport (v2). In v1 (local-only), only local agents
   are chat-capable.
2. The host's `HostHealth` is not `Unreachable`.

If either precondition fails, the footer message (described above) fires
instead of opening the overlay.

**Chat overlay header for remote agents:**

```
┌─ chat: lonko/feat/remote-ux @ kayshon ──────────────── ◉ live ─────────┐
│  [you]  14:22  fix the permission relay, using tmux send-keys for now   │
│                                                                          │
│  [agent] 14:22  Understood. I'll keep the tmux send-keys path and       │
│                 defer the channel relay to v3.                           │
│                                                                          │
│  [you]  14:23  █                                                         │
│ ─────────────────────────────────────────────────────────────────────── │
│  > _                                                                     │
└──────────────────────────────────────────────────────────────────────────┘
```

The `@ kayshon` segment is only shown for remote agents. Local agents show
just `chat: lonko/feat/remote-ux`. This preserves the existing local chat
overlay appearance for the common case.

**Degraded-channel banner (inserted above input when not live):**

```
│  ─────────────────────────────────────────────────────────────────────  │
│  [!] kayshon channel offline — plugin disconnected                       │
│      i: diagnose   U: reconnect                                          │
│  ─────────────────────────────────────────────────────────────────────  │
│  > (input disabled)                                                      │
```

The user keeps access to message history. The input field shows `(input
disabled)` in dim text rather than blinking the cursor on an inoperable field.

---

## Failure modes

Each failure maps to a principle.

| Failure | Principle violated (today) | How the design prevents it |
|---|---|---|
| Remote binary is stale and nobody knows | P3 | Binary version check at startup; `VersionSkew` shown on host header immediately |
| Plugin never built; chat socket has no client | P1, P2 | `install-remote` now builds the plugin; `PluginMissing` is a distinct health state, not hidden under `Online` |
| Repo behind origin, plugin code stale | P2 | `update-remote` pulls the repo and rebuilds the plugin as a single atomic step |
| Chat overlay opens but messages never deliver | P5 | `c` is gated on `chat_online`; if not connected, footer explains why before opening anything |
| Host goes offline silently | P1 | Registered peers stay in the fleet list with `✕ Unreachable` + age; they do not disappear |
| User does not know what is wrong | P4 | `i` expands the detail panel; every degraded field names the fix command |
| Tailnet peer appears in list but is not managed | P6 | Unregistered peers are visually separate and cannot acquire an `Online` badge via the existing poll |

---

## Open questions

**Q1 — Version check cost and frequency.**
The proposed design checks `lonko --version` over SSH at startup and after
updates. How often should it re-check during a running session? Options:
(a) only on startup + after explicit `r`; (b) every N minutes in the
background. Option (a) is simpler and avoids SSH chattiness, but the workstation
may be running for days. A background check every 15–30 minutes is probably
right but should be confirmed.

**Q2 — Where does the channel plugin live on the remote host?**
Today the README says `--dangerously-load-development-channels server:lonko`
and the plugin is loaded from the working directory's `.mcp.json`. For a
provisioned remote host, the user runs claude from an arbitrary directory.
The install flow needs to place the plugin (and its `dist/index.js`) in a
stable, well-known path (e.g. `~/.claude/lonko-channel/`) and write a
user-level `.mcp.json` or equivalent. The exact Claude Code mechanism for a
user-level channel plugin needs to be nailed down before implementing the
install step.

**Q3 — v1 chat scope vs. the fleet section.**
The fleet section and health checks are independent of the v2 cross-host chat
transport. They can land before v2. The open question is: should v1 of this
design (the fleet section, version checks, `install-remote` plugin build)
ship as a single PR, or should health visibility and install completeness be
split into two smaller pieces? The recommendation here is to ship health
visibility (P1, P3, P4) first — it requires no new SSH commands beyond one
`lonko --version` call per peer — and then tackle install completeness (P2)
as a second step once the stable plugin path (Q2) is resolved.
