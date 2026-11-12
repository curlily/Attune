# Provider adapters: known behaviour, problems and plans

Attune controls music apps through their *private* internals (TIDAL's Redux store,
Spotify's PlayerAPI, YouTube Music's store and player), injected over the Chrome DevTools Protocol. These internals
are undocumented and full of timing quirks. This file collects everything learned
about them, what is still broken or unverified, and how to make adapters robust,
so the next adapter starts from what we know.

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
| Playback needs a gesture | A browser refuses to start playback from a script until the page had a user gesture. Desktop apps allow it. | Verified live (YouTube Music) | Calls are evaluated as user gestures; the browser is launched with `--autoplay-policy=no-user-gesture-required` |
| Leave dialog | A page that is playing can block its reload with a "Leave site?" dialog | Verified live (YouTube Music) | `install_bridge` confirms `beforeunload` dialogs |
| Interruptions | Advertisements take over playback for a while | Verified live (YouTube Music) | Adapter sends `Interruption`; the runtime steps out and rejoins after |

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
- **Shuffle, repeat and autoplay** (verified live; found by reading the bundle):
  - **Where the state lives:** `playQueue.shuffleModeEnabled`, `playQueue.repeatMode` (`REPEAT_OFF 0`, `REPEAT_ON 1`, `REPEAT_ONE 2`), and `settings.autoPlay` ("keep playing similar content when your queue ends").
  - **Repeat and autoplay actions:** `playQueue/SET_REPEAT_MODE` with the mode number, and `settings/SET_AUTOPLAY` with a boolean.
  - **Shuffle actions:** `playQueue/ENABLE_SHUFFLE_MODE` and `playQueue/DISABLE_SHUFFLE_MODE` only flip the flag, with no payload. TIDAL's own buttons use `…_AND_SHUFFLE_ITEMS` / `…_AND_UNSHUFFLE_ITEMS`, which reorder the queue.
  - **Persistence:** all of them are saved by TIDAL (`LOAD_PLAYER_SETTINGS_FROM_LOCAL_STORAGE_SUCCESS`).
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
7. **Autoplay, shuffle and repeat aren't synced.** Mitigated: they're turned off for the length of a jam (see 5.9). Changing them in TIDAL during a jam isn't detected yet.

## 3. Spotify

Reworked and tested live against Spotify 1.3.8.620 (Chrome 151), by attaching over CDP,
reading its code and recording its events. Not yet run through `SessionRuntime`
(the `live_session` example) or in a two-person jam.

### Verified behaviour

**Services and where things live**
- The platform object (service registry) is a prop on React fibers: `props.platform.getRegistry()._map`, keyed by symbols (`PlayerAPI`, `SettingsAPI`, `GraphQLLoader`, …).
  - Some components get `props.playerAPI` directly. Taking the first of either found the PlayerAPI without the platform, so search and settings were missing. The adapter prefers the platform.
  - `SettingsAPI` and `GraphQLLoader` are created lazily: they don't exist yet when the adapter becomes ready (0.3 s after the reload). They are looked up when used.
- `PlayerAPI` is a wrapper around the native player, which it reaches through `_contextPlayer` (protobuf RPCs: `play`, `setQueue`, `stop`, `seekTo`, …). RPCs answer `{ error, reasons }` instead of throwing.
- `PlayerAPI.getEvents()` emits `update` (the new state), `queue_update`, `action` / `command` (a PlayerAPI call is about to run) and `queue_action`.
- The state changes **only** with an `update` event, some 10–300 ms after a call resolves. `queue_update` can follow up to 500 ms after its `update`.
- One change produces a burst of updates (2–6 within 100 ms).

