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

CI runs these on every push to master ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

## Releases and updates

Releases are Windows NSIS installers on the
[GitHub releases page](https://github.com/curlily/Attune/releases), built by
[`.github/workflows/release.yml`](.github/workflows/release.yml) when a `v*.*.*` tag
is pushed. To release, from Git Bash on a clean master:

```sh
scripts/release.sh 0.2.0
```

It sets the version in `tauri.conf.json`, `src-tauri/Cargo.toml` and `package.json`,
commits, opens your editor for the tag message (first line a title, the rest the
release notes shown in the app) and pushes master and the tag.

Installed copies check `latest.json` on the latest release a few seconds after
launch (not in development builds) and offer the update: **Update and restart**,
**Later**, or **Skip this version**. Nothing is downloaded until the user chooses
to update. Settings has **Check for updates**, which also shows skipped versions.
Updates are signed. The app refuses one that doesn't match the public key in
`tauri.conf.json`.

The installer's sidebar and header images are drawn as HTML in
[`src-tauri/installer/`](src-tauri/installer/) and rendered to the BMPs NSIS needs
(at twice its sizes, so they stay sharp on high-DPI screens) with
`scripts/render-installer-images.sh`. Commit the BMPs after changing the HTML.

The download page, [curlily.github.io/Attune](https://curlily.github.io/Attune/), is
the static site in [`site/`](site/), published by
[`.github/workflows/pages.yml`](.github/workflows/pages.yml) when it changes. Its
download button looks up the latest release's installer through the GitHub API and
falls back to the releases page.

The repository needs these Actions secrets:

| Secret | What |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | Contents of the update signing key (`npx tauri signer generate`) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Its password (empty if it has none) |
| `ATTUNE_DEFAULT_SERVER_URL` | The relay release builds connect to by default |

If the signing key is lost, installed copies can't update anymore. They'd have
to install the next version by hand. Keep a backup of it.

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

Without a subscription (or signed out) Apple Music plays only previews. Attune then
steps out of the session, saying so, and follows again once Apple Music plays full songs.

## Using Apple Music

Attune uses Apple Music's web player (music.apple.com), opened the same way as
YouTube Music: in a window of an installed browser, with a profile of its own. The
Windows app from the Microsoft Store can't be connected to.

Choose **Open Apple Music** and sign in with your Apple ID in the window that opens.
Unlike Google, Apple allows signing in there, and the sign-in is remembered.

## Discord

While you're in a session and the Discord desktop app is running, your Discord
profile shows **Listening to** the session's song, with its artists, cover, a
and a progress bar. It goes away when you step
out, can't play the song, or leave. Turn it off in Settings (**Show on Discord**).

Friends can join from Discord too. Your status offers them **Ask to Join**, and
Attune shows their request with **Accept** / **Decline**. You can also invite
someone from a DM (the **+** button). Accepting opens Attune for them, starting it
through the `discord-1570918885920604200://` link the installer registers, and
joins your session. Joining needs Attune installed, and a relay new enough to
issue invites: each session gets a random invite secret and party ID, so Discord
never sees the join code.

Attune talks to the Discord app on your computer over its local connection
(`src-tauri/src/discord/`), as the "Attune" application (ID `1570918885920604200`).
No Discord account is linked, and the join code is never part of the activity.
To check against a running Discord client:

```sh
cargo test -p attune --lib discord::live -- --ignored --nocapture
```

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
- **Host:** whoever creates the session hosts it; if they leave, whoever has been
  in it longest takes over. The host's **Allow others to change what's playing**
  setting (on by default) decides whether everyone else may change playback and
  the queue. When it's off, the server refuses others' changes and their app puts
  the session's state back, except moving on when a song has ended. A new host
  starts with it on until their own setting arrives.
- **Stepping out:** a participant can stop following (and later resync) without
  leaving; songs they can't play then no longer count.

## v1 limitations

- No persistence; sessions end on server restart.
- Shuffle and repeat: the music apps' own modes stay off during a session. Shuffling
  reorders the queue, which is shared like any queue edit (where the app's shuffle
  doesn't reorder a session's songs, as in Spotify and YouTube Music, Attune shuffles
  them). Repeat is the session's: one song
  (no "repeat all" yet), toggled from the icon by the current song or the music
  app's repeat button.
- Timing is approximate (network and track-loading delay); there is no clock sync.
- Concurrent queue edits are not merged: the later one is undone.
- The desktop session runtime and widget UI are not implemented yet.
