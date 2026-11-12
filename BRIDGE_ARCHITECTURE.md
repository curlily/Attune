# Attune provider bridge architecture

## Separation of responsibilities

Rust owns the CDP connection, bridge lifecycle, JSON transport, and event delivery. Each music provider owns its renderer-specific discovery, metadata lookup, queue representation, and playback actions in its adapter script.

`BridgeController` does not branch on `ProviderId` to perform provider actions. It receives a `ProviderConfig` containing the executable path, page URL, dedicated CDP port, and JavaScript adapter source.

## CDP lifecycle

`BridgeController::initialise` performs the following steps:

1. Checks the provider's configured loopback CDP port for a matching page target.
2. If the target is unavailable, terminates the configured client process and launches it with `--remote-debugging-port=<port>`. A provider that is a page in a browser (`ProviderConfig::browser`) is never terminated: its page is opened as an app window in a browser profile of its own.
3. Waits for the matching target to appear in `/json/list`.
4. Connects with `chromiumoxide::Browser::connect` and runs the CDP handler stream in a Tokio task.
5. Finds the page target by the configured URL and attaches a `Page` handle.
6. Enables the Page, Runtime, and Log CDP domains; subscribes to binding, console, exception, and log events.
7. Registers the `__jamEvent` CDP binding and the adapter through `evaluate_on_new_document`.
8. Issues a raw CDP page reload so the adapter executes at document start, then waits for its `Ready` event and verifies bridge version 1. If no navigation starts within 2 s (TIDAL sometimes ignores `Page.reload`), it reloads from the page with `location.reload()`. A "Leave site?" dialog the page raises against the reload is confirmed.

Calls into the page are evaluated as user gestures, because browsers refuse to start playback from a script otherwise.

The Browser, Page, handler task, and event-forwarding task are retained by the controller. `dispose` calls the adapter's cleanup method and aborts both tasks; it does not terminate the music application.

TIDAL uses port `9222`, Spotify `9223` and YouTube Music `9224`. These values are provider configuration, not provider logic in the controller. Keeping them distinct allows both desktop clients to remain open without sharing one Chromium debugging listener.

## Provider bridge contract v1

Each adapter installs `window.__ATTUNE_BRIDGE_V1__` and the compatibility alias `window.bridge` with these methods:

- `version`: numeric protocol version (`1`)
- `getState()`: returns `SessionMusicState`
- `applySnapshot(state)`: applies a canonical session state; resolves to `{ unavailableEntryIds }`
- `applyOperation(operation)`: applies one provider-independent operation; resolves to `{ unavailableEntryIds }`
- `dispose()`: removes listeners and adapter state

The controller invokes these methods through CDP. Calls return an explicit success/error envelope so JavaScript exceptions and rejected promises are not mistaken for a successful `undefined` return.

### Tracks a provider cannot find