**The queue**
- Upcoming items have a `provider`: `"queue"` (added by the user), `"context"` (rest of the playlist or album), `"autoplay"` (recommendations).
- The state's `nextItems` is cut off at **50** items (`getCapabilities().maxNextTracks`). The full list is `_queue.getInternalQueue().nextTracks` (60 queued items were all there). It carries only string metadata (`title`, `artist_name`, `artist_name:1`, `album_title`, `duration`).
- A playlist shows a window of its remaining tracks, not all of them.
- Every item has a `uid`. Queue uids start with `q`. A queued item keeps its uid when it becomes the current track.
- **uids are reused:** playing the same single track twice gives the same uid. A current track that didn't come from the queue is treated as a new occurrence.
- **The queue is written as a whole:** `_contextPlayer.setQueue({ nextTracks, prevTracks, queueRevision })`. PlayerAPI's own `addToQueue`, `removeFromQueue`, `reorderQueue`, `insertIntoQueue` and `clearQueue` all do this.
  - It never touches the current track.
  - Existing items are kept by passing them back unchanged. New ones need only `{ contextTrack: { uri, uid: "", metadata: { is_queued: "true" } }, provider: "queue" }` and get a uid.
  - An outdated `queueRevision` is refused with error code 3 (returned, not thrown).
  - Playlist items can be removed, reordered and inserted between. A queue item placed behind playlist items becomes a `"context"` item and keeps its uid; marking a playlist item as queued gives it a new uid.
  - Autoplay items can't be edited: Spotify regenerates that list about a second later.
- Clicking a later item in Spotify's queue drops the queued items before it.

