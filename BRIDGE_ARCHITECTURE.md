# MyJam provider bridge architecture

## Separation of responsibilities

Rust owns the CDP connection, bridge lifecycle, JSON transport, and event delivery. Each music provider owns its renderer-specific discovery, metadata lookup, queue representation, and playback actions in its adapter script.

`BridgeController` does not branch on `ProviderId` to perform provider actions. It receives a `ProviderConfig` containing the executable path, page URL, dedicated CDP port, and JavaScript adapter source.

## CDP lifecycle

`BridgeController::initialise` performs the following steps:

1. Checks the provider's configured loopback CDP port for a matching page target.
2. If the target is unavailable, terminates the configured client process and launches it with `--remote-debugging-port=<port>`.
3. Waits for the matching target to appear in `/json/list`.
4. Connects with `chromiumoxide::Browser::connect` and runs the CDP handler stream in a Tokio task.
5. Finds the page target by the configured URL and attaches a `Page` handle.
6. Enables the Page, Runtime, and Log CDP domains; subscribes to binding, console, exception, and log events.
7. Registers the `__jamEvent` CDP binding and the adapter through `evaluate_on_new_document`.
8. Issues a raw CDP page reload so the adapter executes at document start, then waits for its `Ready` event and verifies protocol version 1.

The Browser, Page, handler task, and event-forwarding task are retained by the controller. `dispose` calls the adapter's cleanup method and aborts both tasks; it does not terminate the music application.

TIDAL uses port `9222`; Spotify uses `9223`. These values are provider configuration, not provider logic in the controller. Keeping them distinct allows both desktop clients to remain open without sharing one Chromium debugging listener.

## Provider bridge contract v1

Each adapter installs `window.__MYJAM_BRIDGE_V1__` and the compatibility alias `window.bridge` with these methods:

- `version`: numeric protocol version (`1`)
- `getState()`: returns `SessionMusicState`
- `applySnapshot(state)`: applies a canonical session state; resolves to `{ unavailableEntryIds }`
- `applyOperation(operation)`: applies one provider-independent operation; resolves to `{ unavailableEntryIds }`
- `dispose()`: removes listeners and adapter state

The controller invokes these methods through CDP. Calls return an explicit success/error envelope so JavaScript exceptions and rejected promises are not mistaken for a successful `undefined` return.

### Tracks a provider cannot find

Each queue entry is looked up separately. A lookup failure (no match, ambiguous match, search timeout) affects only that entry:

- **Upcoming entry not found:** it is left out of the provider's queue, everything else is loaded, and its ID is returned in `unavailableEntryIds`. It stays in the session for everyone else.
- **Current entry not found:** nothing is changed and the adapter throws an error with `code: "trackUnavailable"` and `details: { entryIds }` (every unfound entry). Rust maps it to `Error::TrackUnavailable`.

Every entry is resolved before the provider's queue is touched. Other failures (renderer internals missing, search unavailable) are still thrown as plain errors and become `Error::BridgeCallFailed`.

An error may carry a string `code` and JSON `details`; the envelope passes both to Rust. Codes are the JS→Rust equivalent of error variants, and unknown codes are treated as plain failures.

Renderer events are sent through the CDP binding as JSON:

```json
{"type":"Operation","data":{"type":"playback.setPlaying","isPlaying":false}}
```

`BridgeController::subscribe()` exposes `Ready`, `Error`, legacy playback/queue events, and provider-independent `Operation` events through a Tokio broadcast channel. A future session/network component should subscribe here; it should not know about TIDAL Redux actions or Spotify URIs.

## Shared session state

The Rust types in `core/src/protocol.rs` define the wire contract. Queue entries have independent `entryId` values, so duplicate recordings remain distinct. Queue order is current entry first, followed by upcoming items; playback history is excluded. Provider IDs remain inside their adapter.

The operation set is absolute rather than toggle-based:

- `queue.replace`, `queue.insert`, `queue.remove`, `queue.move`
- `playback.select`, `playback.advance`, `playback.seek`, `playback.setPlaying`

Serde serializes the operation variant as `type` and all variant fields in `camelCase`, matching both JavaScript adapters. `SessionSnapshot` includes the protocol version and session revision metadata; adapters receive only its `state` after Rust validates version 1.

The bridge layer does not implement session networking, server sequencing, conflict resolution, voting, or reconnect/replay policy. Those belong above `BridgeController`. The current Tauri commands expose connect/disconnect, read state, and apply snapshot/operation primitives.

## TIDAL adapter notes

Everything learned about the providers' behaviour, open problems, the Spotify rework checklist and YouTube Music plans are collected in [PROVIDER_ADAPTERS.md](PROVIDER_ADAPTERS.md).

The adapter is self-contained because it is injected as one renderer script. It discovers the Redux store at runtime by scanning loaded script modules for the semantic `No global store set` accessor signature, rather than hard-coding minified bundle names. It validates the discovered store methods before using it.

The current implementation uses these private TIDAL application interfaces observed and tested during reverse engineering:

- `search/SEARCH_FOR`, with results at `state.content.trackLists["search/tracks/" + encodeURIComponent(query)]` and metadata in `state.content.mediaItems`.
- `content/FETCH_AND_PLAY_MEDIA_ITEM` to play a recording that has not previously been loaded.
- `playQueue/ADD_MEDIA_ITEMS_TO_QUEUE` with `overwritePlayQueue: true` to replace the queue.
- `playbackControls/PLAY`, `PAUSE`, `SEEK`, and `SKIP_NEXT` for playback controls.
- `playQueue.currentIndex` to discard TIDAL's played-history prefix when normalizing its queue.

These are undocumented application internals, not a supported TIDAL API. Runtime feature checks and explicit errors reduce the chance of silent corruption, but cannot guarantee compatibility with future TIDAL releases. Track matching requires normalized title and artist agreement; conflicting ISRCs are rejected, exact ISRCs are preferred, and album/duration are tie-breakers. All recordings are resolved before a destructive queue replacement.

An important deliberate limitation: a TIDAL action that safely unloads an active track to produce a truly empty queue has not been verified. When an empty state is requested while a track is still active, the adapter reports an error instead of dispatching an assumed `CLEAR_QUEUE` action.

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

## Validation

Run:

```sh
npm test
```

This runs the mocked TIDAL and Spotify adapter tests (shared API, skipping tracks the provider can't find, the structured `trackUnavailable` error, paused joins, queue edits that keep the song playing, and that remote applies are not echoed as local operations) plus JavaScript syntax checks for both scripts. The mocks stand in for the providers' private renderer interfaces, so they do not replace a live test against the installed desktop clients. Run `cargo test --workspace` for the Rust side.
