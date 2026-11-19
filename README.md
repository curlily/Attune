# Attune

Listen together across music services. Everyone keeps their own music app
(TIDAL, Spotify, YouTube Music or Apple Music); Attune follows play and pause,
seeking, skips and queue edits there and mirrors them to everyone else in the
session, whichever service they use.

## Download

Attune runs on Windows. Get the installer from
[curlily.github.io/Attune](https://curlily.github.io/Attune/) or the
[releases page](https://github.com/curlily/Attune/releases). Installed copies
offer new versions themselves.

## Getting started

1. Open Attune and connect your music app.
2. **Create a session**, or **join** one with the four-letter code someone shared.
3. Share your code (or an `attune://join/KWPZ` link) with whoever should listen along.

Attune then lives in the tray. Its icon shows the session at a glance: green in
sync, amber catching up, red out of sync, grey stepped out.

## Music apps

### TIDAL and Spotify

Attune connects to the desktop app. If it's already running, Attune restarts it
once so it can connect.

### YouTube Music

YouTube Music has no desktop app, so Attune opens it in a window of an installed
browser (Microsoft Edge, Google Chrome or Brave) with a browser profile of its
own. Your normal browser windows and profile aren't touched.

To listen with your account, sign in once before connecting:

1. On the connect screen, choose **Sign in first** under YouTube Music.
2. Sign in to your Google account in the window that opens, then close it.
3. Choose **Open YouTube Music**. Attune remembers the sign-in from now on.

The sign-in needs its own step because Google refuses sign-ins in the window
Attune connects to. If that window was left open, connecting asks you to close it.

Without a subscription YouTube Music plays ads. During one, Attune steps out of
the session and rejoins when it's over, so you may come back a little into the
song the others are hearing.

### Apple Music

Attune uses Apple Music's web player (music.apple.com), opened the same way as
YouTube Music. The Windows app from the Microsoft Store can't be connected to.

Choose **Open Apple Music** and sign in with your Apple ID in the window that
opens; the sign-in is remembered. Without a subscription (or signed out) Apple
Music plays only previews: Attune then steps out, says so, and follows again once
full songs play.

## Overlay

While you're in a session, a small pill in a corner of the screen tells you what
happens, even over a full-screen game: "Paused by Lily", "Sam joined", "Stepped
out: ad". It never takes focus and clicks go straight through it.

- **Flash** (default) shows each event for a moment. **Persistent** keeps a small
  status pill up for the whole session, showing what's playing between events.
- A new event replaces the one on screen at once; nothing waits or piles up.
- Optional song cards show the new song's name and cover when it changes.
- With the **Match** theme it takes the music app's colours.

Settings → **Overlay** sets when it shows (only over full-screen apps, whenever
Attune is hidden, or always), the corner, how long events stay up, which events to
show, and has a test button. It never covers Attune's own window.

## Discord

While a music app is connected and the Discord desktop app is running, your
profile shows **Listening to** your music app and the song, with its artists,
cover and a progress bar. Turn it off in Settings (**Show on Discord**).

**Listen along button** (off by default) adds a button to that status while
you're in a session. It opens the download page with the session's code, which
starts Attune and joins, or offers the download to anyone who doesn't have it.
Anyone who can see your status can then join, so it stays off unless you turn it on.

Attune talks to the Discord app on your computer, as the "Attune" application.
No Discord account is linked, and without Listen along the join code is never
part of your status.

## Settings worth knowing

- **Start with Windows**, and **Start your music app too**: Attune opens your
  last music app and connects to it when Windows starts.
- **Allow others to change what's playing** (on by default): counts while you
  host. When it's off, only the host changes playback and the queue.
- **Remove songs not everyone has**: drops queued songs someone in the session
  can't play.
- **Theme**: Attune's colours, or **Match** your music app's.

## Bug reports

Settings → **Save log** writes what happened since Attune started to
`Downloads\Attune log <date> <time>.txt` and shows it in Explorer: what you did,
what the music app reported, every change the session made or received, sync
states, reconnects and every error you saw. The log lives in memory (up to
16 MB) and is saved only when you ask. It has song titles, names and the join
code, but no rejoin secrets or server address.

## How a session works

- **Join code:** four letters (like `KWPZ`; no `I` or `O`). Anyone with the code
  can join; there are no accounts. Codes are short, so the server allows only a
  few failed join attempts per IP address per minute.
- **Rejoin secret:** each participant also gets a private secret. Reconnecting,
  or rejoining after restarting Attune, uses it to take the same seat back, so
  knowing the code and someone's client ID isn't enough to take their place.
- **Clock:** clients measure how far their clock is from the server's and how
  long messages take, so a seek that took 80 ms to arrive lands 80 ms further
  into the song.
- **One order for everyone:** the server numbers every accepted change. Clients
  apply changes in that order and fetch a snapshot if they ever miss one.
- **Conflicts:** playback changes (pause, seek, skip) simply apply. A queue edit
  based on an outdated queue is undone on the client that made it.
- **Songs a service doesn't have:** each client reports songs its service can't
  find as soon as they're queued, and everyone sees who is missing what. If
  nobody listening can play the current song, the server skips it.
- **Host:** whoever creates the session hosts it; if they leave, whoever has been
  in it longest takes over. The host's **Allow others to change what's playing**
  decides whether everyone else may change playback and the queue.
- **Stepping out:** stop following (and resync later) without leaving. Songs you
  can't play then no longer count.

## Limitations

- Sessions live in the server's memory: restarting the server ends them.
- The music apps' own shuffle and repeat stay off during a session. Shuffling
  reorders the shared queue like any queue edit. Repeat is the session's and
  covers one song; there's no "repeat all" yet.
- Concurrent queue edits aren't merged: the later one is undone.
- Timing depends on the network and on how fast each app loads a song, so
  listeners can be a little apart.

---

## Development

| Crate / folder | What it is |
|---|---|
| `protocol/` (`attune-protocol`) | The session protocol: data model, WebSocket messages, pure state transitions, and the coordinator that decides what's accepted. No I/O. |
| `core/` (`attune-core`) | Desktop library: the CDP bridge into music apps (`BridgeController`), the JavaScript provider adapters, the WebSocket client and the session runtime. |
| `server/` (`attune-server`) | The relay: a small in-memory WebSocket server around the coordinator. |
| `src-tauri/`, `src/` | The Tauri desktop app: commands, settings, tray, overlay and Discord (Rust); the UI (Svelte 5 + Vite). |

[BRIDGE_ARCHITECTURE.md](BRIDGE_ARCHITECTURE.md) explains the provider bridge,
[PROVIDER_ADAPTERS.md](PROVIDER_ADAPTERS.md) what's known about each music app's
internals, and [LIVE_TESTS.md](LIVE_TESTS.md) the tests against real music apps.

### Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
npm test
npm run check
```

`cargo test` covers the coordinator rules, the client against a test WebSocket
peer, the session runtime with fake music apps, and the real server driven by the
real client (`server/tests/e2e.rs`). `npm test` runs the adapter tests against
mocked music apps, and `npm run check` type-checks the UI. None of these replace a
live test with the real apps. CI runs them on every push to master.

To check the Discord status against a running Discord app:

```sh
cargo test -p attune --lib discord::live -- --ignored --nocapture
```

### Running the relay

```sh
cargo run --release -p attune-server
```

It serves `ws://<host>/ws` (sessions) and `http://<host>/healthz` (returns `ok`).

| Setting | Default |
|---|---|
| `ATTUNE_BIND` or `--bind <addr>` | `127.0.0.1:8787` |
| `ATTUNE_MAX_SESSIONS` | `100` |
| `ATTUNE_MAX_PARTICIPANTS` | `16` |
| `ATTUNE_RECONNECT_GRACE_SECS` | `60` |
| `ATTUNE_JOIN_ATTEMPTS_PER_MINUTE` | `10` (failed joins per IP address) |
| `RUST_LOG` | `info` |

A session ends when its last participant leaves, its reconnect grace period
expires, or the server restarts. Logs never contain join codes, rejoin secrets or
track data. Behind a reverse proxy every client shares the proxy's address and so
one join-attempt limit; raise `ATTUNE_JOIN_ATTEMPTS_PER_MINUTE` there.

The desktop app's default relay comes from `ATTUNE_DEFAULT_SERVER_URL` at build
time: an Actions secret for releases, and for local builds the git-ignored
`.cargo/config.toml`:

```toml
[env]
ATTUNE_DEFAULT_SERVER_URL = "ws://<relay>:8787/ws"
```

Without it, builds use a local relay (`ws://127.0.0.1:8787/ws`).

#### Deploying

The public relay runs as a systemd user service, `attune`
([`server/deploy/attune.service`](server/deploy/attune.service)), with lingering
enabled so it starts at boot and restarts after a crash. Its address stays out of
the repository: the deploy script reads `ATTUNE_HOST=user@host` from the
environment or the git-ignored `scripts/deploy.env`. From Git Bash, after
committing:

```sh
scripts/deploy.sh            # deploy HEAD
scripts/deploy.sh rollback   # put the previous binary back
```

It uploads the committed files, builds `attune-server` on the host while the old
one keeps running, installs the service file, keeps the old binary as
`attune-server.prev`, restarts the service and checks `/healthz`. The deployed
commit is recorded in `~/attune/DEPLOYED_COMMIT`. Restarting ends every session.

```sh
systemctl --user status attune
tail -f ~/attune/server.log
```

Any host with Rust (`rustup`) works; for start at boot, run
`loginctl enable-linger <user>`.

#### TLS with Cloudflare Tunnel

The server speaks plain `ws://`. A tunnel adds TLS (`wss://`) and a public
hostname without opening ports; keep `ATTUNE_BIND` on `127.0.0.1` then.

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

Run it with `cloudflared tunnel run attune`; clients use
`wss://attune.example.com/ws`. For a quick test without a domain,
`cloudflared tunnel --url http://127.0.0.1:8787` gives a temporary
`wss://<random-name>.trycloudflare.com/ws`. Cloudflare drops idle WebSockets
after about 100 seconds; the client pings every 30, so sessions stay open.

### Releases

From Git Bash on a clean, up-to-date master:

```sh
scripts/release.sh 0.2.0
```

It sets the version in `tauri.conf.json`, `src-tauri/Cargo.toml` and
`package.json`, commits, opens your editor for the tag message (the first line is
the title, the rest the notes shown in the update offer) and pushes master and the
tag. [`release.yml`](.github/workflows/release.yml) then builds, signs and
publishes the NSIS installer.

Installed copies check for a new release a few seconds after starting and offer
**Update and restart**, **Later** or **Skip this version**; nothing is downloaded
before the user chooses. Updates are signed, and the app refuses one that doesn't
match the public key in `tauri.conf.json`.

The repository needs these Actions secrets:

| Secret | What |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | The update signing key (`npx tauri signer generate`) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Its password (empty if it has none) |
| `ATTUNE_DEFAULT_SERVER_URL` | The relay release builds connect to by default |

Keep a backup of the signing key: without it, installed copies can't update and
would have to install the next version by hand.

The installer's sidebar and header images are HTML in
[`src-tauri/installer/`](src-tauri/installer/), rendered to the BMPs NSIS needs by
`scripts/render-installer-images.sh`; commit the BMPs after changing the HTML.

The download page is the static site in [`site/`](site/), published by
[`pages.yml`](.github/workflows/pages.yml) when it changes. Its download button
finds the latest installer through the GitHub API, falling back to the releases page.
