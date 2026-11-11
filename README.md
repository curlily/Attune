# MyJam

Listen together across music services. Everyone keeps using their own desktop
client (TIDAL or Spotify, more later); MyJam watches play/pause, seeking, skips and
queue edits there and mirrors them to everyone else in the session.

## Workspace

| Crate / folder | What it is |
|---|---|
| `protocol/` (`jam-protocol`) | Provider-independent session protocol: data model, WebSocket messages, pure state transitions, and the per-session coordinator that decides what is accepted. No I/O. |
| `core/` (`jam-core`) | Desktop library: `BridgeController` (CDP bridge into the music client), the JavaScript provider adapters, and `network::SessionClient` (WebSocket client). |
| `server/` (`jam-server`) | The relay: a small in-memory WebSocket server around the coordinator. |
| `src-tauri/`, `src/` | The Tauri desktop app: commands and settings (Rust), widget UI (Svelte 5 + Vite). |

See [BRIDGE_ARCHITECTURE.md](BRIDGE_ARCHITECTURE.md) for the provider bridge, and
[PROVIDER_ADAPTERS.md](PROVIDER_ADAPTERS.md) for what is known about each music app's
internals, open adapter problems and plans (Spotify rework, YouTube Music).

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
npm test
```

`cargo test` includes the coordinator rules, the client against a test-only
WebSocket peer (`core/tests/support`), and the real server driven by the real
client (`server/tests/e2e.rs`). `npm test` runs the mocked TIDAL/Spotify adapter
tests. None of these replace a live test with the installed desktop clients.

## Running the relay server

```sh
cargo run --release -p jam-server
```

It listens on `127.0.0.1:8787` and serves:

- `ws://<host>/ws`: the session WebSocket
- `http://<host>/healthz`: returns `ok`

| Setting | Default |
|---|---|
| `JAM_BIND` or `--bind <addr>` | `127.0.0.1:8787` |
| `JAM_MAX_SESSIONS` | `100` |
| `JAM_MAX_PARTICIPANTS` | `16` |
| `JAM_RECONNECT_GRACE_SECS` | `60` |
| `JAM_JOIN_ATTEMPTS_PER_MINUTE` | `10` (failed joins per IP address) |
| `RUST_LOG` | `info` |

Sessions live in memory: restarting the server ends them, and clients then report
the session as ended. A session also ends when its last participant leaves or its
reconnect grace period expires. Logs never contain join codes, rejoin secrets or
track data.

Behind a reverse proxy every client appears to come from the proxy's address, so
they share one join-attempt limit; raise `JAM_JOIN_ATTEMPTS_PER_MINUTE` there.

### Exposing it with Cloudflare Tunnel

The server speaks plain `ws://` on loopback; a tunnel adds TLS (`wss://`) and a
public hostname without opening ports. Keep `JAM_BIND` on `127.0.0.1`.

Quick test (random URL that changes every start):

```sh
cloudflared tunnel --url http://127.0.0.1:8787
```

Clients then use `wss://<random-name>.trycloudflare.com/ws`.

Stable hostname (needs a domain on Cloudflare):

```sh
cloudflared tunnel login
cloudflared tunnel create myjam
cloudflared tunnel route dns myjam jam.example.com
```

`~/.cloudflared/config.yml`:

```yaml
tunnel: myjam
credentials-file: /path/to/<tunnel-id>.json
ingress:
  - hostname: jam.example.com
    service: http://127.0.0.1:8787
  - service: http_status:404
```

```sh
cloudflared tunnel run myjam
```

Clients then use `wss://jam.example.com/ws`. Cloudflare drops idle WebSockets after
about 100 seconds; the client pings every 30 seconds, so sessions stay open.

## How a session works

- **Join code:** creating a session returns a four-letter code (like `KWPZ`; no
  `I` or `O`). Anyone with the code can join; there are no accounts. Codes are
  short, so the server allows only a few failed join attempts per IP address per
  minute.
- **Rejoin secret:** each participant also gets a private secret. Reconnecting
  (or rejoining after restarting MyJam) uses it to take the same seat back, so
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
