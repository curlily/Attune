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
| Reload ignored | TIDAL 2.48 ignored CDP's `Page.reload` completely after a while (no navigation within 90 s, no dialog, neither `beforeunload` handler cancels), so the bridge never got injected and attaching timed out twice. `location.reload()` from the page reloaded it at once | Verified live, fixed | `install_bridge` reloads from the page if no navigation has started 2 s after `Page.reload` (ready 0.4 s later) |
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
| Background timers | While the app's window is hidden and nothing plays, Chromium runs the page's timers once a second (a 25 ms timeout took 1000 ms). Polling loops slow down exactly while a track loads paused. | Verified live (TIDAL; Spotify, **also while playing**, as its sound is native; YouTube Music, where sound from the page lifts it, and selecting a song always starts sound). Hidden for more than 5 minutes, chained timers run **once a minute** (verified live, YouTube Music) | Waits react to the app's own signals, with a 250 ms fallback check: TIDAL store changes (`waitFor`, `holdUntil`); Spotify `update`/`queue_update` events (`watchPlayer`); YouTube Music store changes and `<video>` media events, caught in the capture phase (`watchPage`). Timers remain only for timeouts and stable periods, and the 150–180 ms debounce of local changes |
| Resuming a new list part-way | A new current song starts at 0 in the session (`queue.replace`), but **Spotify resumes an album or playlist where it was left** (seen 48 s, 164 s and 176 s in). The others then played that far behind, and the next resync (a stale queue edit, a rejoin) threw Spotify back to the session's position: heard as the song restarting, again on every resync | Verified live (Spotify + YouTube Music session), fixed | After a local replace that changes the current song, the runtime compares the provider's position with the session's and attaches where the provider really is to the replace (protocol 2 `playback`, `with_actual_playback`). Covers any provider. Live: Spotify resumed at 1:33, YouTube Music started at 1:34.5, within 20 ms of it |
| Joining an empty session | A provider that joins a session with an empty queue keeps its own queue. Its later changes (inserts, a play) refer to songs the session doesn't have: the server refused each, each refusal resynced to the empty session, and that **pauses**. Spotify starting a playlist was paused five times in a row and nothing was shared | Verified live (YouTube Music hosting after a reload, Spotify joining), fixed | While the session queue is empty, a local change publishes the provider's whole queue instead (`for_empty_session`), then its position and play state; inserts of entries the session already has are dropped |
| Long new queues | Every adapter used to look up all songs of a new queue before starting the current one. Songs from another provider carry no ISRC, so each is a text search: four at a time (Spotify, YouTube Music) or **one at a time** (TIDAL) | Verified live, fixed | All three start the current song first and look up the rest meanwhile (the lookups don't touch the queue, so they run alongside the start and its hold), then add the rest. Live: YouTube Music in sync after 1.5 s (81 songs, the rest 15 s later), Spotify playing after 0.9 s (49 songs, the rest 5 s later), TIDAL playing after 2 s; TIDAL's whole 51-song rejoin now takes 6.8 s (was about 23 s), see TIDAL search |
| Changes waiting behind lookups | A provider applies session changes one at a time, so while it looked up a long new list (6–15 s for 80 songs) a pause or seek from someone else reached it only afterwards | Verified live, fixed | The runtime loads the current song and the next 8 songs the provider doesn't have, and has the provider look the rest up in the background (`prefetch`, outside the adapter's queue of remote changes). Meanwhile those entries count as missing for index translation, like unavailable ones, but aren't reported. When the lookup is done they are added with a replace keeping the current song; a skip to one of them loads everything around it. Live: a pause on Spotify reached YouTube Music 0.9 s later while 72 songs were still being looked up; they were added about 7 s later without a restart (`defer_long_tail`, `on_prefetched`) |
| Remaster tags and band credits | Spotify names many classics "Title - 2016 Remaster", "Title - Remastered 2021" or "Title - 2015 - Remaster"; YouTube Music calls them "Title" or "Title (Remaster)". And YouTube Music credits "Purple Rain" to "Prince & The Revolution", Spotify to "Prince". YouTube Music didn't match 16 of 81 songs in a rock playlist | Verified live, fixed | All three adapters compare (and search) titles without remaster tags, like "(feat. …)" (`stripSameRecordingTags`); live, single and other versions keep their tags. An artist name that starts with the other one as whole words counts as the same act (4 characters or more). Live: 81 of 81. Looser case: "(Single Version; 2018 Remaster)" loses the whole bracket |
| Following a replace, then its seek | A follower applies session changes one at a time. When a provider shares a new list resumed part-way, the seek that follows the replace waits until the follower has looked up the whole list: YouTube Music played the 50-song list from 0 for 8 s, then jumped to Spotify's 1:35 | Verified live, fixed | Protocol 2: `queue.replace` carries the new current song's `playback` (position, play state), so no seek follows it |
| Comparing after a remote change | The app's state keeps changing for about a second after an action, which can push a debounce past a fixed suppression window; the delayed comparison then sees our own change as the user's. **With throttled timers it always happens:** the debounce takes a second, so it always fires after the 300 ms window | Verified live (TIDAL, Spotify, YouTube Music) | Every adapter resets the comparison base to the state the remote change produced: TIDAL `rebaseLocalChanges`, Spotify and YouTube Music `rebase()` at the end of `asRemoteChange`. Removing it makes both new tests publish an echo. **The rebase is only as good as the wait before it:** Spotify's queue write used to count as done while the old items still showed, and an echo followed (verified live, fixed) |

