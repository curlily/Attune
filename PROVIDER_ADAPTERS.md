# Provider adapters: known behaviour, problems and plans

MyJam controls music apps through their *private* internals (TIDAL's Redux store,
Spotify's PlayerAPI), injected over the Chrome DevTools Protocol. These internals
are undocumented and full of timing quirks. This file collects everything learned
about them, what is still broken or unverified, and how to make adapters robust,
so the next adapter (Spotify rework, YouTube Music) starts from what we know.

How things are marked:

- **Verified live**: observed in the real app.
- **Mock only**: covered by the mocked tests in `core/tests/*.test.mjs` but not
  confirmed in the real app.
- **Assumed**: inferred or taken from public API formats, not yet confirmed.

Architecture and the bridge contract are in [BRIDGE_ARCHITECTURE.md](BRIDGE_ARCHITECTURE.md).

---

## 1. Shared lessons (all Chromium-based providers)

| Topic | What happens | Status | Where it's handled |
|---|---|---|---|
| Worker auto-attach | chromiumoxide auto-attaches to the page's workers with "wait for debugger on start". After our reload, TIDAL's renderer froze for about 2 minutes, so the bridge never became ready. A raw CDP reload was ready in 0.3 s. | Verified live, fixed | `install_bridge` turns auto-attach off on the page |
| Hung reload fallback | If the bridge still isn't ready after 10 s, drop the CDP connection (which lets the page finish) and attach again once | Verified live (before the fix above) | `BridgeController::initialise` |
| Attaching reloads the page | The adapter must run before the app's own scripts (store discovery), so attaching reloads the app. Playback is interrupted once. | Verified live | Runtime settles before using state |
| After the reload | TIDAL first reports an empty queue, then restores its queue, then resumes playback by itself. All three look like local changes. | Verified live | `settled_state` waits until queue, current song and play state are stable |
| Entry IDs | Entry-ID mapping lives in the page, so a bridge reload mid-session gives new IDs | Verified live | Resync from a snapshot after re-injection |
| Echoes | Applying a remote change makes the app emit store changes that look like local edits | Verified live | 300 ms adapter suppression window plus runtime echo check (`state::is_noop` after translation). 1.2 s was too long: it swallowed real song-end advances. |
| Restarting the app | Killing a client that is merely mid-reload (port open, page missing) closed the user's app for nothing | Verified live, fixed | Only restart when the debug port is closed; the UI asks first |
| Child process stdio | A launched client inheriting our stdio kept our pipes open after exit | Verified live, fixed | Launch with null stdio |
| Microsoft Store installs | Packaged apps live under `WindowsApps` and are launched through aliases. Passing the debug port is untested. | Assumed | Detected and reported as unsupported |
| chromiumoxide noise | Logs warnings for CDP messages it can't parse from newer Chromium | Verified live | Log filter `chromiumoxide::handler=error` |

## 2. TIDAL

### Verified behaviour

**Store and actions**
- Store discovery scans loaded modules for the `No global store set` accessor.
- Action types are written as template literals in the bundle, e.g. `` A(`playQueue/…`) ``.
- `playQueue.type` is `"local"` for normal queues.
- `playQueue.currentIndex` marks the current element; elements before it are history.

**Queue edits that keep the current track playing**
- `playQueue/ADD_AT_INDEX {context, index, mediaItemIds}`: `index` is absolute, history included.
- `playQueue/REMOVE_ELEMENT {uid}`: not for the current element.
- `playQueue/MOVE_TRACK {fromIndex, toIndex}`: inserts and then removes, so forward moves need `target + 1`. It may give the element a new uid.
- `playQueue/ADD_MEDIA_ITEMS_TO_QUEUE` with `overwritePlayQueue: true, position: "now"`
  replaces the queue but **restarts the current track**. Use it only when a full
  replacement is unavoidable.

**Search**
- Only the latest `search/SEARCH_FOR` survives: a new search cancels one still in flight. Searches must run one at a time.
- Results are at `content.trackLists["search/tracks/" + encodeURIComponent(query)].sorted.defaultSort.items`.
- An empty result is `items: []` with `loading: false`. Anything else that's empty is still loading.

**Play state**
- A pause is `playbackState: "NOT_PLAYING"` (`"PAUSED"` hasn't been seen).
- `playbackState` leaves `"PLAYING"` while a track loads.
- `desiredPlaybackState` stays `"PLAYING"` while loading *and* after a PAUSE action, so neither field alone is reliable. See `isPlaying()`.
- After switching tracks, the **previous track's `PLAYING` lingers** for a moment, so "same track and PLAYING" doesn't mean the new track has started.

**Position**
- `latestCurrentTime` is in seconds and valid *at* `latestCurrentTimeSyncTimestamp` (epoch ms).
- It is updated only on play, pause, seek and track changes, so reading it later needs extrapolation.
- When a track actually starts, TIDAL resets the position to 0 (sometimes about 3 s after the request). This is not a seek: the adapter ignores it for 10 s after a track change.

**Loading a queue**
- A queue loaded with `position: "now"` starts its first track **once it has loaded, from 0**, undoing a PAUSE or SEEK dispatched before that.
- This made joining a paused jam resume it for everyone.
- Fixed in `startLoadedTrack`: the adapter re-applies the position and play state until they hold for 500 ms.

**Other details**
- Covers: `mediaItem.album.cover` is a UUID; the image is `https://resources.tidal.com/images/<uuid with / for ->/320x320.jpg`. Assumed from the API format, and it showed correctly in the app.
- An inactive subscription plays only 30-second previews, which limits testing positions past 30 s.

### Open problems

1. **TIDAL can't always find its own tracks.**
   - What happens: the session carries only title, artists, ISRC, album and duration. So even a TIDAL user receiving a track another TIDAL user queued must search for it.
   - Seen live: "Shoong!" from TIDAL's own queue wasn't found on rejoin.
   - Prevention: per-provider IDs in the protocol (see 5.1).
2. **Joining with a big queue is slow.**
   - What happens: songs are looked up one at a time. 48 songs took about 24 s on rejoin, and hundreds take minutes.
   - Options: load the current song plus the next ~20 first and the rest in the background, skip searches with per-provider IDs (5.1), or find a batch lookup in TIDAL's internals.
3. **Search query building.** The query is `title + artists`, then a scored match (normalized title, overlapping artist, conflicting ISRCs rejected). Titles with punctuation or features ("(feat. …)", "Remastered", non-Latin scripts) probably miss. TIDAL's own search may support ISRC lookup; that's worth checking in the client code.
4. **No safe "unload the current track"** action is known, so an empty session queue can't be applied while something plays (an error is reported instead).
5. **Some edits fall back to full replacement, which restarts the song.** This happens for moving the current song behind upcoming ones, seeking a track that isn't current, and queue types other than `"local"` (album, playlist or radio context?). Check which `playQueue.type` values exist.
6. **The local-change suppression windows are time-based** (300 ms, and up to 5.5 s while holding a loaded track). A real user action inside a window is lost. Prevention: tag our own dispatches (5.3).
7. **Autoplay, shuffle and repeat aren't synced.** TIDAL's "play similar tracks" can append songs at queue end, which then reach the session as local edits. Unverified.

## 3. Spotify (needs a rework, after TIDAL is solid)

**Current state:** implements the v1 contract and passes its mock tests, but has **never been tested live** with sessions. Known gaps compared with TIDAL:

- [ ] **Full queue:** `getState` keeps only `nextItems` with `provider === "queue"`, dropping context, playlist, album and autoplay items. Spotify exposes all of them with a provider flag; report them like TIDAL's full play queue.
- [ ] **Lookups:** one search at a time, if Spotify search has the same cancel behaviour (check). Look up the current song first and stop early if it's missing. Reuse IDs already in the queue instead of searching.
- [ ] **Loading:** hold a freshly loaded track at the session's position and play state, like TIDAL's `startLoadedTrack`. Spotify very likely also starts loaded tracks by itself.
- [ ] **Edits:** use incremental queue edits (`insertIntoQueue`, `removeFromQueue`, `reorderQueue` exist on PlayerAPI) instead of replacements, and check whether they restart the current track.
- [ ] **Play state and position:** verify `positionAsOfTimestamp` / `timestamp` semantics (like TIDAL's sync timestamp?), what `isPaused` does while loading, and the track-start position reset.
- [ ] **Echoes:** check the 300 ms suppression and debounce against real event timing.
- [ ] **Recorded traces:** record real PlayerAPI event and state traces (5.2) and rebuild the mock from them.
- [ ] **Live test:** run the `live_session` example and a two-person jam (TIDAL + Spotify), with a missing song on each side.
- [ ] **Store version:** check whether it can be launched with `--remote-debugging-port`.

## 4. YouTube Music (planned)

- **Client: the browser** (decided with the user). There's no official desktop app, and a Chromium browser on music.youtube.com is attached over CDP just like the Electron apps.
  - **Profile:** Chrome 136+ ignores `--remote-debugging-port` on the user's *default* profile (a security change), so MyJam would launch the browser with its own `--user-data-dir`. The user signs in to YouTube there once. Check whether Edge behaves the same.
  - **Launching:** detection and launching differ from desktop apps. MyJam would look for an installed Chromium browser (Chrome, Edge, Brave) and open a dedicated window on `music.youtube.com`, instead of starting a known exe.

Open questions before writing any code:

- **Internals:** YouTube Music is built with Polymer/Lit web components and has a player API on the `ytmusic-player-bar` / `#movie_player` elements (`getCurrentTime`, `seekTo`, `playVideo`, `pauseVideo`). The queue lives in the `ytmusic-player-queue` component's store. All unverified; study the client code first (5.4).
- **Ads:** they interrupt playback for non-Premium users, so treat them like a temporary "waiting" state.
- **Search:** song vs music video results, region locks.
- **The runtime side needs no protocol change:** the `ProviderBridge` trait is the extension point. Only `BridgeController`'s launch/attach assumptions (one desktop exe, fixed page URL) need generalizing.

## 5. Preventing these problems instead of patching them

1. **Per-provider IDs in tracks.** Add an optional `providerIds: { tidal?: string, spotify?: string, youtube?: string }` to `TrackDescriptor` (protocol v2). Whoever has the track fills its own ID; same-provider clients skip search entirely, and others fill theirs in after a successful lookup. This fixes TIDAL problems 1 and 2 for same-provider groups and cuts search load in general. Unknown fields are dropped by v1 clients, hence v2.
2. **Record real traces to build mocks.** A debug mode in the adapter that records every store action and state diff (or PlayerAPI event) with timestamps during a scripted live run (load a queue, pause, seek, skip, song end). Mocks built from these traces behave like the real app. The paused-join bug passed its first fix because the mock was "politer" than TIDAL.
3. **Tag our own dispatches instead of time windows.** If the store lets us add a marker to actions (or we wrap `dispatch`), state changes caused by MyJam can be told apart from the user's exactly, replacing the 300 ms and 5.5 s suppression windows.
4. **Read the clients' own code more.**
   - Look for the action creators the app uses itself: its playback state machine, a queue "clear current" action, ISRC search, batch lookups, autoplay settings.
   - Prefer the app's own intent actions over raw state writes.
   - Keep a short list of the action types and fields each adapter depends on, and check them at start-up. Fail loudly with "this TIDAL version isn't supported yet" instead of misbehaving.
5. **A shared contract test suite.** The same scenarios run against every adapter's mock:
   - local change published, remote change not echoed
   - paused join stays paused
   - missing current or upcoming song
   - incremental edits keep the song
   - track-start reset isn't a seek
   - lookups one at a time
   A new adapter is done when it passes the suite *and* a scripted live run.
6. **A shared "hold until stable" helper.** `startLoadedTrack`'s pattern (re-apply the target state until it holds) is likely needed by every provider. Move it into a small shared library injected with each adapter.
7. **Capability flags per adapter.** For example: `incrementalQueue`, `canUnloadTrack`, `fullQueue`, `providerIds`. Then the runtime can choose operations, such as avoiding full replacements, instead of discovering failures at runtime.
8. **Drift measurement (phase 7d).** Measuring each client's real position against the session shows timing problems (load delays, lost seeks) as numbers, not anecdotes.
9. **A neutral state after attaching** (proposed by the user; not implemented yet).
    - **The idea:** right after the bridge attaches (which reloads the app), wait for the app's own restore to finish, then put it in a known state before MyJam relies on it.
    - **Include:**
      - **Pause.** This removes the auto-resume after the reload, which today is waited out by the runtime's settle step, and starts every jam from a deliberate state.
      - **Shuffle off, repeat off, autoplay ("play similar") off.** None of them can be synced, and they reorder or extend the queue behind MyJam's back. Remember the user's settings and restore them on leave or disconnect.
    - **Leave out: clearing the queue.** "Start a jam with your current queue" needs it, joining replaces it anyway, and TIDAL has no known safe way to unload the current track (TIDAL problem 4).
    - **Adapter contract:** a `prepare()` method (and `restore()`), so every provider gets the same baseline.
10. **Version awareness.** Record the client version (Electron/app version from CDP `Browser.getVersion` or the exe) with each live verification above, so a client update that breaks something is recognisable.
