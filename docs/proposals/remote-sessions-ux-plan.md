# Remote Sessions UX — Implementation Plan

Source of truth for design intent: `docs/proposals/remote-sessions-ux.md`.
This document is the execution plan for the single developer implementing it.

---

## Scope boundaries

**In scope:** M1 (fleet health visibility) and M2 (install completeness + peer
registry) as described below.

**Explicitly out of scope for this plan:**
- Cross-tailnet chat transport (sending messages to remote agents). That
  requires the v2 TCP channel transport described in `docs/proposals/channels.md`
  and is blocked on that separate protocol work. The affordance changes in M1
  (gating `c` on plugin-connected state) are preparatory groundwork only; they
  do not ship the transport itself.
- TUI-initiated install (`I` keypress on an unregistered peer). The provisioning
  logic is complex enough to land first as a CLI command (M2); the TUI modal can
  follow in a subsequent PR once the CLI is proven.

---

## M1 — Fleet health visibility

### Dependency order within M1

```
Step 1 (HostHealth model)
  └─ Step 2 (periodic checker)
       └─ Step 3 (wire into UI header + chat gate)
            └─ Step 4 (i-key detail panel)
                  └─ Step 5 (tests + verification)
```

Step 3 can be split into two PRs (header changes vs. chat gate changes) if you
want to keep PRs smaller — they share no code paths.

---

### M1-Step 1 — HostHealth data model

**Files touched:**
- `crates/lonko/src/state.rs` (new types + `RemoteHost` extension)

**What it does:**

Replace the binary `HostStatus` with a richer type. Keep `HostStatus` as a
backwards-compat re-export or remove it outright — only two match arms exist
(`ui/remote.rs:135-136`, `app/remote.rs:303-316`) so a rename is painless.

Add to `state.rs`:

```rust
/// Granular health of a registered remote peer. Variants are ordered
/// from most to least degraded so comparison operators work correctly
/// (Unreachable < ChatDead < … < Healthy).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostHealth {
    Unreachable,
    PluginMissing,
    ChatDead,
    VersionSkew { remote: String, local: String },
    Healthy,
}

/// Cached health probe results for a registered peer.
#[derive(Debug, Clone)]
pub struct HealthCache {
    /// Output of `lonko --version` on the remote, or None if not yet checked.
    pub remote_version: Option<String>,
    /// Whether `~/.claude/lonko-channel/dist/index.js` exists on the remote.
    pub plugin_built: Option<bool>,
    /// Derived health level. None means "not yet checked".
    pub health: Option<HostHealth>,
    /// Monotonic instant when the last probe ran.
    pub checked_at: Option<std::time::Instant>,
    /// Consecutive version-check failures (for backoff).
    pub probe_fail_count: u32,
    /// Tick at which the next background probe is due.
    pub next_probe_tick: u64,
}

impl Default for HealthCache {
    fn default() -> Self {
        Self {
            remote_version: None,
            plugin_built: None,
            health: None,
            checked_at: None,
            probe_fail_count: 0,
            next_probe_tick: 0,
        }
    }
}
```

Extend `RemoteHost` in `state.rs:677-685`:

```rust
pub struct RemoteHost {
    pub hostname: String,
    pub status: HostStatus,   // keep for SSH reachability (existing poll unchanged)
    pub sessions: Vec<TmuxSession>,
    pub fail_count: u32,
    pub next_poll_tick: u64,
    // NEW
    pub health: HealthCache,
}
```

The `HostHealth` derives from `HealthCache` + `status`. Add a free function
`fn effective_health(host: &RemoteHost) -> Option<HostHealth>` that:
1. Returns `None` if `health.health` is `None` (not yet probed).
2. Returns `Some(Unreachable)` if `status == HostStatus::Unreachable`.
3. Otherwise returns `health.health.clone()`.

**Depends on:** nothing.

**Test:** Unit test `effective_health` with all five combinations in
`state.rs` `#[cfg(test)]`.

---

### M1-Step 2 — Periodic health checker

**Files touched:**
- `crates/lonko/src/event.rs` (new event variant)
- `crates/lonko/src/app/remote.rs` (checker spawn + handler)