## 2. TIDAL

Reworked and tested live against TIDAL 2.48.0 (Electron 43.8.3, Chrome 150), by
attaching over CDP, reading its bundles (the `playQueue` and `playbackControls`
reducers, the player sagas, the player SDK) and recording store and media-element
traces. The account had no active subscription, so only 30-second previews played.
The adapter was injected into the running app and driven directly, not through
`SessionRuntime` (the `live_session` example) or a two-person jam.

### Verified behaviour

**Store and actions**
- Store discovery scans loaded modules for the `No global store set` accessor.
- Action types are written as template literals in the bundle, e.g. `` A(`playQueue/…`) ``.
- Sagas dispatch through the middleware, not through `store.dispatch`: wrapping
  `store.dispatch` only sees the UI's and our own actions.
- `playQueue.currentIndex` marks the current element; elements before it are history.
- **Queue types** (`playQueue.type`, from the reducer): `"local"` (normal), `"cloud"`
  (TIDAL Connect, Chromecast or Sonos: the queue lives on that device and most queue
  actions are ignored), `"cloudV2"` (a queue kept on TIDAL's servers, behind the
  server flag `cloud-queue-v2`, which was off). The adapter refuses to rewrite the
  queue while it is `"cloud"`. `"cloudV2"` is untested.
- An element is `{ context, mediaItemId, priority, uid }`. `priority` is
  `priority_none`, `priority_keep` ("play next" items, which TIDAL keeps together
  when skipping ahead) or `priority_history`. TIDAL's uids are `pq__<hex>__<n>`; it
  parses that prefix only in its cloud-queue code. **Any uid works** in a local queue.

