# Live cross-client test plan

Every music client in one real session, on the deployed relay, driven with
`live_pair` (`core/examples/live_pair.rs`): Attune's own session runtime per
client, plus `eval` to press each app's own buttons. After every step,
`compare` must say **ALL OK** (same song, same play state, positions within
2.5 s, every queue equal to the session's playable entries) within a few
seconds, unless the step says otherwise.

Clients: **T** TIDAL (desktop), **S** Spotify (desktop), **Y** YouTube Music
(web, Attune's browser window), **A** Apple Music (web). All signed in.

"From each" means the step is run once with each client as the origin and
the other three watching: 4 runs.

## 1. Joining

| # | Step | Expect |
|---|---|---|
| 1.1 | T hosts, playing a playlist; S, Y, A join one after another | Everyone plays the host's song at its position; songs a client lacks show "not here" for it |
| 1.2 | Join while the session is paused | Joiner loads the song paused at the position, no sound |
| 1.3 | Leave and join again from each (`leave`, `join`) | Back in step; the session keeps going for the rest |
| 1.4 | `exists` for the running session, then after everyone left | `Some(true)`, then `Some(false)` (new relay check) |

## 2. Playback from each app's own controls (from each)

| # | Step | Expect |
|---|---|---|
| 2.1 | Pause in the app | Everyone pauses within ~0.5 s, same position |
| 2.2 | Play in the app | Everyone resumes together |
| 2.3 | Seek in the app (bar / +30 s) | Everyone jumps there |
| 2.4 | Next in the app | Everyone moves to the same next song, at 0 |
| 2.5 | Previous / restart in the app | Session restarts or goes back consistently |
| 2.6 | Song ends on its own | Everyone moves on together, nothing skipped twice |

## 3. Queue changes from each app (from each)

| # | Step | Expect |
|---|---|---|
| 3.1 | Add a song to the app's queue ("play next" / "add to queue") | Appears in every queue; matched on each service or marked "not here" |
| 3.2 | Remove an upcoming song in the app (where the app can) | Gone everywhere |
| 3.3 | Start a different playlist/album in the app | It becomes the session's queue; everyone plays its first song |
| 3.4 | Shuffle on in the app | New order everywhere, current song uninterrupted, app's own shuffle back off |
| 3.5 | Repeat in the app | Session repeat toggles; app's own repeat back off |

## 4. Attune's own queue controls (from each member's runtime)

| # | Step | Expect |
|---|---|---|
| 4.1 | `remove` an upcoming entry | Gone everywhere, current song uninterrupted |
| 4.2 | `move` an entry | Same order everywhere |
| 4.3 | `playnow` an entry further down | Everyone plays it from 0 |
| 4.4 | `repeat one`, let the song end, `repeat off` | Everyone restarts it once; then moves on |

## 5. Songs not everyone has

| # | Step | Expect |
|---|---|---|
| 5.1 | Make a song only some have current | Those without it wait ("WaitingForNextSong"), rejoin at the next one |
| 5.2 | A song another listener on the same service found (shared ID) | Played without a search; if that ID won't load, matched here (fix from 2026-11-16) |
| 5.3 | YouTube Music music video vs song | Song version shared; others match it |

## 6. Host lock and roles

| # | Step | Expect |
|---|---|---|
| 6.1 | Host turns "others can change" off; each listener pauses, skips, shuffles | Undone in that app; session unchanged; a notice |
| 6.2 | Host's own changes while locked | Followed by everyone |
| 6.3 | Host leaves a locked session | Longest present listener hosts, session opens up |
| 6.4 | Unlock, listener changes | Followed |

## 7. Lifecycle

| # | Step | Expect |
|---|---|---|
| 7.1 | `stepout` one client, change songs elsewhere, `resync` | Stepped-out app left alone, then back in step |
| 7.2 | Empty session queue, a client presses play | Its queue becomes the session's |
| 7.3 | Last one leaves | Session gone (`exists` → false) |

## Results (2026-11-18, deployed relay `5915da7`)

Hosts rotated over five sessions: TIDAL, Spotify, YouTube Music, TIDAL, TIDAL.
**Apple Music** has no subscription on this account (`me/account` says
`subscription.active: false`), so it plays previews only and steps out, as
designed; it was in every session but couldn't follow. **YouTube Music** was
signed out (Google refuses sign-in in Attune's debug window); it plays fine
that way. All windows were hidden, as they are in normal use.

| # | Result |
|---|---|
| 1.1 | ✓ TIDAL, Spotify and YouTube Music as hosts; joins 0.8–10 s |
| 1.2 | ✓ joined a paused session at the host's position, 0 ms off |
| 1.3 | ✓ YouTube Music left and rejoined (1.2 s); Spotify left as host and rejoined |
| 1.4 | ✓ ended session `Some(false)`, live one `Some(true)` |
| 2.1–2.2 | ✓ from all three, **after bug 1** |
| 2.3 | ✓ seek bars of all three, within 0.8 s |
| 2.4 | ✓ Next from all three; TIDAL is slow to start a remote Next while hidden (known, see PROVIDER_ADAPTERS.md) and once didn't start at all (**bug 4**) |
| 2.5 | ✓ Previous mid-song and at the start: everyone restarts the song |
| 2.6 | ✓ songs ending on their own, several times |
| 3.1 | ✓ TIDAL "Play next", Spotify "Add to queue" (a whole playlist, **bugs 2 and 3**); YouTube Music's menus don't open in a hidden window, not tested |
| 3.2 | ✓ TIDAL's queue panel, **after bug 5** |
| 3.3 | ✓ a new 216-song list from TIDAL, **after bug 6** |
| 3.4 | ✓ shuffle in all three: new order everywhere, current song uninterrupted |
| 3.5 | ✓ repeat in all three toggles the session's repeat |
| 4.1–4.3 | ✓ remove, move, play now |
| 4.4 | ✓ repeat one: everyone restarted the song once |
| 5.1 | ✓ YouTube Music lacked a song, waited, rejoined at the next |
| 5.2 | not live (one client per service); unit tests |
| 5.3 | not tested (YouTube Music signed out) |
| 6.1–6.4 | ✓ lock undoes listeners' pause and Next with a notice; host's changes followed; host leaving hands over (TIDAL) and opens the session |
| 7.1 | ✓ step out, Next elsewhere, resync |
| 7.2 | ✓ Spotify's play in an empty session shared its 73 songs |

### Bugs found and fixed

1. **Pauses added up to a gap between apps.** `playback.setPlaying` has no
   position: the server takes it as of when it accepts the pause, while each
   app stops wherever its player is. Every pause and play left TIDAL 0.1–0.3 s
   further behind and Spotify further ahead (2.3 s apart after four cycles,
   below the 2 s drift tolerance per app, so never corrected). Fix: after a
   pause the runtime moves each app to the session's paused position
   (`align_paused_position`). Live after: 0–250 ms on every pause, no growth.
2. **Spotify's queue grew beyond the session's.** Spotify shows only a
   window of a long playlist and one more song each time one ends. When
   Spotify moved on itself it shared that song; when it moved on for someone
   else's song end, the song went into its baseline unreported. Fix: after a
   remote advance the adapter publishes songs Spotify added (`noteRevealed`).
   Live after: the session kept 81 songs through three remote Nexts.
3. **A song added past the end of the session's queue was refused** (index
   out of range): Spotify's index counted its extra songs; the refusal
   resynced Spotify, which didn't remove them. Fix: the runtime puts such an
   insert at the session's end. Live after: accepted, no resync.
4. **A hidden TIDAL that failed to start a song stayed silent**, shown in
   sync, until the next song: its start timed out without an error, and the
   drift check ignored a paused app. Fix: when the session plays and an app
   sits paused on its song for 3 s, the runtime seeks it and plays
   (`restart_if_stopped`, at most 3 times a song). Not reproduced live
   after; unit test.
5. **TIDAL sent every queue edit as a replace of the whole queue** (one
   removal: 78 songs), which the server refuses as stale if someone else
   edited meanwhile, undoing the edit. Fix: small inserts, removes and moves,
   as the other adapters send. Live after: `queue.remove`.
6. **One refused YouTube request stopped YouTube Music following.** Looking
   up a 216-song list, `music/get_queue` answered 503 "The service is
   currently unavailable"; YouTube's network manager rejects with a plain
   object, which failed the whole list ("applyOperation failed: [object
   Object]") and put YouTube Music in Error. Fix: searches and queue fetches
   are tried again (1 s, 3 s); a refusal that goes on costs only those songs.
   Bridge errors that aren't `Error`s are now shown as JSON. Live after: the
   list loaded in 30 s, one batch left out.

### Seen, not changed

- After a resume Spotify tends to run 0.3–0.9 s ahead (most when it resumed
  itself); it resets at the next pause and stays under the drift tolerance.
- A hidden Spotify window doesn't repaint: its clock and shuffle button show
  stale states. Don't read Spotify's UI to check its state.
- Spotify matched "Mr. Brightside" to a compilation ("Nu Rock") rather than
  "Hot Fuss"; same recording, different album.
- Test harness only: moving Spotify's page with `history.pushState` left it
  on an address its reload can't serve, and Spotify handed that address to
  the default browser. Spotify's own navigation keeps `index.html`.