**Loading and play state**
- `PlayerAPI.play({ uri: track }, origin, { seekTo })` starts at the position, but **drops `initiallyPaused`**. `_contextPlayer.play` with `preparePlayRequest.options.initiallyPaused` and `seekTo` loads the track paused at the position (verified at 45 s). So no correcting afterwards is needed, unlike TIDAL.
- Playlists go through another service that ignores both options.
- A custom play origin (`featureIdentifier: "attune"`) is accepted.
- Playing a single track makes that track the whole context. Nothing follows it except queued items, and queued items survive the play.
- A track loads in about 200 ms: `isBuffering` true, then false. `isPaused` is false meanwhile.
- **A paused track often reports `isBuffering: true`** for as long as it is paused. Ignoring buffering states (as the old adapter did) loses pauses.
- `positionAsOfTimestamp` is the position at `timestamp` (epoch ms), like TIDAL's sync pair. No late reset to 0 after a track starts was seen.
- `pause()` while paused and `resume()` while playing throw (`not_paused`).
- `_contextPlayer.stop({})` unloads the current track and empties the queue. So an empty session queue can be applied (TIDAL can't).
- **End of the queue** (autoplay off): Spotify returns to the track its context started with, paused at 0. With a queue loaded by Attune that is the song the jam was on when this client joined. The adapter reports only a pause and keeps the finished entry current until the user does something.
- **Attaching:** a page reload doesn't interrupt playback (the player is native) and the state is there immediately. `settle` pauses anyway, as the contract says.

**Shuffle, repeat, autoplay**
- State: `shuffle`, `repeat` (0 off, 1 context, 2 track). Actions: `setShuffle(bool)`, `setRepeat(n)`.
- Autoplay: `SettingsAPI.autoPlay.getValue()` / `setValue(bool)`. After switching it off, the recommendations leave the queue within about a second.
- Spotify refuses to change shuffle and repeat while it plays recommendations (`restrictions.disallowTogglingShuffleReasons: ["autoplay"]`).
- Shuffle is remembered per playlist: playing a playlist turns its shuffle setting on.
- Repeat-context fills the upcoming list up to 50 by repeating the playlist.

**Search**
- GraphQL through `GraphQLLoader(operation, variables)`; operations are `{ name, operation: "query", sha256Hash }`. `searchTracks` takes `{ searchTerm, offset, limit, numberOfTopResults, … }` and returns title, artists, album, duration and playability.
- The hash is in `xpui-routes-search.js`. The adapter reads it from the loaded modules, else fetches that script, else uses the 1.3.8 hash. The old "type into the search box" warm-up is gone.
- **`isrc:<code>` works as a search term.**
- Searches are independent: four at once took as long as one (about 350 ms).
- The same recording appears several times (single, album, deluxe) with equal title, artist and duration. The adapter takes Spotify's first.
- Titles differ between services: "Shoong!" is "Shoong! (feat. LISA of BLACKPINK)" here. Featured-artist suffixes are ignored when comparing.

**Timings measured:** adapter ready 0.3 s after the reload; a 5-song snapshot 1.0–1.3 s; 72 songs (12 different ones) 2.0 s; a queue edit 130 ms; a skip 50 ms.

### Open problems

1. **No ISRC.** Spotify's player doesn't carry ISRCs, so tracks it puts into a session have none. Other providers then fall back to title and artist. A metadata lookup (a track GraphQL query or the metadata extension service) could fill it in.
2. **After the queue ended,** a remote insert or play makes Spotify load the finished song again from the start, while the session thinks it is at its end. Rare, and the next song change fixes it.
3. **Podcasts, local files and advertisements** aren't tracks. While one plays, nothing is reported. Assumed, not seen live: the test account didn't get ads.
4. **Changes from another device** (Spotify Connect) haven't been tested.
5. **Starting a jam from a playlist** publishes the window of upcoming playlist tracks Spotify shows (about 15–50). As it moves on and shows more, those are published as inserts. Not tested with a session server.
6. **The local-change suppression is still partly time-based:** 300 ms after a remote change, though the change itself is now waited for. A user action in that window is lost.
7. **Changing shuffle, repeat or autoplay during a jam** isn't detected (same as TIDAL).
8. **Debug port:** Attune's config expects Spotify on 9223. `--remote-debugging-port=<port>` needs the `=`; with a space Spotify ignores it without a word.
9. **Store version:** still unchecked.
10. **Not done yet:** the `live_session` example and a two-person jam (TIDAL + Spotify), with a missing song on each side.

## 4. YouTube Music

Built and tested live against music.youtube.com (client `WEB_REMIX 1.20261110.04.00`) in
Microsoft Edge 157, on an account without a subscription (so with advertisements), by
attaching over CDP, reading the page's code and recording its store actions. The Rust
controller attached to it and applied changes (`live` example). Not yet run through
`SessionRuntime` (the `live_session` example) or in a two-person jam.

### One-time setup

YouTube Music has no desktop app. Attune opens it as an app window of an installed
Chromium browser (Edge, Chrome or Brave), in a browser profile kept only for this
(`%LOCALAPPDATA%\Attune\YouTubeMusicProfile`, or `ATTUNE_YOUTUBE_MUSIC_PROFILE`).

1. **Sign in first, in a window without the debug port.** Google refuses sign-ins in a
   browser started with a debug port (verified live by the user). The connect screen's
   "Sign in first" button opens the same profile without it.
2. **Close that window.** While it is open, a new launch joins it and gets no debug port.
   Attune then reports that it couldn't connect to the window.
3. **Connect.** Attune starts the window with the debug port; the sign-in is remembered in
   the profile.

Signing in is optional: the page also plays signed out. The launch command is

```
<browser> --user-data-dir=<profile> --remote-debugging-port=9224
          --autoplay-policy=no-user-gesture-required
          --no-first-run --no-default-browser-check --app=https://music.youtube.com
```

- Browsers ignore `--remote-debugging-port` on the user's default profile, hence the
  profile of its own.
- Attune never closes the browser to restart it: it holds the user's other windows.
- The profile holds the Google sign-in. Keep it out of version control (`/EdgeProfile/`
  is ignored in this repository for that reason).

### Verified behaviour

**Where things live**
- Script and file names are generated (`/s/player/fad7e4e9/…`, `/s/3d91f0af/…`) and the
  code is minified, so nothing depends on either. Everything is reached from two custom
  elements with fixed tag names and their named properties:
  - `ytmusic-player-bar`: `.inst.store` (Redux store), `.inst.queue` (queue service),
    `.inst.playerApi` (the YouTube player API).
  - `ytmusic-app`: `.inst.networkManager` (signed-in requests to YouTube's API).
  - Elements forward to a controller in `.inst`; the adapter falls back to the element.
- Some properties do have generated names (`JSC$13963_playerController`). They are not used.
- The player API is ready about 0.9 s after a reload.

**The queue**
- `store.getState().queue`: `items`, `selectedItemIndex`, `nextQueueItemId`,
  `automixItems` (the autoplay list), `autoplay`, `repeatMode` (`NONE`, `ALL`, `ONE`),
  `shuffleEnabled`.
- An item is `{ playlistPanelVideoRenderer }` (title, byline, length, video ID), or a
  `playlistPanelVideoWrapperRenderer` around one when a song also has a video version.
- Each item has a number of its own in `navigationEndpoint.watchEndpoint.index`, handed
  out from `nextQueueItemId`. **The numbers start again from 0 when the queue is cleared**,
  so they identify an item only until then.
- Items before the current one are kept as history.
- **`selectedItemIndex` is left behind when the queue is replaced** (seen: 1 item, index 6).
  The current item is the one with `selected: true`; the queue service's
  `getCurrentItemIndex()` knows.
- The player follows the store. The action names are plain strings in the reducer:
  - `UPDATE_ITEMS { currentIndex, items, shouldAssignIds, nextQueueItemId }` replaces
    everything after the current item and never touches playback.
  - `CLEAR`, `ADD_ITEMS { items, index, shouldAssignIds, nextQueueItemId }`,
    `MOVE_ITEM { fromIndex, toIndex }`, `REMOVE_ITEM index`, `SET_INDEX index`.
  - The queue service's `selectQueueItem(item)` makes an item current and starts it.
- Queue items for video IDs come from `music/get_queue { videoIds }` (100 ms for four),
  which is what the app itself uses for "add to queue".
- Edits are instant (0–4 ms) when the songs are already in the queue.
- **Server queues:** the signed-in app also keeps the queue on the server
  (`serverQueuesEnabled`). Store edits bypass that. No effect was seen within a session.

**Loading and play state**
- **Starting playback needs a user gesture.** Without one, the track loads and stays
  "unstarted" (player state -1). Attune launches the browser with
  `--autoplay-policy=no-user-gesture-required` and marks its calls as user gestures
  (`Runtime.evaluate` with `userGesture`), which also covers a window the user started.
- Selecting an item always starts it **from 0, playing**; a paused start wasn't found.
  The adapter re-applies position and play state until they hold (like TIDAL). A paused
  join landed at 30.000 s.
- A track starts in 0.5–0.9 s. Meanwhile the player still reports the previous video.
- Player states: -1 unstarted, 0 ended, 1 playing, 2 paused, 3 buffering, 5 cued.
- The position is read live (`getCurrentTime()`, seconds); there is no sync timestamp.
  Seeks don't pass through the store, so the adapter looks at the position every 500 ms.
- Durations in the queue are whole seconds ("3:22").
- **Song end:** `SET_INDEX` to the next item, which starts about 0.6 s later.
- **End of the queue** (autoplay off): the player stops on the last track (state 0).
- **`Leave site?` dialog:** while a song plays, the page blocks unloading, which holds
  up the reload Attune attaches with. The controller confirms the dialog.
- **After a reload** the page restores its last song (the URL is `/watch?v=…`) and, with
  playback allowed, starts it, possibly behind an advertisement. `settle` pauses either.

**Advertisements**
- `store.getState().player.adPlaying` is true for the length of an advertisement
  (15 s seen, before a song). The song waits underneath in state -1.
- `player.isPlaying` follows the advertisement meanwhile; `pauseVideo()` pauses it.
- A seek during the advertisement moves the song, which then starts there.
- Afterwards the song starts by itself.
- **Handling:** the adapter sends an `Interruption` event when one starts and ends. The
  runtime steps out and rejoins after it (the user asked for this). Nothing the page
  does meanwhile is published, so a click during an advertisement is undone by the
  rejoin. A session change that arrives during one waits for it (up to 2 minutes).

**Autoplay, repeat, shuffle**
- Autoplay: `SET_AUTOPLAY_ENABLED bool`. Repeat: `SET_REPEAT mode`. Both are saved by the app.
- **Starting a song turns autoplay back on**, unless `hasUserChangedDefaultAutoplayMode`
  is set (`HAS_USER_CHANGED_DEFAULT_AUTOPLAY_MODE`, what the app's own toggle does). The
  adapter sets it for a session, switches autoplay off again should it come back, and
  restores both afterwards.
- With autoplay off the autoplay list is still fetched (49 items) but not played.
- Shuffle is a one-time reordering of the queue, not a mode. Nothing to switch off.

**Search**
- `networkManager.fetch("/search", { query, params })` with the "Songs" filter
  (`EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D`): 460 ms, 20 rows.
- A row has the video ID, title, and a byline whose links mark artists
  (`MUSIC_PAGE_TYPE_ARTIST`) and the album (`MUSIC_PAGE_TYPE_ALBUM`), then the length.
- **The ranking is poor:** for "Blinding Lights The Weeknd" the original was fourth,
  behind covers. The adapter scores every row (title, artist, album, length).
- Music videos (`MUSIC_VIDEO_TYPE_OMV`) have no album and no artist links; their byline
  is "Artist & Other • 2.1B views".
- Titles differ again: "Shoong! (feat. 리사 (LISA))".
- The network manager caches responses, so a repeated lookup costs nothing.

**Timings measured:** adapter ready 0.8–1.0 s after the reload; a 5-song snapshot with a
paused start 2.9 s (four searches); a queue edit 0–4 ms, or 450 ms with one lookup;
selecting another song 0.9–1.7 s.

### Open problems

1. **No ISRC.** The page doesn't expose ISRCs, and whether its search finds them wasn't
   tested. Lookups are by title and artist only.
2. **Covers and versions.** With a poor ranking and no ISRC, a cover with the same title
   and artist name can win. Album and length decide between candidates when the session
   carries them.
3. **Advertisements cost a rejoin.** After one, the session state is applied again. If
   the same song is still current that is a seek; otherwise the song is loaded, which can
   trigger another advertisement. Not yet observed how often that chains.
4. **A paused start is audible for a moment:** the song plays from 0 until it is paused
   and sought (a few hundred ms).
5. **Local-change suppression is partly time-based** (300 ms after a remote change).
6. **Server queue:** store edits aren't sent to the server queue. Effects across a
   reload or on other devices weren't tested.
7. **Signed out, age-restricted and region-blocked videos** weren't tested.
8. **Podcasts and uploads** in the queue weren't tested.
9. **Other browsers:** only Edge was tested. Chrome and Brave take the same flags.
10. **Not done yet:** the `live_session` example and a two-person jam, launching the
    browser from Attune (the code path exists, the test used a window started by hand),
    and the connect screen's sign-in button in the running app.

## 5. Preventing these problems instead of patching them

1. **Per-provider IDs in tracks.** Add an optional `providerIds: { tidal?: string, spotify?: string, youtube?: string }` to `TrackDescriptor` (protocol v2). Whoever has the track fills its own ID; same-provider clients skip search entirely, and others fill theirs in after a successful lookup. This fixes TIDAL problems 1 and 2 for same-provider groups and cuts search load in general. Unknown fields are dropped by v1 clients, hence v2.
2. **Record real traces to build mocks.** A debug mode in the adapter that records every store action and state diff (or PlayerAPI event) with timestamps during a scripted live run (load a queue, pause, seek, skip, song end). Mocks built from these traces behave like the real app. The paused-join bug passed its first fix because the mock was "politer" than TIDAL.
3. **Tag our own dispatches instead of time windows.** If the store lets us add a marker to actions (or we wrap `dispatch`), state changes caused by Attune can be told apart from the user's exactly, replacing the 300 ms and 5.5 s suppression windows.
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
6. **A shared "hold until stable" helper.** TIDAL's adapter now has `holdUntil({matches, correct, stableMs, timeoutMs})`, used by `startLoadedTrack` and `settle`: it re-applies the target state until it holds. Every provider will likely need it, so move it into a small shared library injected with each adapter.
7. **Capability flags per adapter.** For example: `incrementalQueue`, `canUnloadTrack`, `fullQueue`, `providerIds`. Then the runtime can choose operations, such as avoiding full replacements, instead of discovering failures at runtime.
8. **Drift measurement (phase 7d).** Measuring each client's real position against the session shows timing problems (load delays, lost seeks) as numbers, not anecdotes.
9. **A neutral state.** Proposed by the user; **done for TIDAL and Spotify**. Adapter contract:
    - **`settle({pause})`** runs right after attaching. It waits until the app has finished restoring itself (queue and play state unchanged for 1 s, or 3 s while empty, at most 8 s), then pauses and holds the pause for 1.5 s, which catches a late auto-resume. The queue is kept. `BridgeController::initialise` calls it.
    - **`prepareForSession()`** runs when entering a jam. It turns off shuffle (without reordering), repeat and autoplay, and returns the user's values.
    - **`restoreAfterSession(previous)`** puts them back on every way out. Leaving, quitting Attune (Tauri's exit event, at most 3 s) and a failed create or join were verified live or in tests; the runtime does this in `prepare`/`restore`.
    - **Left out: clearing the queue.** "Start a jam with your current queue" needs it, joining replaces it anyway, and TIDAL can't safely unload the current track (TIDAL problem 4).
    - **Caveats:**
      - TIDAL saves these settings. If Attune crashes mid-jam, they stay off until changed by hand. A fix would be to save `previous` in Attune's settings and restore it on the next start.
      - Turning shuffle back on *during* a jam isn't detected yet. Detect it and either re-disable it or tell the user.
10. **Version awareness.** Record the client version (Electron/app version from CDP `Browser.getVersion` or the exe) with each live verification above, so a client update that breaks something is recognisable.