**Writing the queue** (all verified live)
- `playQueue/RESET { currentIndex, elements }` sets the elements as given, uids
  included. The player saga then loads the current element **without playing it**
  (the SDK's `load(…, prefetch = true)`): a paused join is never heard.
- RESET doesn't keep a song that is already playing. TIDAL's "the player already
  has it" check reads `localStorage.playerState`, which it only writes when the
  window is hidden or shown, so in most tries it reloaded the song paused from 0.
- **Keeping the current song:** `playQueue/CLEAR_UPCOMING` keeps only the current
  element, `playQueue/APPEND_ELEMENTS { elements }` adds ours. Neither reaches the
  player; the song played on without a gap.
- `playQueue/CLEAR_QUEUE` empties the queue and **unloads the current track**: media
  product `null`, `IDLE`, sound gone within 0.3 s. It also resets shuffle and repeat.
- `playQueue/ADD_MEDIA_ITEMS_TO_QUEUE` with `overwritePlayQueue` (the old way)
  restarts the current song and starts a new one playing. No longer used.
- In-place edits still used for single changes: `ADD_AT_INDEX {context, index,
  mediaItemIds}` (`index` absolute, history included; gives a `priority_keep`
  element with TIDAL's uid), `REMOVE_ELEMENT {uid}` (not the current element),
  `MOVE_TRACK {fromIndex, toIndex}` (inserts then removes, so forward moves need
  `target + 1`; the element gets a new uid when it becomes `priority_keep`).
- The player needs TIDAL's metadata for every element (`content.mediaItems`).
  `content/LOAD_SINGLE_MEDIA_ITEM { id, itemType: "track" }` fetches one: three in
  parallel took 150 ms.

**Loading a track at a position**
- After RESET, `playbackControls.prefilled` is true until the player has the track
  (MEDIA_PRODUCT_TRANSITION, 40–200 ms). A SEEK before that goes to the previous
  track and is lost; a PLAY waits for the load.
- The track loaded paused keeps the previous track's `desiredPlaybackState:
  PLAYING`, so the store looks like it is playing.
- **Seek, then PLAY right away** is reliable: PLAY on a track loaded paused
  ("prefetched") makes the SDK reload it from the position the seek has just set
  (170 ms). A playing join or an advance starts at the right place 0.3–0.4 s after
  the request.
- **Paused at a position is not reliable in one step:**
  - A seek right after the load was undone about 230 ms later, when the player
    finished loading.
  - The player's own position (`activePlayer.getPosition()`) and the store's
    disagreed after that, in both directions (player 0 and store 12 s, and the other
    way round).
  - PLAY then sometimes reloaded the track twice, the second time from 0 (seen with
    every song loaded right after another one had played).
  - **Handled by:** the hold checks both positions; on a session PLAY the adapter
    seeks to the store's position first and holds again. All four test variants then
    resumed at 12 s. In the double-reload case the song may be heard from 0 for a
    moment before the seek.
- Resuming after 40 s paused reloads the stream (180 ms) and keeps the position.
- **Auto-resume after a reload** (`Ybt` in the bundle): at start-up TIDAL loads the
  `playerState` track at its saved position and plays it only if it was playing and
  the state is less than 15 s old.

**Search and lookups**
- **Text search through the open API** (verified live, 2026-11-12): `getSearchResults`
  of the open-API client, `GET /v2/searchResults?filter[query]=…&include=tracks,
  tracks.artists,tracks.albums&explicitFilter=INCLUDE`, answers with TIDAL's ranking
  (the `tracks` relationship, 20 per page) and every track's title, ISRC, length,
  availability, artists and albums in one response, about 400 ms. **They run in
  parallel:** 20 at once took 670 ms, and the right song ranked first for 19 of 20 (the
  20th isn't on TIDAL). The adapter uses it, six at a time, and falls back to
  `search/SEARCH_FOR` without it. `/searchResults/{id}/relationships/tracks` wants the
  opaque ID `/searchResults` returns, not the query (400 `INVALID_RESOURCE_ID`).
- **TIDAL's own UI doesn't use that endpoint**; its search page goes through
  `search/SEARCH_FOR`.
- **Metadata for the player** (`content/LOAD_SINGLE_MEDIA_ITEM`, `GET /v1/tracks/<id>`)
  runs in parallel too: 20 in 267 ms. Not a bottleneck.
- **How TIDAL plays a playlist** (from the sagas): `playQueue/ADD_TRACK_LIST_TO_PLAY_QUEUE`
  `{ trackListName: "playlists/<id>", context, position: "now" }` uses the list's loaded
  items if it has them (`ADD_ALREADY_LOADED_ITEMS_TO_QUEUE`), otherwise
  `FETCH_FIRST_PAGE_AND_ADD_TO_QUEUE` loads one page (`content/LOAD_LIST_ITEMS_PAGE`)
  and starts it, and `FETCH_REST_OF_THE_TRACKS_AND_ADD_TO_QUEUE` pages through the
  rest in the background and appends it. That is the order the adapters now use. It
  works on TIDAL IDs only, so it would help only with per-provider IDs (5.1).
- **A hidden TIDAL's hold is the slow part now:** in a 51-song rejoin the lookups
  waited 6.4 s for the current song's start and hold (timers run once a second while
  hidden) before they ran alongside it. The rejoin took 6.8 s.
- Only the latest `search/SEARCH_FOR` survives: a new search cancels one still in flight. Searches must run one at a time.
- Results are at `content.trackLists["search/tracks/" + encodeURIComponent(query)].sorted.defaultSort.items`.
- An empty result is `items: []` with `loading: false`. Anything else that's empty is still loading.
- **TIDAL lists the same recording more than once** (single and album: same ISRC,
  title, artists and length). The old adapter rejected the tie as "ambiguous", which
  is why "Shoong!" was never found. Now TIDAL's best-ranked one is taken.
- **ISRC lookup:** TIDAL's open-API client (an RTK Query API object with
  `reducerPath: "tidalOpenPlatformApi"`, found among the module exports) has
  `getTracks` with `filter[isrc]` and `filter[id]`. At most **20 values** per request
  (more is a 400 `INVALID_ARRAY_LENGTH`). 20 ISRCs took 360 ms; results come in any
  order, with title, ISO 8601 duration (`PT3M41S`), `availability` (`STREAM`) and,
  with `include: ["artists", "albums"]`, names. Lookups may run in parallel.
- The ISRC can match several releases (e.g. two of "Blinding Lights"). Album and
  length decide, otherwise any of them is the same recording.

**Play state**
- A pause is `playbackState: "NOT_PLAYING"` (`"PAUSED"` hasn't been seen).
- `playbackState` leaves `"PLAYING"` while a track loads.
- From the reducer: only PAUSE, STOP and SET_DESIRED_PAUSE_STATE set
  `desiredPlaybackState` to `NOT_PLAYING`; SET_PLAYBACK_STATE with `PLAYING` or
  `STALLED` sets it back to `PLAYING`. After a PAUSE the player confirms with
  `playbackState` up to a second later. So a requested pause counts at once (see
  `isPlaying()`).
- After switching tracks, the **previous track's `PLAYING` lingers** for a moment, so "same track and PLAYING" doesn't mean the new track has started.

**Position**
- `latestCurrentTime` is in seconds and valid *at* `latestCurrentTimeSyncTimestamp` (epoch ms).
- It is updated only on play, pause, seek and track changes, so reading it later needs extrapolation.
- After a seek TIDAL writes the new position once the player has sought, up to a
  second later.
- `playbackControls/TIME_UPDATE <seconds>` only sets the store's position (nothing
  in the player reacts to it).
- When a track actually starts, TIDAL resets the position to 0 (sometimes about 3 s after the request). This is not a seek: the adapter ignores it for 10 s after a track change.
- **Skipping keeps the old song's sync point for a moment.** Pressing next writes the sync point at the click (75.6 s into the old song), then shows the new track about 740 ms later still carrying it, and resets to 0 only about 220 ms after that (2026-11-13, recorded by polling `getState()` every 100 ms). The 180 ms debounce fell inside that window, so the skip was published as an advance to 75 s into the new song: Attune's progress bar carried on from where the old song was skipped (stuck at the end when that was past the new song's length), and the others were sent there. A song's natural end didn't show it (position 0, synced 0.1 s before). Fixed: a sync point from before the active track was seen to start is read as 0 at its start (`playbackOf`; a track already playing when the adapter attached is left alone). Test: "Skipping in TIDAL advances to the new song's start …".

**Timers while TIDAL is in the background**
- While TIDAL's window is hidden and nothing plays, Chromium runs the page's timers
  **once a second** (a 25 ms timeout took 1000 ms); with sound playing they run
  normally. Polling waits therefore took up to a second per step exactly while a
  track loaded paused. Store notifications aren't delayed (they come from media and
  network events), so the adapter waits on store changes instead of polling.

**Other details**
- **Shuffle, repeat and autoplay** (verified live; found by reading the bundle):
  - **Where the state lives:** `playQueue.shuffleModeEnabled`, `playQueue.repeatMode` (`REPEAT_OFF 0`, `REPEAT_ON 1`, `REPEAT_ONE 2`), and `settings.autoPlay` ("keep playing similar content when your queue ends").
  - **Repeat and autoplay actions:** `playQueue/SET_REPEAT_MODE` with the mode number, and `settings/SET_AUTOPLAY` with a boolean.
  - **Shuffle actions:** `playQueue/ENABLE_SHUFFLE_MODE` and `playQueue/DISABLE_SHUFFLE_MODE` only flip the flag, with no payload. TIDAL's own buttons use `…_AND_SHUFFLE_ITEMS` / `…_AND_UNSHUFFLE_ITEMS`, which reorder the queue.
  - **Persistence:** all of them are saved by TIDAL (`LOAD_PLAYER_SETTINGS_FROM_LOCAL_STORAGE_SUCCESS`).
- End of the queue (from the saga): with autoplay on, TIDAL starts a radio of the last
  track; otherwise it pauses.
- Covers: `mediaItem.album.cover` is a UUID; the image is `https://resources.tidal.com/images/<uuid with / for ->/320x320.jpg`. Assumed from the API format, and it showed correctly in the app.
- An inactive subscription plays only 30-second previews, which limits testing positions past 30 s.

### How the adapter handles it now

- **Replacing the queue:** keeps the current song with CLEAR_UPCOMING +
  APPEND_ELEMENTS when it stays, otherwise RESET with only the current song (loads
  paused), then `startLoadedTrack` holds position and play state. The rest is looked
  up after that and added with CLEAR_UPCOMING + APPEND_ELEMENTS (`appendUpcoming`),
  if the song is still current. Joining an 81-song Spotify queue (no ISRCs) played
  after 2 s; the lookups took another 36 s. An empty session queue is CLEAR_QUEUE.
- **Entry IDs live in the uids** (`pq__attune__<entryId>`). TIDAL saves its queue
  with them, so they survive a bridge reload.
- **Lookups:** ISRCs in batches of 20 first (cached per ISRC), then the open API's
  text search six at a time (the title without "(feat. …)" and the main artist, then
  the full title and all artists), or the app's search one at a time without it.
  Titles are compared without "(feat. …)".
- **No echoes, measured live:** after every remote change the comparison base for
  local changes is reset to the state that change produced (the store kept changing
  for about a second, which pushed the debounce past the 300 ms window). A position
  jump to where our last seek should land is ours. In a scripted run (join, replace
  keeping the song, advance, pause, seek, empty) nothing was published; the user's
  pause, seek, play, skip and removal in TIDAL were.
- **Timings measured** (TIDAL hidden): join playing 0.9 s, paused join 1.0–2.6 s,
  replace keeping the song 0.5–0.7 s (no gap), advance 0.9 s (sound after 0.34 s),
  seek 15 ms, empty queue 25 ms, resume after a paused join 0.7–1.2 s.

### Open problems

1. **Per-provider IDs.** The session still carries only title, artists, ISRC, album
   and duration. With an ISRC, TIDAL now finds the exact recording; without one (e.g.
   from Spotify, which has none) it falls back to text search. Per-provider IDs (5.1)
   would remove the lookup for same-provider groups.
2. **Big queues without ISRCs** are searched six at a time through the open API (a
   51-song rejoin took 6.8 s, most of it the current song's hold while hidden). Without
   the open API (an older or newer TIDAL) it falls back to one at a time.
3. **A paused join can be heard from 0 for a moment** when the session resumes, in
   the double-reload case above. Why the player sometimes loads twice isn't known.
4. **Some edits still replace the whole queue:** inserting at the front, removing or
   moving the current song, and anything while the queue isn't a direct view of
   TIDAL's (a track played outside the queue). With RESET these no longer start the
   song playing, but a new current song is reloaded.
5. **Local-change suppression is still partly time-based** (300 ms after a remote
   change, and while holding a loaded track). A real user action inside that window
   is lost.
6. **Autoplay, shuffle and repeat aren't synced.** Mitigated: they're turned off for the length of a jam (see 5.9). Changing them in TIDAL during a jam isn't detected yet.
7. **Casting and the cloud queue:** writes are refused while casting; `cloudV2`
   (server queue) is untested and RESET is not synced to the server there.
8. **Not done yet:** the `live_session` example and a two-person jam with the reworked
   adapter, and a test with a full subscription (positions past 30 s, the native
   player used for lossless).

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
  - An outdated `queueRevision` is refused with error code 3 (returned, not thrown). A write right after `play` meets this, as the play changes the queue. Revisions aren't counters: one went back to an earlier value during a write.
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

**Waiting and echoes** (verified live against Spotify 1.3.9.2055, Chrome 156, with its
window hidden, by injecting the adapter into the running app and driving it directly;
afterwards the adapter was disposed and Spotify put back on the user's list, song,
position and pause)
- **Timers are throttled even while Spotify plays:** a 25 ms timeout took 680–990 ms,
  playing or paused. The sound comes from the native player, so the page counts as
  silent.
- **The waits react to Spotify's events now** (`watchPlayer`: `update`/`queue_update`,
  plus a 250 ms fallback check). Old adapter, hidden: play, seek, remove and pause about
  1.0 s each, a paused advance 2.0 s, a paused join 2.6 s. New: play 25 ms, seek 15 ms,
  remove 110 ms, pause 265 ms, advance 115–330 ms, a paused join 0.55–1.1 s, of which the
  searches are about 0.5–0.9 s.
- **A queue write right after `play` is refused** (code 3): the play changed the queue,
  and the queue shows the new revision with one of the next updates. The retry used to
  sleep 100 ms (330 ms while hidden). It now waits for the revision to change and
  retries at once.
- **When a queue write counts as done:** the state has to show the items we sent, not
  just the same songs. Rewriting songs that were already queued as new items (new
  entries for them) kept showing the old items, with the old uids, for another 120 ms.
  The revision changed several times per write, once **back to its earlier value**
  (revisions aren't counters), before the new items showed. The old check accepted the
  write at once, mapped the entries to the old uids, and about a second later published
  the new uids as the user's `queue.replace` (seen live 4 times in a row). Now: new items
  must have uids that didn't exist before, kept items their own uid, and queued items
  left out must be gone. After the fix, nothing was published in three such joins, a
  same-queue join (1 ms) or the sequence join, play, seek, remove, pause, advance.
- **Echoes, the TIDAL problem itself:** not affected, as `asRemoteChange` already rebased
  when an action finished. The wait above was the gap: the rebase captured a state
  Spotify hadn't finished producing.
- **Positive control:** a seek made through Spotify's own PlayerAPI was published after
  0.8–1.0 s. That is the 150 ms debounce while hidden.
- **Left to check live:** whether unshuffling re-orders the playlist later than the
  `shuffle` flag (publishing that would be correct, since the queue did change).

**Two-provider session** (verified live with YouTube Music through `SessionRuntime`, the
`live_pair` example, 2026-11-12): play, pause, seeks, skips, song ends and queue inserts
went both ways without echoes; YouTube Music trailed by 0.1–1.2 s.
- **Starting an album or playlist** publishes `queue.replace` and, about 0.2–0.5 s later,
  either a second `queue.replace` (same current song) or one `queue.insert` per song that
  Spotify adds once the context is loaded (nine for a 13-song album).
- **It resumes the context where it was left**, not at 0 (see the shared lessons).

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
10. **Not done yet:** a song missing on the Spotify side. Sessions with YouTube Music and with TIDAL were run live (`live_pair`).

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
  Seeks don't pass through the store. The adapter learns of them from the `<video>`'s
  `seeked` event (listened for in the capture phase on `window`, since media events don't
  bubble; verified live) and checks the position every 500 ms as a fallback.
- **A player can stay stuck "loading"**: seen right after a join, player state 3
  (buffering) with the `<video>` at 0, `readyState` 4 and 130 s buffered, for about
  100 s while the session played on (2026-11-13). Local changes wait 3 s
  (`BUFFERING_WAIT_MS`) for a loading track and then go ahead. The position read then
  (0 if the loaded video isn't the queue item's, else where it stalled) was compared
  with a baseline that expected 3 s of playing, published as a seek, and again every
  3 s: a TIDAL host heard its song restart every 3 s after a YouTube Music listener
  caught up, until she started another song (verified live, fixed). Positions read
  while the track is loading are now never taken for a seek; after one, the baseline
  is re-anchored silently unless the user's own `seeked` event came in between.
  Tests: "A player stuck buffering …", "A player holding another video …".
- Durations in the queue are whole seconds ("3:22").
- **Covers** are on `yt3.googleusercontent.com` (`=w544-h544-…`), besides
  `lh3.googleusercontent.com` and `i.ytimg.com`. That host was missing from the app's
  CSP, so YouTube Music covers didn't show (verified live, fixed).
- **Republishing the queue:** selecting a song further down publishes the page's whole
  queue (`queue.replace`), each song described as YouTube Music found it (no ISRC, its
  own cover and album). Entries the session already has now keep the session's track
  (`state::stamp_added_by`, client and server); before, one click replaced every TIDAL
  cover and ISRC in the session.
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
- **Featured artists named twice throw the search off.** For the session's "Get Lucky (feat.
  Pharrell Williams and Nile Rodgers)" by Daft Punk, Pharrell Williams, Nile Rodgers, the
  query "*title* Daft Punk Pharrell Williams" returned only covers and karaoke versions, so
  the song was reported missing. "Get Lucky Daft Punk", the full title with one artist, and
  the title with all three artists all found the original (ranked second, behind a cover).
  The adapter now searches the title without the "(feat. …)" part plus the main artist
  first, and the old query only if that finds no match.

**Waiting and echoes** (verified live against `WEB_REMIX 1.20261110.04.00` in Edge 157,
signed out, in an app window launched like Attune does, minimized through CDP; the adapter
was injected into the running page and driven step by step from outside; afterwards it
was disposed, the test queue cleared and the window restored)
- **Signed out plays fine.** Google's cookie screen ("Before you continue") came first in
  the fresh profile; "Reject all" leads to the app. No advertisement came up in these runs.
- **Timers while hidden:** visible, a 25 ms timeout took 26 ms; minimized and silent,
  about 1,000 ms. **Playing sound lifts it at once**, and selecting a song always starts
  it playing, so a paused join or advance runs its hold mostly unthrottled (25–33 ms
  timeouts right after). Unlike TIDAL, the paused hold is not where throttling hurts.
- **Where it does:** steps that start from a silent, hidden page. A paused advance took
  0.99 s with the new adapter and 2.0 s with the old one (waiting for the track and the
  hold's stable period, which is still a timer). Seeks, inserts, removals and pauses take
  1–8 ms either way (the change is visible at once). A play from silence took about
  280 ms in both: the sound lifts the throttling before the old polling noticed.
- **Hidden for more than 5 minutes** (and silent), Chromium also limits **chained** timers
  to one wake-up a minute: the sixth and later nested 25 ms timeouts took 21–60 s. A
  single timeout still took about 1 s. In that state a paused seek took 1 ms, a paused
  advance 2.0 s, and the user's seek on the `<video>` was published. Code that re-arms its
  own timer (the fallback checks, the 500 ms position poll) runs once a minute then;
  the waits don't depend on it. **Timers set by code that itself runs in a long timer
  chain inherit that limit:** a test script that slept between steps in the page made
  every adapter timer take 60 s (a paused join 55 s, a play wait 60 s, followed by a
  published `setPlaying` when the song finally started). Attune calls the adapter
  through `Runtime.evaluate`, which starts no chain.
- **User changes:** a seek on the `<video>` element was published after 21–190 ms
  through the `seeked` event (capture listener on `window`), so media events do reach
  it. The user's play and pause were published.
- **Echoes:** none, in five runs of join, seek, remove, play, pause and advance, plus
  inserts, with the window hidden. A song that played to its end during a test was
  published as an advance, as it should be.
- **Left to check live:** the signed-in server queue rewriting items some time after a
  store edit (new item numbers would be new entries, published as a replacement of the
  queue), and advertisements during a hold.

**Loading a long queue:** the adapter used to look up every song before starting the
current one; a 50-song playlist took about 20 s before anything played. Now the current
song is started (and held at the session's position) first and the rest is looked up
after: rejoining an 81-song playlist played in sync after 1.5 s; the rest followed 15 s
later (all found).

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
10. **Not done yet:** a session with TIDAL (one with Spotify was run live, `live_pair`), launching the
    browser from Attune (the code path exists, the test used a window started by hand),
    and the connect screen's sign-in button in the running app.

## 5. Preventing these problems instead of patching them

1. **Per-provider IDs in tracks.** Add an optional `providerIds: { tidal?: string, spotify?: string, youtube?: string }` to `TrackDescriptor` (a later protocol version; protocol 2 added only the replace's `playback`). Whoever has the track fills its own ID; same-provider clients skip search entirely, and others fill theirs in after a successful lookup. This fixes TIDAL problems 1 and 2 for same-provider groups and cuts search load in general. Unknown fields are dropped by v1 clients, hence v2.
2. **Record real traces to build mocks.** A debug mode in the adapter that records every store action and state diff (or PlayerAPI event) with timestamps during a scripted live run (load a queue, pause, seek, skip, song end). Mocks built from these traces behave like the real app. The paused-join bug passed its first fix because the mock was "politer" than TIDAL.
3. **Tag our own dispatches instead of time windows.** If the store lets us add a marker to actions (or we wrap `dispatch`), state changes caused by Attune can be told apart from the user's exactly, replacing the 300 ms and 5.5 s suppression windows. Partly done for TIDAL: the comparison base is reset after each remote change, and a position jump to where our last seek should land is recognised as ours. Wrapping `dispatch` alone can't do it there, as the app's sagas dispatch past it.
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
6. **A shared "hold until stable" helper.** TIDAL's adapter now has `holdUntil({matches, correct, stableMs, timeoutMs})`, used by `startLoadedTrack` and `settle`: it re-applies the target state until it holds. It is checked on every store change, not only on a timer (background timers run once a second). Spotify and YouTube Music now carry their own copies, together with `waitFor` and `waitUntilQuiet`, each on top of a "something changed" source (`watchPlayer`, `watchPage`). They differ only in that source, so move them into a small shared library injected with each adapter, where each adapter supplies its source.
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