**What it does:**

Add a new event variant to `event.rs`:

```rust
/// Result of a background SSH health probe for a registered peer.
HostHealthProbed {
    host: String,
    remote_version: Option<String>,
    plugin_built: bool,
    /// True when the SSH probe itself failed (not just bad values).
    probe_failed: bool,
},
```

In `app/remote.rs`, add `spawn_host_health_probe(host, tx)` — a
`tokio::task::spawn_blocking` closure that:
1. Runs `ssh -o BatchMode=yes -o ConnectTimeout=5 <host> 'lonko --version && test -f ~/.claude/lonko-channel/dist/index.js && echo plugin_ok || echo plugin_missing'` as a single SSH invocation (one round-trip, not two).
2. Parses stdout for the version line and the plugin sentinel.
3. Sends `Event::HostHealthProbed { ... }`.

**Cadence commitment:** Background probes run every **20 minutes** per
registered peer (1200 ticks at 100 ms/tick = `next_probe_tick = tick + 12000`).
Also triggered immediately on startup (tick 0) and after any `update-remote`
or `install-remote` completes. The user can force a refresh with `r` on any
host row (step 4), which resets `next_probe_tick = 0`.

Rationale for 20 min: Eric suggested 15–30 min. 30 min is too long for a
developer who notices version skew and wants to know it resolved. 15 min is
close to correct. 20 min lands in the middle and is a round number of ticks.

Handle `Event::HostHealthProbed` in `app/remote.rs`:
- Find the matching host in `state.remote_hosts`.
- Populate `health.remote_version`, `health.plugin_built`, `health.checked_at`.
- Compute and set `health.health` based on version comparison + plugin flag.
  Local version: `env!("CARGO_PKG_VERSION")`.
- If `probe_failed`: increment `health.probe_fail_count`, apply exponential
  backoff (same `backoff_ticks` helper already at line 281 of `app/remote.rs`).

Wire into `on_tick` (the existing periodic-check block in `app.rs`): iterate
registered hosts, check `next_probe_tick <= tick`, spawn probes where due.

**Chat-socket liveness:** do NOT SSH-poll the socket. As the proposal notes,
derive it from `Event::ChatOnline`/`Event::ChatOffline` which already exist in
`event.rs:73-75`. Add `chat_connected: bool` to `RemoteHost` and flip it on
those events (matched by `session.host`). This avoids an extra SSH round-trip
entirely.

**Depends on:** M1-Step 1.

**Test:** Unit-test the version-comparison logic and the `HostHealth` derivation
function with fixture strings (no SSH needed). Integration test: manual — run
lonko, verify the probe fires at startup and again after ~20 min per log output.

---

### M1-Step 3 — Wire HostHealth into UI

**Files touched:**
- `crates/lonko/src/ui/remote.rs` (host header row)
- `crates/lonko/src/ui/chat.rs` (chat title + input gate)
- `crates/lonko/src/ui/list.rs` or `crates/lonko/src/ui/footer.rs`
  (chat key hint conditional)

**Part A — Host header (ui/remote.rs)**

`RenderItem::HostHeader` currently carries only `hostname`, `status`, and
`session_count` (`remote.rs:103`). Extend it to carry `health: Option<&HostHealth>`
and `remote_version: Option<&str>` and `chat_connected: bool`.

Update `render_item` (`remote.rs:131`) for `HostHeader`:

- Status glyph: `●` green when Healthy, `⚠` amber when VersionSkew /
  PluginMissing / ChatDead, `✕` red when Unreachable, `?` dim when None
  (not yet probed).
- Version span: show `remote_version` dimmed when it matches local; amber + `≠`
  when it doesn't; omit when None.
- Chat indicator: `◉ chat` (teal) when `chat_connected`, `○ chat` (dim) when not.
  Only show when host is Online.
- `[↓]` update badge: show when `health == Some(VersionSkew { .. })` and local
  version is newer.
- Last-event age: derive from the max `last_activity` across sessions belonging
  to this host. Add `AppState::host_last_event_age(hostname) -> Option<Duration>`
  helper in `state.rs`.