Each queue entry is looked up separately. A lookup failure (no match, search timeout, metadata that can't be loaded) affects only that entry:

- **Upcoming entry not found:** it is left out of the provider's queue, everything else is loaded, and its ID is returned in `unavailableEntryIds`. It stays in the session for everyone else.
- **Current entry not found:** nothing is changed and the adapter throws an error with `code: "trackUnavailable"` and `details: { entryIds }` (every unfound entry). Rust maps it to `Error::TrackUnavailable`.

Every entry is resolved before the provider's queue is touched. Other failures (renderer internals missing, search unavailable) are still thrown as plain errors and become `Error::BridgeCallFailed`.

An error may carry a string `code` and JSON `details`; the envelope passes both to Rust. Codes are the JS→Rust equivalent of error variants, and unknown codes are treated as plain failures.

Renderer events are sent through the CDP binding as JSON:

```json
{"type":"Operation","data":{"type":"playback.setPlaying","isPlaying":false}}
```

`BridgeController::subscribe()` exposes `Ready`, `Error`, legacy playback/queue events, provider-independent `Operation` events, and `Interruption` events through a Tokio broadcast channel. An interruption (`{ active, reason }`) means the provider can't follow for a while, e.g. during an advertisement: the session runtime steps out and rejoins when it is over. A future session/network component should subscribe here; it should not know about TIDAL Redux actions or Spotify URIs.

## Shared session state

The Rust types in `core/src/protocol.rs` define the wire contract. Queue entries have independent `entryId` values, so duplicate recordings remain distinct. Queue order is current entry first, followed by upcoming items; playback history is excluded. Provider IDs remain inside their adapter.

The operation set is absolute rather than toggle-based:

- `queue.replace`, `queue.insert`, `queue.remove`, `queue.move`
- `playback.select`, `playback.advance`, `playback.seek`, `playback.setPlaying`

Since protocol 2, `queue.replace` may carry `playback: { positionMs, isPlaying }` for its new current song (a provider may resume a list part-way); without it the new song starts at 0, playing. Adapters start the song there.

Serde serializes the operation variant as `type` and all variant fields in `camelCase`, matching both JavaScript adapters. `SessionSnapshot` includes the protocol version and session revision metadata; adapters receive only its `state` after Rust validates the protocol version (2).

The bridge layer does not implement session networking, server sequencing, conflict resolution, voting, or reconnect/replay policy. Those belong above `BridgeController`. The current Tauri commands expose connect/disconnect, read state, and apply snapshot/operation primitives.

## TIDAL adapter notes

Everything learned about the providers' behaviour, open problems, the Spotify rework checklist and YouTube Music plans are collected in [PROVIDER_ADAPTERS.md](PROVIDER_ADAPTERS.md).

The adapter is self-contained because it is injected as one renderer script. It discovers the Redux store at runtime by scanning loaded script modules for the semantic `No global store set` accessor signature, rather than hard-coding minified bundle names. It validates the discovered store methods before using it.

It also finds two optional helpers among the module exports: TIDAL's open-API client (an RTK Query API object, by its `reducerPath`) and the player SDK's position getter (by its code). Without them the adapter falls back to text search and the store's position.

The current implementation uses these private TIDAL application interfaces (all verified live against TIDAL 2.48, details in [PROVIDER_ADAPTERS.md](PROVIDER_ADAPTERS.md)):

- `playQueue/RESET` to write the whole queue with uids of our own, which carry the session entry IDs. The player loads the new current track paused.
- `playQueue/CLEAR_UPCOMING` and `APPEND_ELEMENTS` to replace everything after a current song that stays, without touching playback.
- `playQueue/CLEAR_QUEUE` to unload the current track for an empty session queue.
- `playQueue/ADD_AT_INDEX`, `REMOVE_ELEMENT` and `MOVE_TRACK` for single upcoming-queue edits.
- `playbackControls/PLAY`, `PAUSE`, `SEEK` and `TIME_UPDATE` (store only), plus `playbackControls.prefilled` to know when a loaded track can be sought.
- The open API's `getTracks` with `filter[isrc]` (20 per request) to find recordings by ISRC, and `content/LOAD_SINGLE_MEDIA_ITEM` to give TIDAL their metadata.
- `search/SEARCH_FOR`, with results at `state.content.trackLists["search/tracks/" + encodeURIComponent(query)]` and metadata in `state.content.mediaItems`, for songs without an ISRC hit.
- `playQueue.currentIndex` to discard TIDAL's played-history prefix when normalizing its queue, and `playQueue.type` to refuse writes while TIDAL plays on another device.

These are undocumented application internals, not a supported TIDAL API. Runtime feature checks and explicit errors reduce the chance of silent corruption, but cannot guarantee compatibility with future TIDAL releases. An exact ISRC identifies the recording; otherwise matching requires normalized title (featured artists ignored) and artist agreement, conflicting ISRCs are rejected, and album/duration are tie-breakers. All recordings are resolved before the queue is replaced.

Waits in the adapter react to store changes instead of polling, because TIDAL's page runs timers only once a second while its window is hidden and silent.

## Spotify adapter notes

`core/src/adapters/spotify.js` finds Spotify's platform object on a React fiber prop and takes `PlayerAPI`, `SettingsAPI` and `GraphQLLoader` from its service registry. It depends on these private interfaces (all verified live, details in [PROVIDER_ADAPTERS.md](PROVIDER_ADAPTERS.md)):

- `PlayerAPI.getState()` and its `update` / `queue_update` events for observing playback.
- `PlayerAPI._contextPlayer.play` to start a track at a position, playing or paused, in one request.
- `PlayerAPI._contextPlayer.setQueue` to write the whole upcoming list against a revision. Every queue edit goes through it, so edits never restart the current track.
- `PlayerAPI._queue.getInternalQueue()` for the full queue (the state lists at most 50 upcoming items).
- `PlayerAPI._contextPlayer.stop` to unload the current track for an empty session queue.
- The `searchTracks` GraphQL query, including `isrc:` search terms.
- `SettingsAPI.autoPlay`, `PlayerAPI.setShuffle` and `setRepeat` for the neutral state.

The session queue is the current track plus Spotify's queued and playlist ("context") items. Recommendations ("autoplay") are left out, and autoplay is switched off during a session.

## YouTube Music adapter notes

`core/src/adapters/youtube_music.js` runs in music.youtube.com, opened in a Chromium browser. The page's script names are generated and its code is minified, so the adapter depends on neither. It reaches everything from the `ytmusic-player-bar` and `ytmusic-app` elements (all verified live, details and the one-time sign-in in [PROVIDER_ADAPTERS.md](PROVIDER_ADAPTERS.md)):

- The app's Redux store for the queue, by action name: `UPDATE_ITEMS` (replace everything after the current item), `CLEAR`, `ADD_ITEMS`, and `SET_AUTOPLAY_ENABLED` / `SET_REPEAT` for the neutral state.
- The queue service's `selectQueueItem` to make an item current and start it.
- The YouTube player API (`playVideo`, `pauseVideo`, `seekTo`, `getCurrentTime`, `getPlayerState`).
- The app's network manager for song search (`/search`) and for queue items by video ID (`/music/get_queue`).
- `player.adPlaying` in the store, reported as an `Interruption`.

## Validation

Run:

```sh
npm test
```

This runs the mocked TIDAL, Spotify and YouTube Music adapter tests (shared API, skipping tracks the provider can't find, the structured `trackUnavailable` error, paused joins, queue edits that keep the song playing, and that remote applies are not echoed as local operations) plus JavaScript syntax checks for the scripts. The mocks stand in for the providers' private renderer interfaces, so they do not replace a live test against the installed desktop clients. Run `cargo test --workspace` for the Rust side.
