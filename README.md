# Attune

Listen together across music services. Everyone keeps using their own desktop
client (TIDAL, Spotify, YouTube Music or Apple Music); Attune watches play/pause, seeking, skips and
queue edits there and mirrors them to everyone else in the session.

## Workspace

| Crate / folder | What it is |
|---|---|
| `protocol/` (`attune-protocol`) | Provider-independent session protocol: data model, WebSocket messages, pure state transitions, and the per-session coordinator that decides what is accepted. No I/O. |
| `core/` (`attune-core`) | Desktop library: `BridgeController` (CDP bridge into the music client), the JavaScript provider adapters, and `network::SessionClient` (WebSocket client). |
| `server/` (`attune-server`) | The relay: a small in-memory WebSocket server around the coordinator. |
| `src-tauri/`, `src/` | The Tauri desktop app: commands and settings (Rust), widget UI (Svelte 5 + Vite). |

See [BRIDGE_ARCHITECTURE.md](BRIDGE_ARCHITECTURE.md) for the provider bridge, and
[PROVIDER_ADAPTERS.md](PROVIDER_ADAPTERS.md) for what is known about each music app's
internals and open adapter problems.

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
npm test
```

`cargo test` includes the coordinator rules, the client against a test-only
WebSocket peer (`core/tests/support`), and the real server driven by the real
client (`server/tests/e2e.rs`). `npm test` runs the mocked TIDAL, Spotify,
YouTube Music and Apple Music adapter tests. None of these replace a live test with the installed desktop clients.

## Running the relay server

```sh
cargo run --release -p attune-server
```

It listens on `127.0.0.1:8787` and serves:

- `ws://<host>/ws`: the session WebSocket
- `http://<host>/healthz`: returns `ok`

| Setting | Default |
|---|---|
| `ATTUNE_BIND` or `--bind <addr>` | `127.0.0.1:8787` |
| `ATTUNE_MAX_SESSIONS` | `100` |
| `ATTUNE_MAX_PARTICIPANTS` | `16` |
| `ATTUNE_RECONNECT_GRACE_SECS` | `60` |
| `ATTUNE_JOIN_ATTEMPTS_PER_MINUTE` | `10` (failed joins per IP address) |
| `RUST_LOG` | `info` |

Sessions live in memory: restarting the server ends them, and clients then report
the session as ended. A session also ends when its last participant leaves or its
reconnect grace period expires. Logs never contain join codes, rejoin secrets or
track data.

Behind a reverse proxy every client appears to come from the proxy's address, so
they share one join-attempt limit; raise `ATTUNE_JOIN_ATTEMPTS_PER_MINUTE` there.

### Deploying to the relay host

The public relay runs as a systemd **user** service, `attune`, defined in
[`server/deploy/attune.service`](server/deploy/attune.service). No root is needed;
lingering is enabled for its user, so it starts at boot and is restarted if it
crashes.

The relay's address is kept out of the repository. The deploy script reads
`ATTUNE_HOST=user@host` from the environment or from `scripts/deploy.env`
(ignored by git), and the desktop app takes its default server from
`ATTUNE_DEFAULT_SERVER_URL` at build time: a GitHub Actions secret for releases,
and for local builds the ignored `.cargo/config.toml`:

```toml
[env]
ATTUNE_DEFAULT_SERVER_URL = "ws://<relay>:8787/ws"
```

Without it, builds default to a local relay (`ws://127.0.0.1:8787/ws`).

From Git Bash in the repo, after committing:

```sh
scripts/deploy.sh            # deploy HEAD
scripts/deploy.sh rollback   # put the previous binary back
```

The script uploads `git archive HEAD` (only committed files) to `~/attune/src.tar`,
builds `attune-server` on the server (incrementally, so about a minute, while the old
server keeps running), installs the service file, keeps the old binary as
`attune-server.prev`, restarts the service and checks `/healthz`. The deployed commit
is recorded in `~/attune/DEPLOYED_COMMIT`. Restarting ends every live session.