Keep the existing separator line. Total host-header height stays 2 lines —
all new info fits on the first line at typical terminal widths (80+).

**Part B — Chat gate (ui/chat.rs + footer)**

In `ui/chat.rs::render`, the `online` flag at line 40 gates the input.
Extend: when the agent belongs to a remote host (`session.host.is_some()`),
also require that host's `chat_connected` flag. If the agent is remote and
chat is not connected, disable the input field and show the banner:

```
[chat offline — plugin not connected on <host>  (i: diagnose)]
```

In the agents-list footer (`ui/footer.rs`), gate the `c:chat` hint: show it
only when `state.chat_online.contains(agent_id)`. For remote agents, also
require `chat_connected` on the host. When chat is unavailable, show
`○ chat` instead of `c:chat`.

**Depends on:** M1-Step 1, M1-Step 2 (for the new RemoteHost fields).

**Test:** Manual TUI check with chat_connected toggled via a simulated
`Event::ChatOffline`. Add a unit test to `state.rs` for
`host_last_event_age`.

---

### M1-Step 4 — `i` keypress: host detail panel

**Files touched:**
- `crates/lonko/src/state.rs` (new modal flag)
- `crates/lonko/src/app.rs` (key handler for `i`)
- `crates/lonko/src/ui/remote.rs` (detail panel render)

**What it does:**

Add `host_detail_open: Option<String>` to `AppState` (hostname of the host
whose detail panel is showing; `None` when closed).

