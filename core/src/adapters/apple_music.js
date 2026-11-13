/*
 * Attune adapter for Apple Music (music.apple.com in a Chromium browser).
 *
 * This script runs inside the page. All Apple Music implementation details
 * stay here; Rust only talks to the shared bridge contract.
 *
 * The page plays through MusicKit JS v3 (`window.MusicKit`), Apple's
 * player library for websites. Its instance has a documented API (queue,
 * playback, events, the catalog API), so this adapter needs far fewer
 * private details than the others. The few it uses (`queue.remove`,
 * `queue._queueItems`, the `<audio id="apple-music-player">` element) are
 * checked before use. What was observed live is collected in
 * PROVIDER_ADAPTERS.md.
 */
(async () => {
    "use strict";

    // Verified live: the page has a same-origin iframe (a commerce fetch
    // proxy) that gets this script too. MusicKit is only in the top frame;
    // elsewhere the adapter would wait for it and report a failure.
    if (window.top && window.top !== window) return null;

    const BRIDGE_KEY = "__ATTUNE_BRIDGE_V1__";
    const BRIDGE_VERSION = 1;
    const EVENT_BINDING = "__jamEvent";
    const APP_TIMEOUT_MS = 30_000;
    const SEARCH_TIMEOUT_MS = 12_000;
    const SEARCH_CONCURRENCY = 4;
    const SEARCH_LIMIT = 10;
    // MusicKit fires several events for one user action; handle the last.
    const EVENT_DEBOUNCE_MS = 150;
    // Seeks don't always come with an event: look for them this often.
    const POSITION_POLL_MS = 500;
    // A track that is loading or buffering is in between two states.
    const BUFFERING_WAIT_MS = 3_000;
    const REMOTE_CHANGE_SUPPRESSION_MS = 300;
    // How long the page may take to show a change we asked for.
    const CHANGE_TIMEOUT_MS = 5_000;
    // A track may take this long to start (a slow connection).
    const TRACK_START_TIMEOUT_MS = 10_000;
    // A loaded track is held at the session's state until it stayed there
    // this long; re-applied at most this often.
    const TRACK_HOLD_STABLE_MS = 500;
    const TRACK_HOLD_RETRY_MS = 250;
    // Waits check on MusicKit and media events (see `watchPage`); this is
    // only a safety net for a change that came without either.
    const FALLBACK_CHECK_MS = 250;
    // The player's <audio> reports these; they don't bubble, so they are
    // caught on the way down (capture), for every media element.
    const MEDIA_EVENTS = [
        "loadstart", "loadeddata", "canplay", "playing", "pause", "waiting",
        "seeking", "seeked", "timeupdate", "ended", "emptied",
    ];
    const MUSICKIT_EVENTS = [
        "playbackStateDidChange", "nowPlayingItemDidChange", "queueItemsDidChange",
        "queuePositionDidChange", "mediaPlaybackError",
        // So a repeat or shuffle press is noticed (and undone) at once.
        "repeatModeDidChange", "shuffleModeDidChange",
    ];
    const POSITION_TOLERANCE_MS = 1_000;
    // A position jump larger than this is a seek.
    const SEEK_THRESHOLD_MS = 1_500;
    // MusicKit carries out a seek this long after it is asked for.
    const SEEK_DEBOUNCE_MS = 250;
    const SETTLE_TIMEOUT_MS = 8_000;
    const SETTLE_STABLE_MS = 1_000;
    const SETTLE_PAUSED_MS = 1_500;
    const RECENT_EVENT_LIMIT = 200;
    const COVER_SIZE = 600;
    // MusicKit.PlaybackStates.
    const STATE = {
        NONE: 0, LOADING: 1, PLAYING: 2, PAUSED: 3, STOPPED: 4, ENDED: 5,
        SEEKING: 6, WAITING: 8, STALLED: 9, COMPLETED: 10,
    };
    const IN_BETWEEN = new Set([STATE.LOADING, STATE.SEEKING, STATE.WAITING, STATE.STALLED]);
    // MusicKit.PlayerRepeatMode.none and PlayerShuffleMode.off.
    const REPEAT_NONE = 0;
    const SHUFFLE_OFF = 0;

    const existingBridge = window[BRIDGE_KEY];
    if (existingBridge?.version === BRIDGE_VERSION && existingBridge.ready) {
        existingBridge.sendEvent("Ready");
        return existingBridge;
    }
    if (typeof existingBridge?.dispose === "function") {
        await existingBridge.dispose();
    }

    const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

    // ---------------------------------------------------------------------
    // App discovery
    // ---------------------------------------------------------------------

    let music = null;

    function discover() {
        const instance = window.MusicKit?.getInstance?.();
        const usable =
            instance &&
            typeof instance.setQueue === "function" &&
            typeof instance.changeToMediaAtIndex === "function" &&
            typeof instance.playAt === "function" &&
            typeof instance.playLater === "function" &&
            typeof instance.seekToTime === "function" &&
            typeof instance.addEventListener === "function" &&
            typeof instance.api?.music === "function" &&
            typeof instance.queue?.remove === "function" &&
            Array.isArray(instance.queue?.items);
        return usable ? instance : null;
    }

    async function waitForApp() {
        const deadline = performance.now() + APP_TIMEOUT_MS;
        while (performance.now() < deadline) {
            const found = discover();
            if (found) return found;
            await sleep(100);
        }
        throw new Error("Timed out waiting for Apple Music's player; this version may not be supported yet");
    }

    // Verified live: `setQueue` puts a new queue object in place, so the
    // queue is always read from the instance.
    const queueOf = () => music.queue;
    const audio = () => document.getElementById("apple-music-player");
    const storefront = () => music.storefrontId || music.storefrontCountryCode || "us";

    // ---------------------------------------------------------------------
    // Track metadata and matching
    // ---------------------------------------------------------------------

    class TrackLookupError extends Error {
        constructor(message) {
            super(message);
            this.name = "TrackLookupError";
        }
    }

    // Structured bridge error understood by Rust (Error::TrackUnavailable).
    function trackUnavailableError(failures) {
        const error = new Error(
            "Track unavailable on Apple Music: " + failures.map(failure => failure.error.message).join("; "),
        );
        error.code = "trackUnavailable";
        error.details = { entryIds: failures.map(failure => failure.entry.entryId) };
        return error;
    }

    function normalize(value) {
        return String(value ?? "")
            .normalize("NFKC")
            .toLowerCase()
            .replace(/[’‘]/g, "'")
            .replace(/["']/g, "")
            .replace(/[^\p{L}\p{N}]+/gu, " ")
            .trim()
            .replace(/\s+/g, " ");
    }

    // Services disagree on where featured artists go in a title.
    function normalizeTitle(value) {
        return normalize(stripSameRecordingTags(value));
    }

    /**
     * A title without the tags that still name the same recording: a
     * featured artist (services disagree on where it goes) and a remaster.
     * Spotify's "Hotel California - 2013 Remaster" is "Hotel California"
     * elsewhere. Remixes, live and other versions are different recordings
     * and keep their tags.
     */
    function stripSameRecordingTags(value) {
        return String(value ?? "")
            .replace(/\s*[([][^)\]]*\b(feat|ft|featuring|with)\b[^)\]]*[)\]]/gi, " ")
            .replace(/\s*[([][^)\]]*\bremaster(ed)?\b[^)\]]*[)\]]/gi, " ")
            .replace(/\s[-–]\s*[^-–]*\bremaster(ed)?\b[^-–]*$/i, " ")
            // "Rock You Like a Hurricane - 2015 - Remaster": the year is left.
            .replace(/\s[-–]\s*\d{4}\s*$/, " ")
            .replace(/\s[-–]\s*(feat|ft|featuring)\b.*$/i, " ")
            .replace(/\s+/g, " ")
            .trim();
    }

    /**
     * Artists from MusicKit's one-line credit. Verified live: "Get Lucky"
     * is by "Daft Punk, Pharrell Williams & Nile Rodgers", with three
     * artists among its relationships. A credit is split only when that
     * gives as many names as there are artists, so "Simon & Garfunkel" and
     * "Earth, Wind & Fire" stay one name.
     */
    function splitArtists(credit, artistCount) {
        const text = String(credit ?? "").trim();
        if (!text) return [];
        const parts = text.split(/\s*,\s*|\s+&\s+/).map(part => part.trim()).filter(Boolean);
        return artistCount > 1 && parts.length === artistCount ? parts : [text];
    }

    // Artwork URLs are templates: ".../{w}x{h}bb.jpg".
    function coverUrlOf(template) {
        if (typeof template !== "string") return null;
        const url = template.replace("{w}", COVER_SIZE).replace("{h}", COVER_SIZE).replace("{f}", "jpg");
        return url.startsWith("https://") && url.length <= 1024 ? url : null;
    }

    function finiteOrNull(value) {
        const number = Number(value);
        return value != null && Number.isFinite(number) ? Math.round(number) : null;
    }

    /** A track descriptor from catalog data or a queue's media item. */
    function descriptorOf(source) {
        const attributes = source?.attributes ?? {};
        const artistCount = source?.relationships?.artists?.data?.length ?? 0;
        return {
            title: String(attributes.name ?? source?.title ?? ""),
            artists: splitArtists(attributes.artistName ?? source?.artistName, artistCount),
            isrc: attributes.isrc ?? source?.isrc ?? null,
            album: attributes.albumName ?? source?.albumName ?? null,
            durationMs: finiteOrNull(attributes.durationInMillis ?? source?.playbackDuration),
            coverUrl: coverUrlOf(attributes.artwork?.url ?? source?.artworkURL),
        };
    }

    // ---------------------------------------------------------------------
    // Queue items
    // ---------------------------------------------------------------------
    //
    // Verified live: `queue.items` holds media item objects, played ones
    // first (history), then the current one at `queue.position`, then the
    // rest. The same song twice is two objects, and edits (`remove`,
    // `playAt`, `playLater`) keep the other objects, so an entry ID is kept
    // per object. `setQueue` makes new objects for everything.

    const entryIdByItem = new WeakMap();

    function entryIdOf(item) {
        if (!entryIdByItem.has(item)) entryIdByItem.set(item, crypto.randomUUID());
        return entryIdByItem.get(item);
    }

    // Songs MusicKit adds by itself after the queue (autoplay) aren't the
    // session's; they are switched off during a session anyway.
    function isAutoplayItem(queue, index) {
        return Boolean(queue._queueItems?.[index]?.isAutoplay);
    }

    /**
     * Index of the current item. Verified live: after `setQueue` without
     * playing, `position` is -1 and nothing is loaded; the first item is
     * then the one to play.
     */
    function currentIndex(queue = queueOf()) {
        const position = queue.position;
        return Number.isInteger(position) && position >= 0 && position < queue.items.length ? position : 0;
    }

    /** The queue from the current item on: `[{ item, songId, index }]`. */
    function readQueue() {
        const queue = queueOf();
        const items = queue.items ?? [];
        const start = currentIndex(queue);
        const result = [];
        for (let index = start; index < items.length; index += 1) {
            if (isAutoplayItem(queue, index)) continue;
            const item = items[index];
            if (item?.id == null) continue;
            result.push({ item, songId: String(item.id), index });
        }
        return result;
    }

    // ---------------------------------------------------------------------
    // Player state
    // ---------------------------------------------------------------------

    // Loading, seeking and buffering say nothing about play or pause, so
    // remember the last clear answer.
    let lastKnownPlaying = false;

    function readPlaying() {
        const state = music.playbackState;
        if (state === STATE.PLAYING) lastKnownPlaying = true;
        else if (!IN_BETWEEN.has(state)) lastKnownPlaying = false;
        return lastKnownPlaying;
    }

    const isBuffering = () => IN_BETWEEN.has(music.playbackState);

    /** The item the player has loaded, if any. */
    const loadedItem = () => music.nowPlayingItem ?? null;

    function readPositionMs() {
        if (!loadedItem()) return 0;
        // The <audio> element has milliseconds; MusicKit rounds to seconds.
        const element = audio();
        const seconds = Number(element?.currentTime ?? music.currentPlaybackTime);
        return Number.isFinite(seconds) ? Math.max(0, Math.round(seconds * 1000)) : 0;
    }

    function stateFromQueue(queue) {
        if (!queue.length) {
            return {
                queue: [],
                currentEntryId: null,
                playback: { isPlaying: false, positionMs: 0, positionCapturedAtMs: Date.now() },
            };
        }
        const entries = queue.map(entry => ({ entryId: entryIdOf(entry.item), track: descriptorOf(entry.item) }));
        // The player may still hold the previous track for a moment.
        const loaded = loadedItem() === queue[0].item;
        return {
            queue: entries,
            currentEntryId: entries[0].entryId,
            playback: {
                isPlaying: loaded && readPlaying(),
                positionMs: loaded ? readPositionMs() : 0,
                // The position is read live, so it is as of now.
                positionCapturedAtMs: Date.now(),
            },
        };
    }

    function getState() {
        return stateFromQueue(readQueue());
    }

    // ---------------------------------------------------------------------
    // Catalog lookups
    // ---------------------------------------------------------------------

    const resolvedSongIdCache = new Map();

    function withTimeout(promise, message) {
        let timer = null;
        const timeout = new Promise((_, reject) => {
            timer = setTimeout(() => reject(new TrackLookupError(message)), SEARCH_TIMEOUT_MS);
        });
        return Promise.race([Promise.resolve(promise), timeout]).finally(() => clearTimeout(timer));
    }

    // A song the storefront can play has play parameters.
    const playable = song => song?.type === "songs" && song.attributes?.playParams && song.id != null;

    /** Catalog songs with this ISRC (verified live: works signed out). */
    async function songsByIsrc(isrc) {
        const response = await withTimeout(
            music.api.music(`/v1/catalog/${storefront()}/songs`, { "filter[isrc]": isrc }),
            `Apple Music took too long to look up ISRC ${isrc}`,
        );
        return (response?.data?.data ?? []).filter(playable);
    }

    async function searchSongs(term) {
        const response = await withTimeout(
            music.api.music(`/v1/catalog/${storefront()}/search`, { term, types: "songs", limit: SEARCH_LIMIT }),
            `Apple Music search timed out for "${term}"`,
        );
        return (response?.data?.results?.songs?.data ?? []).filter(playable);
    }

    /**
     * Whether two normalized artist names are the same act. A name that
     * starts with the other one, as whole words, counts: YouTube Music
     * credits "Purple Rain" to "Prince & The Revolution", Spotify to
     * "Prince" (verified live). Very short names must match exactly.
     */
    function sameArtist(a, b) {
        if (a === b) return true;
        const [short, long] = a.length < b.length ? [a, b] : [b, a];
        return short.length >= 4 && long.startsWith(`${short} `);
    }

    function artistOverlap(trackA, trackB) {
        const artistsA = (trackA.artists ?? []).map(normalize).filter(Boolean);
        return (trackB.artists ?? []).map(normalize).filter(Boolean)
            .some(name => artistsA.some(other => sameArtist(name, other)));
    }

    function scoreCandidate(source, candidate) {
        if (source.isrc && candidate.isrc && source.isrc !== candidate.isrc) return -1;
        if (normalizeTitle(source.title) !== normalizeTitle(candidate.title)) return -1;
        if (!artistOverlap(source, candidate)) return -1;

        let score = 10; // Same title and at least one matching artist.
        if (normalize(source.title) === normalize(candidate.title)) score += 1;
        if (source.album && candidate.album && normalize(source.album) === normalize(candidate.album)) {
            score += 3;
        }
        if (Number.isFinite(source.durationMs) && Number.isFinite(candidate.durationMs)) {
            const difference = Math.abs(source.durationMs - candidate.durationMs);
            if (difference <= 2_500) score += 2;
            else if (difference > 8_000) score -= 2;
        }
        return score;
    }

    /**
     * Among releases of the same recording (one ISRC is on the album and on
     * every compilation, verified live: five for "Get Lucky"), the one on
     * the same album, else the closest length, else Apple's first.
     */
    function bestRelease(source, songs) {
        let best = null;
        let bestScore = -Infinity;
        for (const song of songs) {
            const candidate = descriptorOf(song);
            let score = 0;
            if (source.album && candidate.album && normalize(source.album) === normalize(candidate.album)) score += 3;
            if (Number.isFinite(source.durationMs) && Number.isFinite(candidate.durationMs) &&
                Math.abs(source.durationMs - candidate.durationMs) <= 2_500) {
                score += 1;
            }
            if (score > bestScore) {
                best = song;
                bestScore = score;
            }
        }
        return best;
    }

    /**
     * Search terms to try in turn: the title without featured artists and
     * remaster tags with the main artist first (featured artists named
     * twice throw searches off), the full title and two artists after.
     */
    function searchTerms(track) {
        const artists = (track.artists ?? []).filter(Boolean);
        const bareTitle = stripSameRecordingTags(track.title) || String(track.title ?? "");
        return [...new Set([
            `${bareTitle} ${artists.slice(0, 1).join(" ")}`.trim(),
            `${track.title} ${artists.slice(0, 2).join(" ")}`.trim(),
        ])];
    }

    async function resolveTrack(track) {
        const existingProviderId = track.providerTrackId ?? track.provider_track_id;
        if (track.provider === "AppleMusic" && existingProviderId) return String(existingProviderId);

        const cacheKey = [
            track.isrc ?? "",
            normalize(track.title),
            (track.artists ?? []).map(normalize).join("|"),
            normalize(track.album),
            track.durationMs ?? "",
        ].join("::");
        if (resolvedSongIdCache.has(cacheKey)) return resolvedSongIdCache.get(cacheKey);

        let found = null;
        if (track.isrc) {
            const songs = await songsByIsrc(track.isrc);
            if (songs.length) found = bestRelease(track, songs);
        }
        for (const term of found ? [] : searchTerms(track)) {
            const candidates = await searchSongs(term);
            // Sorting is stable: equal scores keep Apple's ranking.
            found = candidates
                .map(song => ({ song, score: scoreCandidate(track, descriptorOf(song)) }))
                .filter(candidate => candidate.score >= 0)
                .sort((a, b) => b.score - a.score)[0]?.song ?? null;
            if (found) break;
        }
        if (!found) throw new TrackLookupError(`No matching Apple Music song found for "${track.title}"`);
        const songId = String(found.id);
        resolvedSongIdCache.set(cacheKey, songId);
        return songId;
    }

    // Resolve every entry independently, a few at a time. Entries already
    // in the queue (`known`: entryId -> song ID) need no lookup.
    async function resolveEntries(entries, known) {
        const results = new Array(entries.length);
        let next = 0;
        async function worker() {
            while (next < entries.length) {
                const index = next;
                next += 1;
                const entry = entries[index];
                const knownId = known.get(entry.entryId);
                if (knownId) {
                    results[index] = { entry, songId: knownId };
                    continue;
                }
                try {
                    results[index] = { entry, songId: await resolveTrack(entry.track) };
                } catch (error) {
                    if (!(error instanceof TrackLookupError)) throw error;
                    results[index] = { entry, error };
                }
            }
        }
        await Promise.all(Array.from({ length: Math.min(SEARCH_CONCURRENCY, entries.length) }, worker));
        return results;
    }

    // ---------------------------------------------------------------------
    // Applying changes: suppression and waiting
    // ---------------------------------------------------------------------

    let applying = 0;
    let suppressLocalChangesUntil = 0;
    let applyChain = Promise.resolve();

    /**
     * Run `action` as a remote change: nothing the page does meanwhile is
     * reported as the user's doing. Remote changes run one after another.
     */
    function asRemoteChange(action) {
        const run = applyChain.then(async () => {
            applying += 1;
            try {
                return await action();
            } finally {
                applying -= 1;
                suppressLocalChangesUntil = Date.now() + REMOTE_CHANGE_SUPPRESSION_MS;
                rebase();
            }
        });
        applyChain = run.catch(() => {});
        return run;
    }

    /** Call `listener` on every MusicKit and media event. Returns a stop function. */
    function onPageChange(listener, mediaEvents = MEDIA_EVENTS, musicEvents = MUSICKIT_EVENTS) {
        const instance = music;
        musicEvents.forEach(type => instance?.addEventListener(type, listener));
        const target = typeof window.addEventListener === "function" ? window : null;
        mediaEvents.forEach(type => target?.addEventListener(type, listener, true));
        return () => {
            musicEvents.forEach(type => instance?.removeEventListener?.(type, listener));
            mediaEvents.forEach(type => target?.removeEventListener?.(type, listener, true));
        };
    }

    /**
     * Call `check` (in a microtask, outside MusicKit's dispatch) whenever
     * the page may have changed. Returns a function that stops it.
     *
     * While a Chromium page is hidden and silent its timers run once a
     * second (verified live in TIDAL and YouTube Music), which is exactly
     * while a song is held paused. Events aren't timers and arrive on time.
     */
    function watchPage(check) {
        let scheduled = false;
        let stopped = false;
        let fallback = null;
        const run = () => {
            scheduled = false;
            if (!stopped) check();
        };
        const schedule = () => {
            if (scheduled || stopped) return;
            scheduled = true;
            Promise.resolve().then(run);
        };
        const stopEvents = onPageChange(schedule);
        const poll = () => {
            schedule();
            fallback = setTimeout(poll, FALLBACK_CHECK_MS);
        };
        fallback = setTimeout(poll, FALLBACK_CHECK_MS);
        return () => {
            stopped = true;
            clearTimeout(fallback);
            stopEvents();
        };
    }

    /** Wait until `condition()` holds. False on timeout. */
    function waitFor(condition, timeoutMs = CHANGE_TIMEOUT_MS) {
        return new Promise(resolve => {
            let done = false;
            let timer = null;
            let stop = () => {};
            const holds = () => {
                try {
                    return Boolean(condition());
                } catch {
                    return false;
                }
            };
            const finish = value => {
                if (done) return;
                done = true;
                clearTimeout(timer);
                stop();
                resolve(value);
            };
            if (holds()) return finish(true);
            stop = watchPage(() => {
                if (holds()) finish(true);
            });
            timer = setTimeout(() => finish(holds()), timeoutMs);
        });
    }

    /** Resolve once `key()` hasn't changed for `quietMs`, or at the timeout. */
    function waitUntilQuiet(key, quietMs, timeoutMs) {
        const deadline = Date.now() + timeoutMs;
        return new Promise(resolve => {
            let last = key();
            let since = Date.now();
            let done = false;
            let timer = null;
            let stop = () => {};
            const finish = () => {
                if (done) return;
                done = true;
                clearTimeout(timer);
                stop();
                resolve();
            };
            const check = () => {
                if (done) return;
                const now = Date.now();
                const value = key();
                if (value !== last) {
                    last = value;
                    since = now;
                }
                const left = Math.min(quietMs - (now - since), deadline - now);
                if (left <= 0) return finish();
                clearTimeout(timer);
                timer = setTimeout(check, left);
            };
            stop = watchPage(check);
            check();
        });
    }

    /**
     * Re-apply a target state until it has held for `stableMs`: `correct()`
     * runs whenever `matches()` is false (at most every TRACK_HOLD_RETRY_MS)
     * and returns whether it did anything. Checked on page events, plus a
     * timer for the stable period. Resolves to whether it held.
     */
    function holdUntil({ timeoutMs, stableMs, matches, correct }) {
        const deadline = Date.now() + timeoutMs;
        return new Promise((resolve, reject) => {
            let matchingSince = null;
            let lastCorrected = 0;
            let done = false;
            let timer = null;
            let stop = () => {};
            const finish = (error, value) => {
                if (done) return;
                done = true;
                clearTimeout(timer);
                stop();
                if (error) reject(error);
                else resolve(value);
            };
            const check = () => {
                if (done) return;
                const now = Date.now();
                if (now >= deadline) return finish(null, false);
                try {
                    if (matches()) {
                        matchingSince ??= now;
                        if (now - matchingSince >= stableMs) return finish(null, true);
                    } else {
                        matchingSince = null;
                        if (now - lastCorrected >= TRACK_HOLD_RETRY_MS && correct()) lastCorrected = now;
                    }
                } catch (error) {
                    return finish(error);
                }
                const wait = matchingSince != null
                    ? stableMs - (now - matchingSince)
                    : TRACK_HOLD_RETRY_MS - (now - lastCorrected);
                clearTimeout(timer);
                timer = setTimeout(check, Math.max(1, Math.min(wait, deadline - now)));
            };
            stop = watchPage(check);
            check();
        });
    }

    // MusicKit's calls return promises that may reject for a change that
    // is simply superseded; the holds check the outcome instead.
    function quietly(promise) {
        Promise.resolve(promise).catch(error => console.warn("[Attune] Apple Music call failed", error));
    }

    // ---------------------------------------------------------------------
    // Playback actions
    // ---------------------------------------------------------------------

    /**
     * Keep the player silent while `action` runs. Verified live: there is
     * no way to load a song paused (`setQueue` without playing loads
     * nothing, and a seek then does nothing), so a paused start plays for
     * a moment before it is paused. Muting the <audio> hides that.
     *
     * Verified live: the page follows the element's mute into MusicKit's
     * volume (0) and saves that as the user's volume, and unmuting doesn't
     * bring it back. So MusicKit's volume is put back too.
     */
    async function muted(action) {
        const element = audio();
        if (!element) return action();
        const wasMuted = element.muted;
        const volume = music.volume;
        element.muted = true;
        try {
            return await action();
        } finally {
            const now = audio();
            if (now) now.muted = wasMuted;
            if (element !== now) element.muted = wasMuted;
            if (Number.isFinite(volume) && music && music.volume !== volume) music.volume = volume;
        }
    }

    async function setPlaying(isPlaying) {
        if (readPlaying() === Boolean(isPlaying) && !isBuffering()) return;
        if (isPlaying) {
            // Nothing loaded (a queue set without playing, or its end): start
            // the current item.
            if (!loadedItem() && readQueue().length) quietly(music.changeToMediaAtIndex(readQueue()[0].index));
            else quietly(music.play());
        } else {
            quietly(music.pause());
        }
        await waitFor(() => readPlaying() === Boolean(isPlaying) && !isBuffering(), 2_000);
    }

    /**
     * Verified live: MusicKit's player debounces seeks by about 250 ms, and
     * a new call cancels the one waiting. Re-seeking every 250 ms (as the
     * hold did) cancelled every seek, and none landed until the hold gave
     * up. So a seek is only asked for when none is on its way; the promise
     * settles when it has been carried out (or cancelled).
     */
    let seekPending = false;

    function seekTo(positionMs) {
        if (seekPending) return false;
        seekPending = true;
        Promise.resolve(music.seekToTime(positionMs / 1000))
            .catch(error => console.warn("[Attune] Apple Music seek failed", error))
            .finally(() => {
                seekPending = false;
            });
        return true;
    }

    async function seek(positionMs) {
        if (!Number.isFinite(positionMs) || positionMs < 0) {
            throw new TypeError("positionMs must be a non-negative number");
        }
        await waitFor(() => !seekPending, 2_000);
        seekTo(positionMs);
        await waitFor(() => Math.abs(readPositionMs() - positionMs) <= SEEK_THRESHOLD_MS && !isBuffering(), 2_000);
    }

    /**
     * Bring the loaded track to the session's position and play state and
     * keep it there until it stays put. Verified live: seeking and pausing
     * right after a start both hold.
     */
    async function holdPlayback(item, playback, calledAt, timeoutMs = CHANGE_TIMEOUT_MS) {
        if (!playback) return;
        const wantPlaying = Boolean(playback.isPlaying);
        const target = () => Math.max(0, (playback.positionMs ?? 0) + (wantPlaying ? Date.now() - calledAt : 0));
        const loaded = () => loadedItem() === item && !isBuffering();
        await holdUntil({
            timeoutMs,
            stableMs: TRACK_HOLD_STABLE_MS,
            matches: () => loaded() &&
                readPlaying() === wantPlaying &&
                Math.abs(readPositionMs() - target()) <= POSITION_TOLERANCE_MS,
            correct: () => {
                if (!loaded()) return false;
                if (Math.abs(readPositionMs() - target()) > POSITION_TOLERANCE_MS) {
                    // The seek lands 250 ms later; aim where the song will be.
                    seekTo(target() + (wantPlaying ? SEEK_DEBOUNCE_MS : 0));
                }
                if (readPlaying() !== wantPlaying) quietly(wantPlaying ? music.play() : music.pause());
                return true;
            },
        });
    }

    /** Start the queue item at `index` and hold it at `playback`. */
    async function startItem(index, playback, calledAt) {
        const run = async () => {
            quietly(music.changeToMediaAtIndex(index));
            const item = queueOf().items[index];
            await waitFor(() => loadedItem() === item, TRACK_START_TIMEOUT_MS);
            await holdPlayback(item, playback, calledAt, TRACK_START_TIMEOUT_MS);
        };
        return playback && !playback.isPlaying ? muted(run) : run();
    }

    // ---------------------------------------------------------------------
    // Queue writes
    // ---------------------------------------------------------------------
    //
    // Verified live:
    // - `setQueue({ songs })` replaces the queue. One ID it can't find
    //   fails the whole call and changes nothing.
    // - `queue.remove(index)`, `playAt(index, { song })` and
    //   `playLater({ songs })` edit the queue in place, without touching
    //   the current song.
    // - `changeToMediaAtIndex(index)` makes an item current and starts it.

    function knownSongIds(queue = readQueue()) {
        const known = new Map();
        for (const entry of queue) known.set(entryIdOf(entry.item), entry.songId);
        return known;
    }

    /**
     * Append `wanted` (`[{ entryId, songId }]`) to the queue. A batch with
     * a song Apple can't queue fails as a whole, so then they are added
     * one at a time. Resolves to the entries that couldn't be added.
     */
    async function append(wanted) {
        if (!wanted.length) return [];
        const before = queueOf().items.length;
        const added = [];
        try {
            await music.playLater({ songs: wanted.map(item => item.songId) });
            added.push(...wanted);
        } catch {
            for (const item of wanted) {
                try {
                    await music.playLater({ song: item.songId });
                    added.push(item);
                } catch {
                    // Left out; reported as unavailable.
                }
            }
        }
        const items = queueOf().items;
        added.forEach((item, offset) => {
            const shown = items[before + offset];
            if (shown) entryIdByItem.set(shown, item.entryId);
        });
        return wanted.filter(item => !added.includes(item)).map(item => item.entryId);
    }

    /** Remove the queue items at `indexes` (absolute), last first. */
    function removeAt(indexes) {
        [...indexes].sort((a, b) => b - a).forEach(index => queueOf().remove(index));
    }

    /**
     * Make `wanted` everything after the current item. What already
     * matches at the front is kept; the rest is removed and appended, so
     * the current song plays on.
     */
    async function writeUpcoming(wanted) {
        const upcoming = readQueue().slice(1);
        let same = 0;
        while (same < upcoming.length && same < wanted.length &&
            entryIdOf(upcoming[same].item) === wanted[same].entryId) {
            same += 1;
        }
        if (same === upcoming.length && same === wanted.length) return [];

        // Autoplay items after the queue go too: the session decides.
        const queue = queueOf();
        const firstStale = same < upcoming.length ? upcoming[same].index : null;
        const stale = [];
        for (let index = queue.items.length - 1; index > currentIndex(queue); index -= 1) {
            if ((firstStale != null && index >= firstStale) || isAutoplayItem(queue, index)) stale.push(index);
        }
        removeAt(stale);
        return append(wanted.slice(same));
    }

    /** Replace the whole queue with `head` and start it at `playback`. */
    async function replaceAndStart(head, playback, calledAt) {
        const start = async () => {
            await music.setQueue({
                songs: [head.songId],
                startPlaying: true,
                startTime: Math.max(0, playback?.positionMs ?? 0) / 1000,
            });
            const [item] = queueOf().items;
            if (!item) throw new Error("Apple Music could not queue the current song");
            entryIdByItem.set(item, head.entryId);
            await waitFor(() => loadedItem() === item, TRACK_START_TIMEOUT_MS);
            await holdPlayback(item, playback ?? { isPlaying: true, positionMs: 0 }, calledAt, TRACK_START_TIMEOUT_MS);
        };
        try {
            await (playback && !playback.isPlaying ? muted(start) : start());
        } catch (error) {
            if (error instanceof TrackLookupError) throw error;
            throw new Error(`Apple Music could not start the current song: ${error?.message ?? error}`);
        }
    }

    async function unload() {
        quietly(music.pause());
        await music.stop?.();
        await music.clearQueue?.();
        await waitFor(() => !readPlaying() && readQueue().length === 0, 2_000);
    }

    /**
     * Load `entries` (current first).
     *
     * Resolves to `{ unavailableEntryIds }`: upcoming entries without a
     * match are left out and reported. If the *current* entry has no
     * match, nothing is changed and a `trackUnavailable` error is thrown.
     * If the current entry is already playing it keeps playing.
     */
    async function loadQueue(entries, playback, calledAt = Date.now()) {
        if (!Array.isArray(entries)) throw new TypeError("Queue entries must be an array");
        if (entries.length === 0) {
            await unload();
            return { unavailableEntryIds: [] };
        }

        // The current entry first: if it can't play, nothing changes and the
        // rest isn't looked up. It starts before the rest is looked up, which
        // runs meanwhile (the lookups don't touch the queue).
        const known = knownSongIds();
        const [current] = await resolveEntries([entries[0]], known);
        if (current.error) throw trackUnavailableError([current]);
        const head = { entryId: current.entry.entryId, songId: current.songId };
        const lookups = resolveEntries(entries.slice(1), known);
        lookups.catch(() => {}); // awaited below; don't report it twice

        const queue = readQueue();
        const unavailable = [];
        const queued = queue.find(entry => entryIdOf(entry.item) === head.entryId);
        if (queue.length && entryIdOf(queue[0].item) === head.entryId) {
            // Already current: keep it, and line up the playback if asked.
            if (!loadedItem() && playback) await startItem(queue[0].index, playback, calledAt);
            else await holdPlayback(queue[0].item, playback, calledAt);
        } else if (queued) {
            // Already queued further down: move on to it.
            await startItem(queued.index, playback ?? { isPlaying: true, positionMs: 0 }, calledAt);
        } else {
            await replaceAndStart(head, playback, calledAt);
        }

        const upcoming = await lookups;
        unavailable.push(...upcoming.filter(result => result.error).map(failure => failure.entry.entryId));
        const wanted = upcoming.filter(result => !result.error)
            .map(result => ({ entryId: result.entry.entryId, songId: result.songId }));
        // The song may have ended during the lookups; then the queue moved on
        // and the session's own advance will follow.
        const now = readQueue();
        if (now.length && entryIdOf(now[0].item) === head.entryId) {
            unavailable.push(...await writeUpcoming(wanted));
        }
        return { unavailableEntryIds: unavailable };
    }

    // ---------------------------------------------------------------------
    // Session operations
    // ---------------------------------------------------------------------

    function cloneEntries(queue) {
        return queue.map(entry => ({
            entryId: entry.entryId,
            track: { ...entry.track, artists: [...(entry.track.artists ?? [])] },
        }));
    }

    function applySnapshot(nextState) {
        if (!nextState || !Array.isArray(nextState.queue) || !nextState.playback) {
            throw new TypeError("Invalid Attune music state");
        }
        const calledAt = Date.now();
        return asRemoteChange(async () => {
            const entries = nextState.queue;
            const start = nextState.currentEntryId == null
                ? 0
                : entries.findIndex(entry => entry.entryId === nextState.currentEntryId);
            if (start < 0) throw new Error(`Current queue entry was not found: ${nextState.currentEntryId}`);
            // Entries before the current one are history.
            return loadQueue(entries.slice(start), nextState.playback, calledAt);
        });
    }

    /**
     * Look entries up without changing the queue, so that loading them
     * later is quick (the lookups are cached). Not a remote change: it runs
     * beside them. Resolves to `{ unavailableEntryIds }`.
     */
    async function prefetch(entries) {
        if (!Array.isArray(entries)) throw new TypeError("Queue entries must be an array");
        const results = await resolveEntries(entries, knownSongIds());
        return { unavailableEntryIds: results.filter(result => result.error).map(result => result.entry.entryId) };
    }

    function applyOperation(operation) {
        if (!operation || typeof operation.type !== "string") {
            throw new TypeError("Invalid Attune operation");
        }
        const calledAt = Date.now();
        return asRemoteChange(() => applyOperationNow(operation, calledAt));
    }

    async function applyOperationNow(operation, calledAt) {
        const done = { unavailableEntryIds: [] };
        const state = getState();
        const queue = cloneEntries(state.queue);
        // A new current track starts from the beginning, playing.
        const fromStart = { isPlaying: true, positionMs: 0 };
        const indexOf = entryId => queue.findIndex(entry => entry.entryId === entryId);

        switch (operation.type) {
            case "queue.replace": {
                const entries = operation.entries ?? [];
                const currentEntryId = operation.currentEntryId ?? entries[0]?.entryId ?? null;
                const start = currentEntryId == null
                    ? 0
                    : entries.findIndex(entry => entry.entryId === currentEntryId);
                if (start < 0) throw new Error(`Current queue entry was not found: ${currentEntryId}`);
                const sameCurrent = currentEntryId != null && currentEntryId === state.currentEntryId;
                // Protocol 2: where the new current song is (a list may be
                // resumed part-way); without it, from 0, playing.
                const startAt = operation.playback
                    ? { isPlaying: Boolean(operation.playback.isPlaying), positionMs: operation.playback.positionMs ?? 0 }
                    : fromStart;
                return loadQueue(entries.slice(start), sameCurrent ? null : startAt, calledAt);
            }

            case "queue.insert": {
                const index = Math.max(0, Math.min(operation.index, queue.length));
                if (index === 0) {
                    queue.splice(0, 0, operation.entry);
                    return loadQueue(queue, fromStart, calledAt);
                }
                const [found] = await resolveEntries([operation.entry], new Map());
                if (found.error) return { unavailableEntryIds: [operation.entry.entryId] };
                // In place: the queue's own index of the session position.
                const shown = readQueue();
                const at = index < shown.length ? shown[index].index : queueOf().items.length;
                await music.playAt(at, { song: found.songId });
                const item = queueOf().items[at];
                if (item) entryIdByItem.set(item, operation.entry.entryId);
                return done;
            }

            case "queue.remove": {
                const index = indexOf(operation.entryId);
                if (index < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                if (index > 0) {
                    removeAt([readQueue()[index].index]);
                    return done;
                }
                queue.splice(index, 1);
                return loadQueue(queue, { ...fromStart, isPlaying: state.playback.isPlaying }, calledAt);
            }

            case "queue.move": {
                const from = indexOf(operation.entryId);
                if (from < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                if (operation.toIndex < 0 || operation.toIndex >= queue.length) {
                    throw new RangeError("Queue index is out of range");
                }
                const [entry] = queue.splice(from, 1);
                queue.splice(operation.toIndex, 0, entry);
                const currentChanged = queue[0].entryId !== state.currentEntryId;
                return loadQueue(queue, currentChanged ? fromStart : null, calledAt);
            }

            case "playback.select":
            case "playback.advance": {
                const targetId = operation.entryId ?? operation.targetEntryId;
                const target = indexOf(targetId);
                if (target < 0) throw new Error(`Playback target is not in the queue: ${targetId}`);
                const wanted = {
                    isPlaying: operation.isPlaying ?? true,
                    positionMs: operation.positionMs ?? 0,
                };
                // The page may have moved on by itself already (a song end
                // reaches every client): then only line up the playback.
                if (target === 0) {
                    const [current] = readQueue();
                    if (!loadedItem()) {
                        await startItem(current.index, wanted, calledAt);
                        return done;
                    }
                    const far = Math.abs(readPositionMs() - wanted.positionMs) > POSITION_TOLERANCE_MS + (Date.now() - calledAt);
                    if (far || readPlaying() !== wanted.isPlaying) await holdPlayback(current.item, wanted, calledAt);
                    return done;
                }
                // Selecting a later item turns what lies before it into history.
                return loadQueue(queue.slice(target), wanted, calledAt);
            }

            case "playback.seek": {
                if (operation.entryId !== state.currentEntryId) {
                    throw new Error("Cannot seek a track that is not currently playing");
                }
                await seek(operation.positionMs);
                return done;
            }

            case "playback.setPlaying": {
                await setPlaying(Boolean(operation.isPlaying));
                return done;
            }

            default:
                throw new Error(`Unsupported Attune operation: ${operation.type}`);
        }
    }

    // ---------------------------------------------------------------------
    // Neutral state
    // ---------------------------------------------------------------------

    /**
     * After attaching (which reloads the page), wait until the page has
     * stopped changing, then pause and keep it paused.
     */
    function settle({ pause = true } = {}) {
        return asRemoteChange(async () => {
            const key = () => JSON.stringify([readQueue().map(entry => entryIdOf(entry.item)), readPlaying()]);
            await waitUntilQuiet(key, SETTLE_STABLE_MS, SETTLE_TIMEOUT_MS);
            if (!pause) return;
            await holdUntil({
                timeoutMs: CHANGE_TIMEOUT_MS,
                stableMs: SETTLE_PAUSED_MS,
                matches: () => !readPlaying(),
                correct: () => {
                    quietly(music.pause());
                    return true;
                },
            });
        });
    }

    // The settings `prepareForSession` found, while a session lasts.
    let sessionSettings = null;

    /**
     * What a session needs: no autoplay (similar songs after the queue),
     * no repeat or shuffle, and `continuous` on. Verified live: signed out,
     * `continuous` is off and the player stops after every song instead of
     * moving on to the next.
     */
    function keepSessionSettings({ fromUser = false } = {}) {
        if (music.autoplayEnabled) music.autoplayEnabled = false;
        if (music.repeatMode !== REPEAT_NONE) {
            // Repeat belongs to the session: a press here is reported (the
            // runtime turns the session's repeat on or off), and MusicKit's
            // own goes off again.
            if (fromUser) sendOperation({ type: "playback.setRepeat", mode: "one" });
            music.repeatMode = REPEAT_NONE;
        }
        // Not yet shared: whether MusicKit's shuffle reorders the queue it
        // shows, and puts the old order back when switched off, isn't known
        // (PROVIDER_ADAPTERS.md). Until it's checked, it's simply undone.
        if (music.shuffleMode !== SHUFFLE_OFF) music.shuffleMode = SHUFFLE_OFF;
        if (music.continuous === false) music.continuous = true;
    }

    /** Switch to the session settings; returns the user's, for `restoreAfterSession`. */
    function prepareForSession() {
        return asRemoteChange(async () => {
            const previous = {
                autoplay: Boolean(music.autoplayEnabled),
                repeatMode: Number.isInteger(music.repeatMode) ? music.repeatMode : REPEAT_NONE,
                shuffleMode: Number.isInteger(music.shuffleMode) ? music.shuffleMode : SHUFFLE_OFF,
                continuous: typeof music.continuous === "boolean" ? music.continuous : true,
            };
            sessionSettings = previous;
            keepSessionSettings();
            return previous;
        });
    }

    /** Put back the settings `prepareForSession` returned. */
    function restoreAfterSession(previous) {
        sessionSettings = null;
        if (!previous || typeof previous !== "object") return Promise.resolve();
        return asRemoteChange(async () => {
            if (previous.autoplay === true) music.autoplayEnabled = true;
            if (Number.isInteger(previous.repeatMode) && previous.repeatMode !== REPEAT_NONE) {
                music.repeatMode = previous.repeatMode;
            }
            if (previous.continuous === false) music.continuous = false;
            // Shuffle is left off: turning it on reorders the queue.
        });
    }

    // ---------------------------------------------------------------------
    // Local state observation and operation emission
    // ---------------------------------------------------------------------

    let baseline = null;
    // Whether the baseline's position was read from a player that had the
    // track and wasn't buffering. Only such positions show a seek.
    let baselineSettled = false;
    // A `seeked` event that wasn't ours since the baseline: the user moved
    // the song, even if the player buffered meanwhile.
    let userSeeked = false;
    let eventTimer = null;
    let pollTimer = null;
    let bufferingSince = null;
    let unsubscribe = null;

    /** Whether the current track is still on its way (loading or buffering). */
    function trackLoading(queue = readQueue()) {
        if (!queue.length) return false;
        if (isBuffering()) return true;
        const loaded = loadedItem();
        // Nothing loaded is a stopped player (the end of the queue, or a
        // queue set without playing), not one on its way.
        return loaded != null && loaded !== queue[0].item;
    }

    function rebase() {
        try {
            const queue = readQueue();
            baseline = stateFromQueue(queue);
            baselineSettled = !trackLoading(queue);
        } catch {
            baseline = null;
            baselineSettled = false;
        }
        userSeeked = false;
    }

    const ids = queue => queue.map(entry => entry.entryId);
    const sameIds = (left, right) =>
        left.length === right.length && left.every((id, index) => id === right[index]);

    /**
     * The queue edit that turns `before` into `after` (same current entry),
     * as operations. Small edits stay small so other clients don't reload
     * their queue.
     */
    function queueEditOperations(before, after) {
        const beforeIds = ids(before);
        const afterIds = ids(after);
        if (sameIds(beforeIds, afterIds)) return [];

        const added = after.filter(entry => !beforeIds.includes(entry.entryId));
        const removed = beforeIds.filter(id => !afterIds.includes(id));

        if (removed.length === 0 && added.length > 0 && added.length <= 20 &&
            sameIds(beforeIds, afterIds.filter(id => beforeIds.includes(id)))) {
            return added.map(entry => ({
                type: "queue.insert",
                entry,
                index: afterIds.indexOf(entry.entryId),
            }));
        }
        if (added.length === 0 && removed.length > 0 && removed.length <= 20 &&
            sameIds(afterIds, beforeIds.filter(id => afterIds.includes(id)))) {
            return removed.map(entryId => ({ type: "queue.remove", entryId }));
        }
        if (added.length === 0 && removed.length === 0) {
            const first = beforeIds.findIndex((id, index) => id !== afterIds[index]);
            for (const moved of [beforeIds[first], afterIds[first]]) {
                if (sameIds(beforeIds.filter(id => id !== moved), afterIds.filter(id => id !== moved))) {
                    return [{ type: "queue.move", entryId: moved, toIndex: afterIds.indexOf(moved) }];
                }
            }
        }
        return null;
    }

    function positionWasSought(before, after) {
        if (before.currentEntryId !== after.currentEntryId) return false;
        if (before.playback.isPlaying !== after.playback.isPlaying) return false;
        const actual = after.playback.positionMs - before.playback.positionMs;
        const elapsed = after.playback.positionCapturedAtMs - before.playback.positionCapturedAtMs;
        const expected = before.playback.isPlaying ? Math.max(0, elapsed) : 0;
        return Math.abs(actual - expected) > SEEK_THRESHOLD_MS;
    }

    function emitLocalChanges() {
        if (sessionSettings) {
            keepSessionSettings({ fromUser: applying === 0 && Date.now() >= suppressLocalChangesUntil });
        }
        if (applying > 0 || Date.now() < suppressLocalChangesUntil) {
            rebase();
            return;
        }

        // A track that is loading or buffering hasn't arrived yet.
        const queue = readQueue();
        const loading = trackLoading(queue);
        if (loading && Date.now() - (bufferingSince ??= Date.now()) < BUFFERING_WAIT_MS) {
            scheduleEmit();
            return;
        }
        bufferingSince = null;

        const before = baseline;
        const beforeSettled = baselineSettled;
        const after = stateFromQueue(queue);
        baseline = after;
        baselineSettled = !loading;
        if (!before) return;

        const currentChanged = before.currentEntryId !== after.currentEntryId;
        let reported = false;

        if (currentChanged && after.currentEntryId && before.queue[1]?.entryId === after.currentEntryId) {
            const edits = queueEditOperations(before.queue.slice(1), after.queue);
            if (edits) {
                sendOperation({
                    type: "playback.advance",
                    targetEntryId: after.currentEntryId,
                    positionMs: after.playback.positionMs,
                    isPlaying: after.playback.isPlaying,
                });
                edits.forEach(sendOperation);
                reported = true;
            }
        } else if (!currentChanged) {
            const edits = queueEditOperations(before.queue, after.queue);
            if (edits) {
                edits.forEach(sendOperation);
                reported = true;
            }
        }
        if (!reported) {
            sendOperation({
                type: "queue.replace",
                entries: after.queue,
                currentEntryId: after.currentEntryId,
            });
        }

        if (before.playback.isPlaying !== after.playback.isPlaying) {
            sendOperation({ type: "playback.setPlaying", isPlaying: after.playback.isPlaying });
        }
        // A position read while the track is still on its way says nothing
        // (verified live with YouTube Music: a player stuck buffering
        // published a seek every 3 s). Once settled, only a real seek counts.
        let sought = false;
        if (!loading) {
            sought = beforeSettled
                ? positionWasSought(before, after)
                : userSeeked && before.currentEntryId === after.currentEntryId;
            userSeeked = false;
        }
        if (sought) {
            sendOperation({
                type: "playback.seek",
                entryId: after.currentEntryId,
                positionMs: after.playback.positionMs,
            });
        }

        if (currentChanged || sought || before.playback.isPlaying !== after.playback.isPlaying) {
            sendEvent("PlaystateChange", getPlayState());
        }
    }

    function runEmit() {
        try {
            emitLocalChanges();
        } catch (error) {
            sendEvent("Error", `Could not follow Apple Music's state: ${error.message}`);
        }
    }

    function scheduleEmit() {
        clearTimeout(eventTimer);
        eventTimer = setTimeout(() => {
            eventTimer = null;
            runEmit();
        }, EVENT_DEBOUNCE_MS);
    }

    function subscribe() {
        rebase();
        unsubscribe = onPageChange(event => {
            if (event?.type === "seeked" && applying === 0 && Date.now() >= suppressLocalChangesUntil) {
                userSeeked = true;
            }
            scheduleEmit();
        }, ["seeked"]);
        pollTimer = setInterval(() => {
            if (!eventTimer) runEmit();
        }, POSITION_POLL_MS);
    }

    // ---------------------------------------------------------------------
    // Legacy methods retained for the current Rust/UI call sites
    // ---------------------------------------------------------------------

    function getPlayState() {
        const [current] = readQueue();
        const loaded = current && loadedItem() === current.item;
        return {
            track: current ? {
                ...descriptorOf(current.item),
                provider: "AppleMusic",
                provider_track_id: current.songId,
                cover_url: descriptorOf(current.item).coverUrl,
            } : null,
            position_ms: loaded ? readPositionMs() : 0,
            timestamp_ms: Date.now(),
            playing: Boolean(loaded) && readPlaying(),
        };
    }

    // ---------------------------------------------------------------------
    // Lifecycle and public bridge API
    // ---------------------------------------------------------------------

    // The last events sent, for diagnosing live behaviour from the console.
    const recentEvents = [];

    function sendEvent(type, data) {
        const event = { type };
        if (data !== undefined) event.data = data;
        recentEvents.push({ at: Date.now(), ...event });
        if (recentEvents.length > RECENT_EVENT_LIMIT) recentEvents.shift();

        if (typeof window[EVENT_BINDING] !== "function") {
            console.warn("[Attune] CDP event binding is unavailable", type);
            return;
        }
        window[EVENT_BINDING](JSON.stringify(event));
    }

    function sendOperation(operation) {
        sendEvent("Operation", operation);
    }

    async function initialise() {
        music = await waitForApp();
        subscribe();
        bridge.ready = true;
        sendEvent("Ready");
        console.info("[Attune] Apple Music adapter ready");
    }

    async function dispose() {
        clearTimeout(eventTimer);
        clearInterval(pollTimer);
        eventTimer = null;
        pollTimer = null;
        unsubscribe?.();
        unsubscribe = null;
        music = null;
        baseline = null;
        sessionSettings = null;
        resolvedSongIdCache.clear();
        bridge.ready = false;

        if (window[BRIDGE_KEY] === bridge) delete window[BRIDGE_KEY];
        if (window.bridge === bridge) delete window.bridge;
    }

    const bridge = {
        version: BRIDGE_VERSION,
        ready: false,
        initialise,
        dispose,
        sendEvent,
        recentEvents,
        getState,
        getSnapshot: getState,
        applySnapshot,
        applyOperation,
        prefetch,
        settle,
        prepareForSession,
        restoreAfterSession,

        // Existing controller commands.
        getPlayState,
        playTrack: track => asRemoteChange(async () => {
            const songId = await resolveTrack(track);
            await replaceAndStart({ entryId: crypto.randomUUID(), songId }, null, Date.now());
        }),
        pause: () => asRemoteChange(() => setPlaying(false)),
        resume: () => asRemoteChange(() => setPlaying(true)),
        seek: positionMs => asRemoteChange(() => seek(positionMs)),
        next: () => {
            const next = getState().queue[1];
            if (!next) return Promise.resolve();
            return applyOperation({ type: "playback.advance", targetEntryId: next.entryId, positionMs: 0, isPlaying: true });
        },
        previous: () => asRemoteChange(() => seek(0)),
        addToQueue: tracks => asRemoteChange(async () => {
            if (!readQueue().length) throw new Error("Nothing is playing in Apple Music");
            const songIds = await Promise.all(tracks.map(resolveTrack));
            await append(songIds.map(songId => ({ entryId: crypto.randomUUID(), songId })));
        }),
        moveQueueItem: (fromIndex, toIndex) => {
            const entry = getState().queue[fromIndex];
            if (!entry) throw new RangeError("Queue index is out of range");
            return applyOperation({ type: "queue.move", entryId: entry.entryId, toIndex });
        },
        removeFromQueue: index => {
            const entry = getState().queue[index];
            if (!entry) throw new RangeError("Queue index is out of range");
            return applyOperation({ type: "queue.remove", entryId: entry.entryId });
        },
    };

    window[BRIDGE_KEY] = bridge;
    window.bridge = bridge;

    try {
        await initialise();
    } catch (error) {
        console.error("[Attune] Apple Music adapter initialization failed", error);
        sendEvent("Error", error?.message ?? String(error));
    }

    return bridge;
})();