On the server:

```sh
systemctl --user status attune
systemctl --user restart attune
tail -f ~/attune/server.log
```

Any host works the same way; it needs Rust installed
(`rustup`) and, for start at boot, `loginctl enable-linger <user>`.

### Exposing it with Cloudflare Tunnel

The server speaks plain `ws://` on loopback; a tunnel adds TLS (`wss://`) and a
public hostname without opening ports. Keep `ATTUNE_BIND` on `127.0.0.1`.

Quick test (random URL that changes every start):

```sh
cloudflared tunnel --url http://127.0.0.1:8787
```

Clients then use `wss://<random-name>.trycloudflare.com/ws`.

Stable hostname (needs a domain on Cloudflare):

```sh
cloudflared tunnel login
cloudflared tunnel create attune
cloudflared tunnel route dns attune attune.example.com
```

`~/.cloudflared/config.yml`:

```yaml
tunnel: attune
credentials-file: /path/to/<tunnel-id>.json
ingress:
  - hostname: attune.example.com
    service: http://127.0.0.1:8787
  - service: http_status:404
```

```sh
cloudflared tunnel run attune
```

Clients then use `wss://attune.example.com/ws`. Cloudflare drops idle WebSockets after
about 100 seconds; the client pings every 30 seconds, so sessions stay open.

## Using YouTube Music

YouTube Music has no desktop app, so Attune opens it in a window of an installed
browser (Microsoft Edge, Google Chrome or Brave), with a browser profile of its own.
Your normal browser windows and profile are not touched.

To listen with your account, sign in once before connecting:

1. On the connect screen, choose **Sign in first** under YouTube Music.
2. Sign in to your Google account in the window that opens.
3. Close that window.
4. Choose **Open YouTube Music**. Attune remembers the sign-in from now on.

The sign-in needs its own step because Google refuses sign-ins in the window Attune
connects to. If that window was left open, connecting fails with a message asking you
to close it.

Without a subscription YouTube Music plays advertisements. During one, Attune steps
out of the session and rejoins when it is over, so you may come back a little into
the song the others are hearing.

## Using Apple Music

Attune uses Apple Music's web player (music.apple.com), opened the same way as
YouTube Music: in a window of an installed browser, with a profile of its own. The
Windows app from the Microsoft Store can't be connected to.

Choose **Open Apple Music** and sign in with your Apple ID in the window that opens.
Unlike Google, Apple allows signing in there, and the sign-in is remembered.

## How a session works

- **Join code:** creating a session returns a four-letter code (like `KWPZ`; no
  `I` or `O`). Anyone with the code can join; there are no accounts. Codes are
  short, so the server allows only a few failed join attempts per IP address per
  minute.
- **Rejoin secret:** each participant also gets a private secret. Reconnecting
  (or rejoining after restarting Attune) uses it to take the same seat back, so
  knowing the code and someone's client ID isn't enough to take their place.
- **Clock:** clients measure how far their clock is from the server's and how long
  messages take, and use that to place positions correctly: a seek that took
  80 ms to arrive lands 80 ms further into the song.
- **One order for everyone:** the server numbers every accepted change. Clients
  apply changes in that order and fetch a snapshot if they ever miss one.
- **Conflicts:** concurrent playback changes (pause, seek, skip) simply apply. A
  queue edit based on an outdated queue is rejected and undone on that client.
  Nothing is merged.
- **Songs a service doesn't have:** each client reports songs its service can't
  find as soon as they're queued. Others see who is missing what. If nobody
  listening can play the current song, the server skips it.
- **Stepping out:** a participant can stop following (and later resync) without
  leaving; songs they can't play then no longer count.

## v1 limitations

- No persistence; sessions end on server restart.
- Shuffle and repeat are not synchronized (adapters disable them while following).
- Timing is approximate (network and track-loading delay); there is no clock sync.
- Concurrent queue edits are not merged: the later one is undone.
- The desktop session runtime and widget UI are not implemented yet.