In `app.rs` key handler for the Remote tab:
- `i` → toggle `state.host_detail_open` to the currently-selected host's
  hostname (or clear it if it's already that host).
- `q` while panel open → clear `host_detail_open`.
- `U` while panel open → spawn `update_remote` for the host (M2 implements
  the actual command; in M1 just log "update not yet implemented").
- `B` while panel open → placeholder for rebuild-plugin (M2).
- `r` → reset `health.next_probe_tick = 0` on the selected host to trigger
  an immediate re-probe on the next tick.

In `ui/remote.rs`, when `state.host_detail_open == Some(hostname)`, replace
that host's session list with the detail panel widget. The panel is a
`Paragraph` inside a bordered `Block` showing:

```
Binary     <remote_version>  (local <local_version>)  OK / SKEW
Hook cfg   ~/.claude/settings.json                    OK (assumed)
Plugin     dist/index.js                              built / MISSING
Chat sock  <state>
Last event <age>

Actions:  U update   B rebuild-plugin   R reprovision   q close
checked <N>s ago
```

"Hook cfg" is assumed OK if SSH reachability is confirmed (we don't probe
`settings.json` separately — that would require another SSH round-trip and
adds marginal value since `install-remote` already ensures it). Call this out
in a code comment.

**Pushback on Eric's proposal:** Eric's detail panel shows `agent PID` for
the chat socket. We don't have that data; probing it would require another
SSH command. Omit it. `connected / disconnected` from `chat_connected` is
sufficient and is already in-process.

**Depends on:** M1-Step 2, M1-Step 3.

**Test:** Manual TUI check. Assert in a unit test that `host_detail_open` is
cleared when the selected host changes (so stale panels don't show for the
wrong host after navigation).

---

### M1-Step 5 — Tests and manual verification recipe

**Unit tests to add or extend:**
- `state.rs`: `effective_health` permutations (5 cases).
- `state.rs`: `host_last_event_age` with mocked sessions.
- `app/remote.rs`: `on_host_health_probed` with fixture probe results.
- `state.rs`: `host_detail_open` cleared on host selection change.

**Manual verification recipe:**
1. Start lonko on workstation with a live remote peer.
2. Check that the host header shows `?` glyph for ~20 s until probe completes.
3. Verify glyph flips to `●` / `⚠` / `✕` depending on peer state.
4. Kill lonko-channel on the remote; verify `○ chat` appears within one tick.
5. Press `i` on the host row; verify detail panel opens showing correct state.
6. Press `r`; verify probe fires again (check log output `tracing::debug`).
7. Open chat overlay for a remote agent with chat offline; verify banner shows
   and input is disabled.

---

## M2 — Install completeness & peer registry

### Dependency order within M2

```
Step 1 (Q1 resolution — plugin path + MCP registration)
  └─ Step 2 (extend install_remote.rs)
       └─ Step 3 (peer registry store + config.toml [[peer]])
            └─ Step 4 (update-remote subcommand)
                  └─ Step 5 (tests + verification)
```

Step 3 should be done before Step 2 lands in a single merged PR — you need
the registry write to be present before `install-remote` can record to it.
Or: merge Step 3 first, then Step 2.

---

### M2-Step 1 — Q1 resolution: stable plugin path and MCP registration

**The decision:** place the plugin at `~/.claude/lonko-channel/` on the remote.
Register it in `~/.claude/settings.json` under `mcpServers` as a user-level
entry that Claude Code loads regardless of cwd.

**Justification:**

`~/.claude/` is already where Claude Code looks for `settings.json` and session
files. It is the only path Claude Code is guaranteed to read from regardless of
the working directory. Placing the plugin adjacent to `settings.json` keeps the
installation self-contained under a single well-known prefix, and avoids
depending on `$HOME/.config/` or `$XDG_DATA_HOME` conventions that vary across
Linux distributions. The `mcpServers` field in `settings.json` is Claude Code's
documented user-level MCP loading mechanism; using `--dangerously-load-development-channels`
was a local-dev shortcut that is not appropriate for a provisioned remote host.

**Concretely:**
- Remote source lives at `~/.claude/lonko-channel/` (the installer `git clone`s
  or copies the plugin source there, then runs `npm install && npx tsc`).
- `settings.json` gains an entry:
  ```json
  "mcpServers": {
    "lonko": {
      "command": "node",
      "args": ["~/.claude/lonko-channel/dist/index.js"]
    }
  }
  ```
  The installer writes this via the same `read/merge/write` pattern already
  used for hook entries in `install_remote.rs:132-149`.

**No new files.** This step is purely a design decision; the implementation
lives in M2-Step 2.

---

### M2-Step 2 — Extend install_remote.rs

**Files touched:**
- `crates/lonko/src/install_remote.rs`

**What it does:**

Add three new phases after `configure_hooks`:

**Phase 1 — Copy plugin source to remote**
```
ssh host "mkdir -p ~/.claude/lonko-channel"
```
Then `scp -r lonko-channel/src lonko-channel/package.json lonko-channel/tsconfig.json host:~/.claude/lonko-channel/`

Problem: `scp` of the source is fine, but we cannot assume the workstation's
local `lonko-channel/` is at the `install-remote` invocation site. The binary
is installed from a git tag, not from a local checkout. Therefore:

SSH to remote and clone the same git tag used for the binary:
```
ssh host "rm -rf ~/.claude/lonko-channel && \
  git clone --depth 1 --branch v<version> <REPO_URL> /tmp/lonko-channel-src && \
  mkdir -p ~/.claude/lonko-channel && \
  cp -r /tmp/lonko-channel-src/lonko-channel/. ~/.claude/lonko-channel/ && \
  rm -rf /tmp/lonko-channel-src"
```

**Phase 2 — Build the plugin**
```
ssh host "cd ~/.claude/lonko-channel && npm install && npx tsc"
```
Stream output to the user (same as `install_hook_binary` does for cargo).

Add `fn install_plugin(host: &str, version: &str) -> Result<()>` following the
same pattern as `fn install_hook_binary`.

**Phase 3 — Register in settings.json**
Add `fn register_mcp_server(host: &str) -> Result<()>` that reads the remote
`settings.json`, merges `mcpServers.lonko`, and writes it back. Reuse
`read_remote_settings` / `write_remote_settings` already in the file.

Add to `run(host)` before the success message:
```rust
println!("\n==> Installing lonko-channel plugin on {host}...");
install_plugin(host, version)?;
println!("    dist/index.js built");
println!("\n==> Registering MCP server in {host}:~/.claude/settings.json...");
register_mcp_server(host)?;
```

**Depends on:** M2-Step 1 (decision), M2-Step 3 (registry write — see note
below about ordering).

**Note on ordering:** The registry write (Step 3) should be merged first so
this step can call it. Alternatively, include both in the same PR.

**Test:** Unit-test `register_mcp_server`'s JSON merge logic with fixture
settings payloads (no SSH; mock the read/write). Check idempotency (running
twice does not duplicate the `mcpServers.lonko` entry).

---

### M2-Step 3 — Registered peers store

**Files touched:**
- `crates/lonko/src/config.rs` (new `[[peer]]` table + load/save)
- `crates/lonko/src/state.rs` (`AppState` gains `registered_peers`)
- `crates/lonko/src/app.rs` (load peers on startup)

**Schema** — extend `~/.config/lonko/config.toml`:

```toml
[[peer]]
hostname = "kayshon"
provisioned_version = "0.26.0"
plugin_built = true
last_seen = "2026-05-13T18:00:00Z"
```

In `config.rs`, add:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRecord {
    pub hostname: String,
    pub provisioned_version: String,
    pub plugin_built: bool,
    pub last_seen: Option<String>,  // ISO-8601, updated on hook arrival
}
```

Extend `Config` with `pub peers: Vec<PeerRecord>`.

Add `config::save_peer` (upsert by hostname) and `config::load_peers`.

**Important:** do NOT write to `config.toml` on every hook event (that would
thrash the user's file with `last_seen` updates). Instead, write `last_seen`
only when `install-remote` or `update-remote` succeeds, or when the user
explicitly triggers `R` (reprovision). Hook event arrival updates
`RemoteHost::health.checked_at` in memory only.

**AppState extension:**
```rust
pub registered_peers: Vec<crate::config::PeerRecord>,
```
Populated at startup from `config::load()`. Used by `ui/remote.rs` to split
the fleet section (registered, always shown) from the tailnet section
(unregistered, dimmed).

**Pushback on Eric's P6 ("unregistered peers visually distinct"):** For a
solo developer with no registered peers yet (first install), the entire Remote
tab is the "unregistered" section, which is useless noise. The visual split
between registered and unregistered peers only has value when the registry is
non-empty. Add a guard: only render the "unregistered" section if
`registered_peers` is non-empty. When the registry is empty, the existing
tailnet peer list renders as it does today (no dimming, no label). This avoids
confusing first-time users who haven't run `install-remote` yet.

**Depends on:** M2-Step 1.

**Test:** Unit-test round-trip parse of the TOML schema including the `[[peer]]`
array. Test that `save_peer` is idempotent on repeated calls with the same
hostname.

---

### M2-Step 4 — `update-remote` subcommand

**Files touched:**
- `crates/lonko/src/main.rs` (CLI parsing)
- `crates/lonko/src/update_remote.rs` (new file)
- `crates/lonko/src/config.rs` (update `provisioned_version` after success)

**What it does:**

New `update_remote.rs` with `pub fn run(host: Option<&str>) -> Result<()>`:

- If `host` is `Some(h)`: run the update sequence for that host.
- If `host` is `None`: load registered peers, filter to those whose
  `provisioned_version != local_version`, run sequentially.

Update sequence for one host:
1. `cargo install --git <REPO_URL> --tag v<local_version> --locked lonko lonko-hook` (same as install).
2. `cd ~/.claude/lonko-channel && git fetch --depth 1 origin tag v<local_version> && git checkout v<local_version> && npm install && npx tsc`.
3. Call `config::save_peer` to update `provisioned_version`.
4. Send `Event::HostHealthProbed` reset (or just set `next_probe_tick = 0` in
   memory — but `update-remote` is a CLI command, not a TUI action, so it can't
   reach `AppState` directly). Leave the re-probe to the next lonko startup.

Wire into `main.rs` argument parsing. Lonko already dispatches subcommands
in `main.rs`; add `update-remote` alongside `install-remote`.

**TUI binding:** add `U` on the host row in the Remote tab to run
`update-remote <host>` as a subprocess (same as the existing `lonko respond`
dispatch pattern). Show output in a progress overlay if the TUI is open.
However, the subprocess approach means lonko loses the event stream while the
subprocess runs; defer the progress overlay to a follow-on PR and in M2 just
invoke the subprocess and re-probe on completion.

**Pushback on Eric's proposal — `update-remote` with no host flag:** Eric
proposes "updates all registered peers that are out of date." This is
convenient but dangerous: a single slow cargo build on a peer with poor
connectivity blocks the entire fleet update with no partial-success reporting.
Implement `update-remote` (no host) to update peers sequentially and print
per-host success/failure summaries. Do not make it atomic or transactional —
just continue past failures and summarize at the end.

**Depends on:** M2-Step 2, M2-Step 3.

**Test:** Unit-test the "no host" path filters only peers with version skew.
Manual verification: run `lonko update-remote kayshon`, confirm cargo install
runs on the remote and `provisioned_version` updates in `config.toml`.

---

### M2-Step 5 — Tests and manual verification recipe

**Unit tests:**
- `config.rs`: `PeerRecord` TOML round-trip, `save_peer` idempotency.
- `install_remote.rs`: MCP server JSON merge idempotency.
- `update_remote.rs`: no-host filtering logic.

**Integration / manual recipe:**
1. Fresh remote host with no prior install.
2. Run `lonko install-remote <host>`.
3. Verify all steps print OK, `config.toml` gains a `[[peer]]` entry.
4. Verify `~/.claude/lonko-channel/dist/index.js` exists on remote.
5. Verify `~/.claude/settings.json` on remote has `mcpServers.lonko`.
6. Start lonko on workstation; verify the host appears in the fleet section
   (not the unregistered section).
7. Bump local version (or fake it by editing `env!("CARGO_PKG_VERSION")`
   in a test build). Verify host header shows `[↓]` and VersionSkew.
8. Run `lonko update-remote <host>`. Verify `provisioned_version` updates.
9. Restart lonko; verify host is Healthy again.

---

## Cross-cutting notes

### Risk callouts where Eric's proposal fights the existing code

**1. `HostStatus` vs. `HostHealth` — don't maintain two parallel enums.**
The proposal describes `HostHealth` as a "computed value" but then uses the
word "status" throughout. The existing code has `HostStatus` on `RemoteHost`
and all match arms use it. The plan above keeps `HostStatus` for SSH
reachability (it's the output of the existing SSH poll) and adds `HostHealth`
as a separate derived field. This keeps the existing backoff/retry logic
(`app/remote.rs:281-287`) untouched. Do NOT merge them into one enum —
SSH reachability is polled on a different cadence and by a different mechanism
than the version/plugin probe.

**2. Chat socket liveness — do not SSH-poll.**
The proposal mentions `ssh host test -S ~/.claude/lonko-chat.sock` as one of
the health check inputs. This is wrong for this codebase: `chat_online` and
`chat_offline` events already flow through the event channel (`event.rs:73-75`)
and are the authoritative source of plugin connectivity. Adding an SSH poll
would be redundant, slower (one extra SSH handshake per probe), and would
create a race between the event-driven state and the polled state. The plan
uses the event-driven flag exclusively.

**3. `install-remote` TUI flow — defer to post-M2.**
Eric proposes a TUI modal (`I` on an unregistered peer) that mirrors the CLI
output in a scrollable overlay. This is non-trivial: the CLI install can take
several minutes (cargo build), and rendering a live subprocess output stream
in the TUI requires either a dedicated task that pipes stdout back over the
event channel, or blocking the event loop entirely (unacceptable). The
pattern doesn't exist elsewhere in this codebase. Get the CLI working and
battle-tested first. The TUI flow is a nice-to-have that belongs in a third
milestone.

**4. P6 — empty registry guard.**
As noted in M2-Step 3: the "unregistered peers" visual split is useless
noise before any peer is registered. Guard it. This is a divergence from
Eric's proposal and the right call.

**5. `last_seen` write frequency.**
Eric's `Registered Peer` object has a `last_seen` timestamp that implies it
updates on every hook event. Writing `config.toml` on every hook event (which
can fire multiple times per second during an active Claude session) would be
insane — file I/O inside the hot event path. The plan records `last_seen` only
at install/update time. If live `last_seen` is needed for UX purposes, derive
it from `RemoteHost::sessions[*].last_activity` at render time (already in
memory). Do not persist it.
