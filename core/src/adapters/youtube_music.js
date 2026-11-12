/*
 * Attune adapter for YouTube Music (music.youtube.com in a Chromium browser).
 *
 * This script runs inside the page. All YouTube Music implementation
 * details stay here; Rust only talks to the shared bridge contract.
 *
 * YouTube Music's scripts have generated names and its code is minified,
 * so nothing here depends on either. Everything is reached from two custom
 * elements with fixed tag names (`ytmusic-app`, `ytmusic-player-bar`),
 * their named properties, and the action names of the app's Redux store.
 * These are private and may change, so each one is checked before use.
 * What was observed live is collected in PROVIDER_ADAPTERS.md.
 */
(async () => {
    "use strict";

    const BRIDGE_KEY = "__ATTUNE_BRIDGE_V1__";
    const BRIDGE_VERSION = 1;
    const EVENT_BINDING = "__jamEvent";
    const APP_TIMEOUT_MS = 30_000;
    const SEARCH_TIMEOUT_MS = 12_000;
    const SEARCH_CONCURRENCY = 4;
    // The store changes many times for one user action; handle the last.
    const EVENT_DEBOUNCE_MS = 150;
    // The player's position isn't in the store: look for seeks this often.
    const POSITION_POLL_MS = 500;
    // A track that is loading or buffering is in between two states.
    const BUFFERING_WAIT_MS = 3_000;
    const REMOTE_CHANGE_SUPPRESSION_MS = 300;
    // How long the page may take to show a change we asked for.
    const CHANGE_TIMEOUT_MS = 5_000;
    // A track may take this long to start (a slow connection).
    const TRACK_START_TIMEOUT_MS = 10_000;
    // A session change that arrives during an advertisement waits this
    // long for it to end (there can be two in a row).
    const AD_BREAK_TIMEOUT_MS = 120_000;
    // A loaded track is held at the session's state until it stayed there
    // this long; re-applied at most this often.
    const TRACK_HOLD_STABLE_MS = 500;
    const TRACK_HOLD_RETRY_MS = 250;
    // Waits check on store changes and media events (see `watchPage`);
    // this is only a safety net for a change that came without either.
    const FALLBACK_CHECK_MS = 250;
    // The player's <video> reports these; they don't bubble, so they are
    // caught on the way down (capture), for every media element.
    const MEDIA_EVENTS = [
        "loadstart", "loadeddata", "canplay", "playing", "pause", "waiting",
        "seeking", "seeked", "timeupdate", "ended", "emptied",
    ];
    const POSITION_TOLERANCE_MS = 1_000;
    // A position jump larger than this is a seek.
    const SEEK_THRESHOLD_MS = 1_500;
    const SETTLE_TIMEOUT_MS = 8_000;
    const SETTLE_STABLE_MS = 1_000;
    const SETTLE_PAUSED_MS = 1_500;
    // `music/get_queue` takes several video IDs per request.
    const QUEUE_FETCH_BATCH = 50;
    const RECENT_EVENT_LIMIT = 200;
    // The "Songs" filter of YouTube Music's search.
    const SEARCH_SONGS_PARAMS = "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D";
    // YouTube player states.
    const PLAYER = { UNSTARTED: -1, ENDED: 0, PLAYING: 1, PAUSED: 2, BUFFERING: 3, CUED: 5 };
    const REPEAT_OFF = "NONE";

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

    let store = null;
    let queueService = null;
    let playerApi = null;
    let network = null;

    // The elements forward to a controller (`inst`) in current builds.
    function controllerOf(selector) {
        const element = document.querySelector(selector);
        return element?.inst ?? element ?? null;
    }

    function discover() {
        const app = controllerOf("ytmusic-app");
        const bar = controllerOf("ytmusic-player-bar");
        const found = {
            store: bar?.store ?? app?.store,
            queueService: bar?.queue ?? app?.queue,
            playerApi: bar?.playerApi,
            network: app?.networkManager,
        };
        const usable =
            typeof found.store?.getState === "function" &&
            typeof found.store?.dispatch === "function" &&
            typeof found.store?.subscribe === "function" &&
            typeof found.queueService?.selectQueueItem === "function" &&
            typeof found.playerApi?.getPlayerState === "function" &&
            typeof found.playerApi?.seekTo === "function" &&
            typeof found.network?.fetch === "function" &&
            Array.isArray(found.store.getState()?.queue?.items);
        return usable ? found : null;
    }

    async function waitForApp() {
        const deadline = performance.now() + APP_TIMEOUT_MS;
        while (performance.now() < deadline) {
            const found = discover();
            if (found) return found;
            await sleep(100);
        }
        throw new Error("Timed out waiting for YouTube Music's player; this version may not be supported yet");
    }

    const appState = () => store.getState();
    const dispatch = (type, payload) => store.dispatch(payload === undefined ? { type } : { type, payload });

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
            "Track unavailable on YouTube Music: " + failures.map(failure => failure.error.message).join("; "),
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
        return normalize(String(value ?? "")
            .replace(/[([][^)\]]*\b(feat|ft|featuring|with)\b[^)\]]*[)\]]/gi, " ")
            .replace(/\s[-–]\s*(feat|ft|featuring)\b.*$/i, " "));
    }

    const runsText = text => (text?.runs ?? []).map(run => run.text ?? "").join("");

    function pageTypeOf(run) {
        return run?.navigationEndpoint?.browseEndpoint?.browseEndpointContextSupportedConfigs
            ?.browseEndpointContextMusicConfig?.pageType ?? null;
    }

    // "3:22" or "1:02:03" in milliseconds.
    function parseDuration(text) {
        const parts = String(text ?? "").trim().split(":").map(Number);
        if (parts.length < 2 || parts.length > 3 || parts.some(part => !Number.isFinite(part))) return null;
        return parts.reduce((total, part) => total * 60 + part, 0) * 1000;
    }

    /**
     * Artists and album from a byline such as "Artist & Other • Album • 2019".
     * Artists and albums are links to their pages; without links (videos,
     * uploads) the first part of the line names the artists.
     */
    function parseByline(text) {
        const runs = text?.runs ?? [];
        let artists = runs.filter(run => pageTypeOf(run) === "MUSIC_PAGE_TYPE_ARTIST").map(run => run.text);
        const album = runs.find(run => pageTypeOf(run) === "MUSIC_PAGE_TYPE_ALBUM")?.text ?? null;
        if (!artists.length) {
            const first = runsText(text).split(" • ")[0] ?? "";
            artists = first.split(/\s*(?:,|&)\s*/).map(name => name.trim());
        }
        return { artists: artists.filter(Boolean), album };
    }

    function finiteOrNull(value) {
        const number = Number(value);
        return value != null && Number.isFinite(number) ? Math.round(number) : null;
    }

    // ---------------------------------------------------------------------
    // Queue items
    // ---------------------------------------------------------------------
    //
    // A queue item is a `playlistPanelVideoRenderer`, or a wrapper around
    // one when a song also has a video version. Each item in the queue has
    // a number of its own in `navigationEndpoint.watchEndpoint.index`,
    // handed out from `queue.nextQueueItemId`.

    function rendererOf(item) {
        return item?.playlistPanelVideoRenderer ??
            item?.playlistPanelVideoWrapperRenderer?.primaryRenderer?.playlistPanelVideoRenderer ?? null;
    }

    function counterpartOf(item) {
        return item?.playlistPanelVideoWrapperRenderer?.counterpart?.[0]?.counterpartRenderer
            ?.playlistPanelVideoRenderer ?? null;
    }

    function videoIdOf(item) {
        const renderer = rendererOf(item);
        return renderer?.videoId ?? renderer?.navigationEndpoint?.watchEndpoint?.videoId ?? null;
    }

    function queueNumberOf(item) {
        const number = rendererOf(item)?.navigationEndpoint?.watchEndpoint?.index;
        return typeof number === "number" ? number : null;
    }

    function setQueueNumber(item, number) {
        for (const renderer of [rendererOf(item), counterpartOf(item)]) {
            const endpoint = renderer?.navigationEndpoint?.watchEndpoint;
            if (endpoint) endpoint.index = number;
        }
    }

    // The session only takes https art of a sensible length.
    function coverUrlOf(url) {
        return typeof url === "string" && url.startsWith("https://") && url.length <= 1024 ? url : null;
    }

    function descriptorOf(item) {
        const renderer = rendererOf(item);
        const { artists, album } = parseByline(renderer?.longBylineText ?? renderer?.shortBylineText);
        return {
            title: runsText(renderer?.title),
            artists,
            // YouTube Music doesn't expose ISRCs.
            isrc: null,
            album,
            durationMs: parseDuration(runsText(renderer?.lengthText)),
            coverUrl: coverUrlOf(renderer?.thumbnail?.thumbnails?.at(-1)?.url),
        };
    }

    // Queue numbers start again from 0 when the queue is cleared, so they
    // identify an item only within one "generation" of the queue.
    let generation = 0;
    let lastNextQueueItemId = 0;
    const entryIdByKey = new Map();

    function noteGeneration() {
        const next = appState().queue.nextQueueItemId ?? 0;
        if (next < lastNextQueueItemId) generation += 1;
        lastNextQueueItemId = next;
    }

    function keyOf(item, position) {
        const number = queueNumberOf(item);
        return number == null
            ? `${generation}:video:${videoIdOf(item)}:${position}`
            : `${generation}:${number}`;
    }

    function entryIdFor(key) {
        if (!entryIdByKey.has(key)) entryIdByKey.set(key, crypto.randomUUID());
        return entryIdByKey.get(key);
    }

    /**
     * Index of the current item. Verified live: `queue.selectedItemIndex`
     * is left behind when the queue is replaced (1 item, index 6), so ask
     * the queue service, which looks for the item marked as selected.
     */
    function currentIndex(state = appState()) {
        const items = state.queue?.items ?? [];
        const marked = items.findIndex(item => rendererOf(item)?.selected === true);
        if (marked >= 0) return marked;
        const fromService = queueService?.getCurrentItemIndex?.();
        const index = Number.isInteger(fromService) ? fromService : state.queue?.selectedItemIndex;
        return Number.isInteger(index) && index >= 0 && index < items.length ? index : 0;
    }

    /** The queue from the current item on: `[{ key, videoId, item }]`. */
    function readQueue(state = appState()) {
        noteGeneration();
        const items = state.queue?.items ?? [];
        const selected = currentIndex(state);
        // Items before the selected one have been played.
        return items.slice(selected)
            .map((item, offset) => ({ key: keyOf(item, selected + offset), videoId: videoIdOf(item), item }))
            .filter(entry => entry.videoId);
    }

    // ---------------------------------------------------------------------
    // Player state
    // ---------------------------------------------------------------------

    const isAdPlaying = (state = appState()) => Boolean(state.player?.adPlaying);

    // Loading and buffering say nothing about play or pause, so remember
    // the last clear answer.
    let lastKnownPlaying = false;

    function readPlaying() {
        const playerState = playerApi.getPlayerState();
        if (playerState === PLAYER.PLAYING) lastKnownPlaying = true;
        else if (playerState === PLAYER.PAUSED || playerState === PLAYER.ENDED || playerState === PLAYER.CUED) {
            lastKnownPlaying = false;
        }
        return lastKnownPlaying;
    }

    // An advertisement plays in a player of its own; the store follows it.
    const isAnythingPlaying = () =>
        isAdPlaying() ? Boolean(appState().player?.isPlaying) : readPlaying();

    const isBuffering = () => {
        const playerState = playerApi.getPlayerState();
        return playerState === PLAYER.BUFFERING || playerState === PLAYER.UNSTARTED;
    };

    function readPositionMs() {
        const seconds = Number(playerApi.getCurrentTime());
        return Number.isFinite(seconds) ? Math.max(0, Math.round(seconds * 1000)) : 0;
    }

    const loadedVideoId = () => playerApi.getVideoData?.()?.video_id ?? null;

    function stateFromQueue(queue) {
        if (!queue.length) {
            return {
                queue: [],
                currentEntryId: null,
                playback: { isPlaying: false, positionMs: 0, positionCapturedAtMs: Date.now() },
            };
        }
        const entries = queue.map(entry => ({ entryId: entryIdFor(entry.key), track: descriptorOf(entry.item) }));
        // The player may still hold the previous track for a moment.
        const loaded = loadedVideoId() === queue[0].videoId;
        return {
            queue: entries,
            currentEntryId: entries[0].entryId,
            playback: {
                // During an advertisement the track itself is not playing.
                isPlaying: !isAdPlaying() && readPlaying(),
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
    // Search and lookups
    // ---------------------------------------------------------------------

    const resolvedVideoIdCache = new Map();

    function withTimeout(promise, message) {
        const timeout = new Promise((_, reject) => setTimeout(
            () => reject(new TrackLookupError(message)), SEARCH_TIMEOUT_MS,
        ));
        return Promise.race([Promise.resolve(promise), timeout]);
    }

    function collectRows(node, rows = [], depth = 0) {
        if (!node || typeof node !== "object" || depth > 16) return rows;
        if (node.musicResponsiveListItemRenderer) {
            rows.push(node.musicResponsiveListItemRenderer);
            return rows;
        }
        for (const value of Object.values(node)) collectRows(value, rows, depth + 1);
        return rows;
    }

    /** YouTube Music's song search. Resolves to candidates in its ranking. */
    async function searchSongs(query) {
        const response = await withTimeout(
            network.fetch("/search", { query, params: SEARCH_SONGS_PARAMS }),
            `YouTube Music search timed out for "${query}"`,
        );
        if (!response || typeof response !== "object") {
            throw new Error("YouTube Music's search returned an unexpected response");
        }

        return collectRows(response.contents).map(row => {
            const videoId = row.playlistItemData?.videoId;
            const columns = (row.flexColumns ?? [])
                .map(column => column.musicResponsiveListItemFlexColumnRenderer?.text);
            if (!videoId || !columns[0]) return null;
            const { artists, album } = parseByline(columns[1]);
            const durationText = (columns[1]?.runs ?? []).map(run => run.text).reverse()
                .find(text => parseDuration(text) != null);
            return {
                videoId,
                track: {
                    title: runsText(columns[0]),
                    artists,
                    isrc: null,
                    album,
                    durationMs: parseDuration(durationText),
                },
            };
        }).filter(Boolean);
    }

    function artistOverlap(trackA, trackB) {
        const artistsA = new Set((trackA.artists ?? []).map(normalize).filter(Boolean));
        return (trackB.artists ?? []).map(normalize).some(name => artistsA.has(name));
    }

    function scoreCandidate(source, candidate) {
        if (normalizeTitle(source.title) !== normalizeTitle(candidate.track.title)) return -1;
        if (!artistOverlap(source, candidate.track)) return -1;

        let score = 10; // Same title and at least one matching artist.
        if (normalize(source.title) === normalize(candidate.track.title)) score += 1;
        if (source.album && candidate.track.album &&
            normalize(source.album) === normalize(candidate.track.album)) {
            score += 3;
        }
        if (Number.isFinite(source.durationMs) && Number.isFinite(candidate.track.durationMs)) {
            // Durations here are whole seconds.
            const difference = Math.abs(source.durationMs - candidate.track.durationMs);
            if (difference <= 2_500) score += 2;
            else if (difference > 8_000) score -= 2;
        }
        return score;
    }

    async function resolveTrack(track) {
        const existingProviderId = track.providerTrackId ?? track.provider_track_id;
        if (track.provider === "YoutubeMusic" && existingProviderId) return String(existingProviderId);

        const cacheKey = [
            normalize(track.title),
            (track.artists ?? []).map(normalize).join("|"),
            normalize(track.album),
            track.durationMs ?? "",
        ].join("::");
        if (resolvedVideoIdCache.has(cacheKey)) return resolvedVideoIdCache.get(cacheKey);

        const query = `${track.title} ${(track.artists ?? []).slice(0, 2).join(" ")}`.trim();
        const candidates = await searchSongs(query);
        // Sorting is stable: equal scores keep YouTube Music's ranking.
        const best = candidates
            .map(candidate => ({ ...candidate, score: scoreCandidate(track, candidate) }))
            .filter(candidate => candidate.score >= 0)
            .sort((a, b) => b.score - a.score)[0];
        if (!best) {
            throw new TrackLookupError(`No matching YouTube Music song found for "${track.title}"`);
        }
        resolvedVideoIdCache.set(cacheKey, best.videoId);
        return best.videoId;
    }

    // Resolve every entry independently, a few at a time. Entries already
    // in the queue (`known`: entryId -> video ID) need no search.
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
                    results[index] = { entry, videoId: knownId };
                    continue;
                }
                try {
                    results[index] = { entry, videoId: await resolveTrack(entry.track) };
                } catch (error) {
                    if (!(error instanceof TrackLookupError)) throw error;
                    results[index] = { entry, error };
                }
            }
        }
        await Promise.all(Array.from({ length: Math.min(SEARCH_CONCURRENCY, entries.length) }, worker));
        return results;
    }

    /**
     * Queue items for video IDs, as the app itself gets them when a song is
     * added to the queue. Resolves to a map; IDs that can't be queued
     * (removed or blocked videos) are missing from it.
     */
    async function fetchQueueItems(videoIds) {
        const items = new Map();
        const unique = [...new Set(videoIds)];
        for (let start = 0; start < unique.length; start += QUEUE_FETCH_BATCH) {
            const response = await withTimeout(
                network.fetch("/music/get_queue", { videoIds: unique.slice(start, start + QUEUE_FETCH_BATCH) }),
                "YouTube Music took too long to prepare the queue",
            );
            for (const data of response?.queueDatas ?? []) {
                const videoId = videoIdOf(data?.content);
                if (videoId && !items.has(videoId)) items.set(videoId, data.content);
            }
        }
        return items;
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

    /**
     * Wait for an advertisement to end. Nothing can be loaded or sought
     * meanwhile, and the track starts by itself afterwards.
     */
    async function adBreakOver() {
        if (!isAdPlaying()) return;
        if (!await waitFor(() => !isAdPlaying(), AD_BREAK_TIMEOUT_MS)) {
            throw new Error("YouTube Music is still playing advertisements");
        }
        // The track takes a moment to start after the advertisement.
        await waitFor(() => !isBuffering(), CHANGE_TIMEOUT_MS);
    }

    /** Call `listener` on every store change and media event. Returns a stop function. */
    function onPageChange(listener, mediaEvents = MEDIA_EVENTS) {
        const unsubscribeStore = store?.subscribe?.(listener);
        const target = typeof window.addEventListener === "function" ? window : null;
        mediaEvents.forEach(type => target?.addEventListener(type, listener, true));
        return () => {
            if (typeof unsubscribeStore === "function") unsubscribeStore();
            mediaEvents.forEach(type => target?.removeEventListener?.(type, listener, true));
        };
    }

    /**
     * Call `check` (in a microtask, outside the store's dispatch) whenever
     * the page may have changed. Returns a function that stops it.
     *
     * Waits used to poll every 25 ms. While a Chromium page is hidden and
     * silent its timers run once a second (verified live in TIDAL; assumed
     * for YouTube Music's app window, not measured), which is exactly while
     * a song is held paused. Store changes and media events aren't timers
     * and arrive on time.
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
     * and returns whether it did anything; `gaveUp()` ends the hold early.
     * Checked on store changes and media events, plus a timer for the
     * stable period. Resolves to whether it held.
     */
    function holdUntil({ timeoutMs, stableMs, matches, correct, gaveUp = () => false }) {
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
                    if (gaveUp()) return finish(null, false);
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

    // ---------------------------------------------------------------------
    // Playback actions
    // ---------------------------------------------------------------------

    async function setPlaying(isPlaying) {
        if (readPlaying() === Boolean(isPlaying) && !isBuffering()) return;
        if (isPlaying) playerApi.playVideo();
        else playerApi.pauseVideo();
        await waitFor(() => readPlaying() === Boolean(isPlaying) && !isBuffering(), 2_000);
    }

    async function seek(positionMs) {
        if (!Number.isFinite(positionMs) || positionMs < 0) {
            throw new TypeError("positionMs must be a non-negative number");
        }
        // Seconds; `true` allows seeking past what is buffered.
        playerApi.seekTo(positionMs / 1000, true);
        await waitFor(() => Math.abs(readPositionMs() - positionMs) <= SEEK_THRESHOLD_MS, 2_000);
    }

    /**
     * Bring the loaded track to the session's position and play state and
     * keep it there until it stays put.
     *
     * Verified live: selecting a queue item always starts it from 0,
     * playing, and there is no way to load it paused. So the position and
     * play state are re-applied until they hold. An advertisement may come
     * first; then this gives up, and the session is rejoined afterwards.
     */
    async function holdPlayback(videoId, playback, calledAt, timeoutMs = CHANGE_TIMEOUT_MS) {
        if (!playback) return;
        const wantPlaying = Boolean(playback.isPlaying);
        const target = () => Math.max(0, (playback.positionMs ?? 0) + (wantPlaying ? Date.now() - calledAt : 0));
        const loaded = () => loadedVideoId() === videoId && !isBuffering();
        await holdUntil({
            timeoutMs,
            stableMs: TRACK_HOLD_STABLE_MS,
            // An advertisement first: the session is rejoined after it.
            gaveUp: isAdPlaying,
            matches: () => loaded() &&
                readPlaying() === wantPlaying &&
                Math.abs(readPositionMs() - target()) <= POSITION_TOLERANCE_MS,
            correct: () => {
                if (!loaded()) return false;
                if (Math.abs(readPositionMs() - target()) > POSITION_TOLERANCE_MS) {
                    playerApi.seekTo(target() / 1000, true);
                }
                if (readPlaying() !== wantPlaying) {
                    if (wantPlaying) playerApi.playVideo();
                    else playerApi.pauseVideo();
                }
                return true;
            },
        });
    }

    // ---------------------------------------------------------------------
    // Queue writes
    // ---------------------------------------------------------------------
    //
    // Verified live: the queue lives in the store and the player follows
    // it. `UPDATE_ITEMS` replaces everything after the current item and
    // never touches playback; `CLEAR` + `ADD_ITEMS` replace the whole
    // queue; the queue service's `selectQueueItem` makes an item current
    // and starts it. Items before the current one are history.

    function knownVideoIds(queue = readQueue()) {
        const known = new Map();
        for (const entry of queue) known.set(entryIdFor(entry.key), entry.videoId);
        return known;
    }

    /**
     * Items for `wanted` (`[{ entryId, videoId }]`): the queue's own item
     * where the entry is already queued, a new one otherwise. Entries whose
     * video can't be queued come back in `missing`.
     */
    async function buildItems(wanted, queue) {
        const existing = new Map(queue.map(entry => [entryIdFor(entry.key), entry]));
        const needed = wanted
            .filter(item => existing.get(item.entryId)?.videoId !== item.videoId)
            .map(item => item.videoId);
        const fetched = needed.length ? await fetchQueueItems(needed) : new Map();

        let nextNumber = appState().queue.nextQueueItemId ?? 0;
        const built = [];
        const missing = [];
        for (const item of wanted) {
            const current = existing.get(item.entryId);
            if (current?.videoId === item.videoId) {
                built.push({ entryId: item.entryId, item: current.item });
                continue;
            }
            const template = fetched.get(item.videoId);
            if (!template) {
                missing.push(item.entryId);
                continue;
            }
            // Each occurrence needs an item object and a number of its own.
            const fresh = JSON.parse(JSON.stringify(template));
            setQueueNumber(fresh, nextNumber);
            nextNumber += 1;
            built.push({ entryId: item.entryId, item: fresh });
        }
        return { built, missing };
    }

    function rememberEntryIds(built, offset) {
        const queue = readQueue();
        built.forEach((entry, index) => {
            const shown = queue[index + offset];
            if (shown) entryIdByKey.set(shown.key, entry.entryId);
        });
    }

    /** Make `wanted` everything after the current item. */
    async function writeUpcoming(wanted) {
        const queue = readQueue();
        const upcoming = queue.slice(1);
        const unchanged = upcoming.length === wanted.length &&
            upcoming.every((entry, index) => entryIdFor(entry.key) === wanted[index].entryId);
        if (unchanged) return [];

        const { built, missing } = await buildItems(wanted, upcoming);
        const state = appState();
        dispatch("UPDATE_ITEMS", {
            currentIndex: currentIndex(state),
            items: built.map(entry => entry.item),
            // The numbers were assigned in `buildItems`.
            shouldAssignIds: false,
            nextQueueItemId: state.queue.nextQueueItemId,
        });
        rememberEntryIds(built, 1);
        return missing;
    }

    /** Replace the whole queue with `wanted` and start its first item. */
    async function replaceAndStart(wanted) {
        const { built, missing } = await buildItems(wanted, []);
        if (!built.length || built[0].entryId !== wanted[0].entryId) {
            throw new Error("YouTube Music could not queue the current song");
        }
        dispatch("CLEAR");
        noteGeneration();
        dispatch("ADD_ITEMS", {
            items: built.map(entry => entry.item),
            index: 0,
            // After a clear the numbers start again; let the app assign them.
            shouldAssignIds: true,
            nextQueueItemId: appState().queue.nextQueueItemId ?? 0,
        });
        rememberEntryIds(built, 0);
        queueService.selectQueueItem(appState().queue.items[0]);
        return missing;
    }

    /** Make the queued item for `entryId` the current one. */
    function selectEntry(entryId) {
        const entry = readQueue().find(candidate => entryIdFor(candidate.key) === entryId);
        if (!entry) return false;
        queueService.selectQueueItem(entry.item);
        return true;
    }

    async function unload() {
        playerApi.pauseVideo();
        dispatch("CLEAR");
        noteGeneration();
        await waitFor(() => !readPlaying(), 2_000);
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

        // Resolve everything before touching the queue, the current entry
        // first: if it can't play, don't search for the rest.
        const known = knownVideoIds();
        const [current] = await resolveEntries([entries[0]], known);
        if (current.error) throw trackUnavailableError([current]);
        const upcoming = await resolveEntries(entries.slice(1), known);
        const failures = upcoming.filter(result => result.error);
        const unavailable = failures.map(failure => failure.entry.entryId);
        const wanted = [current, ...upcoming.filter(result => !result.error)]
            .map(result => ({ entryId: result.entry.entryId, videoId: result.videoId }));

        const queue = readQueue();
        const currentEntryId = queue.length ? entryIdFor(queue[0].key) : null;
        let started = false;
        if (currentEntryId === wanted[0].entryId) {
            unavailable.push(...await writeUpcoming(wanted.slice(1)));
        } else if (selectEntry(wanted[0].entryId)) {
            // Already queued further down: move on to it.
            started = true;
            unavailable.push(...await writeUpcoming(wanted.slice(1)));
        } else {
            started = true;
            unavailable.push(...await replaceAndStart(wanted));
        }

        if (started) {
            await waitFor(() => isAdPlaying() || loadedVideoId() === wanted[0].videoId, TRACK_START_TIMEOUT_MS);
        }
        await holdPlayback(wanted[0].videoId, playback, calledAt, started ? TRACK_START_TIMEOUT_MS : CHANGE_TIMEOUT_MS);
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
            await adBreakOver();
            const entries = nextState.queue;
            const start = nextState.currentEntryId == null
                ? 0
                : entries.findIndex(entry => entry.entryId === nextState.currentEntryId);
            if (start < 0) throw new Error(`Current queue entry was not found: ${nextState.currentEntryId}`);
            // Entries before the current one are history.
            return loadQueue(entries.slice(start), nextState.playback, calledAt);
        });
    }

    function applyOperation(operation) {
        if (!operation || typeof operation.type !== "string") {
            throw new TypeError("Invalid Attune operation");
        }
        const calledAt = Date.now();
        return asRemoteChange(async () => {
            await adBreakOver();
            return applyOperationNow(operation, calledAt);
        });
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
                return loadQueue(entries.slice(start), sameCurrent ? null : fromStart, calledAt);
            }

            case "queue.insert": {
                const index = Math.max(0, Math.min(operation.index, queue.length));
                queue.splice(index, 0, operation.entry);
                return loadQueue(queue, index === 0 ? fromStart : null, calledAt);
            }

            case "queue.remove": {
                const index = indexOf(operation.entryId);
                if (index < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                queue.splice(index, 1);
                return loadQueue(queue, index === 0 ? { ...fromStart, isPlaying: state.playback.isPlaying } : null, calledAt);
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
                    const videoId = readQueue()[0]?.videoId;
                    const far = Math.abs(readPositionMs() - wanted.positionMs) > POSITION_TOLERANCE_MS + (Date.now() - calledAt);
                    if (far || readPlaying() !== wanted.isPlaying) await holdPlayback(videoId, wanted, calledAt);
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
            const key = () => JSON.stringify([readQueue().map(entry => entry.key), isAnythingPlaying(), isAdPlaying()]);
            await waitUntilQuiet(key, SETTLE_STABLE_MS, SETTLE_TIMEOUT_MS);
            if (!pause) return;

            // Verified live: after a reload the page restores its last song
            // and may start it, behind an advertisement.
            await holdUntil({
                timeoutMs: CHANGE_TIMEOUT_MS,
                stableMs: SETTLE_PAUSED_MS,
                matches: () => !isAnythingPlaying(),
                correct: () => {
                    playerApi.pauseVideo();
                    return true;
                },
            });
        });
    }

    // The settings `prepareForSession` found, while a session lasts.
    let sessionSettings = null;

    /**
     * Verified live: starting a song turns autoplay back on, unless the
     * app believes the user chose the setting. So mark it as chosen, and
     * switch it off again should it come back.
     */
    function keepAutoplayOff() {
        const queue = appState().queue ?? {};
        if (queue.hasUserChangedDefaultAutoplayMode !== true) {
            dispatch("HAS_USER_CHANGED_DEFAULT_AUTOPLAY_MODE", true);
        }
        if (queue.autoplay !== false) dispatch("SET_AUTOPLAY_ENABLED", false);
        if (queue.repeatMode && queue.repeatMode !== REPEAT_OFF) dispatch("SET_REPEAT", REPEAT_OFF);
    }

    /**
     * Turn off what a session can't follow: autoplay (similar songs after
     * the queue) and repeat. Returns the user's settings, for
     * `restoreAfterSession`. Shuffle here is a one-time reordering of the
     * queue, not a mode, so there is nothing to switch off.
     */
    function prepareForSession() {
        return asRemoteChange(async () => {
            const queue = appState().queue ?? {};
            const previous = {
                autoplay: typeof queue.autoplay === "boolean" ? queue.autoplay : null,
                autoplayChosen: Boolean(queue.hasUserChangedDefaultAutoplayMode),
                repeatMode: typeof queue.repeatMode === "string" ? queue.repeatMode : REPEAT_OFF,
            };
            sessionSettings = previous;
            keepAutoplayOff();
            return previous;
        });
    }

    /** Put back the settings `prepareForSession` returned. */
    function restoreAfterSession(previous) {
        sessionSettings = null;
        if (!previous || typeof previous !== "object") return Promise.resolve();
        return asRemoteChange(async () => {
            if (previous.autoplayChosen === false) dispatch("HAS_USER_CHANGED_DEFAULT_AUTOPLAY_MODE", false);
            if (previous.autoplay === true) dispatch("SET_AUTOPLAY_ENABLED", true);
            if (typeof previous.repeatMode === "string" && previous.repeatMode !== REPEAT_OFF) {
                dispatch("SET_REPEAT", previous.repeatMode);
            }
        });
    }

    // ---------------------------------------------------------------------
    // Local state observation and operation emission
    // ---------------------------------------------------------------------

    let baseline = null;
    let eventTimer = null;
    let pollTimer = null;
    let bufferingSince = null;
    let unsubscribe = null;
    let interrupted = false;

    function rebase() {
        try {
            baseline = getState();
        } catch {
            baseline = null;
        }
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

    /**
     * Advertisements interrupt playback for listeners without a
     * subscription. The session goes on without this client meanwhile:
     * tell the runtime, which steps out and rejoins afterwards.
     */
    function noteInterruption() {
        const active = isAdPlaying();
        if (active === interrupted) return active;
        interrupted = active;
        sendEvent("Interruption", { active, reason: "advertisement" });
        if (!active) rebase();
        return active;
    }

    function emitLocalChanges() {
        if (sessionSettings) keepAutoplayOff();
        if (noteInterruption()) return;
        if (applying > 0 || Date.now() < suppressLocalChangesUntil) {
            rebase();
            return;
        }

        // A track that is loading or buffering hasn't arrived yet.
        const queue = readQueue();
        const loading = queue.length > 0 && (isBuffering() || loadedVideoId() !== queue[0].videoId);
        if (loading && Date.now() - (bufferingSince ??= Date.now()) < BUFFERING_WAIT_MS) {
            scheduleEmit();
            return;
        }
        bufferingSince = null;

        const before = baseline;
        const after = stateFromQueue(queue);
        baseline = after;
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
        const sought = positionWasSought(before, after);
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
            sendEvent("Error", `Could not follow YouTube Music's state: ${error.message}`);
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
        interrupted = isAdPlaying();
        // Seeks don't pass through the store: the <video>'s `seeked` event
        // reports them, the poll is the fallback.
        unsubscribe = onPageChange(scheduleEmit, ["seeked"]);
        pollTimer = setInterval(() => {
            if (!eventTimer) runEmit();
        }, POSITION_POLL_MS);
    }

    // ---------------------------------------------------------------------
    // Legacy methods retained for the current Rust/UI call sites
    // ---------------------------------------------------------------------

    function getPlayState() {
        const [current] = readQueue();
        const renderer = rendererOf(current?.item);
        const thumbnails = renderer?.thumbnail?.thumbnails ?? [];
        return {
            track: current ? {
                ...descriptorOf(current.item),
                provider: "YoutubeMusic",
                provider_track_id: current.videoId,
                cover_url: thumbnails[thumbnails.length - 1]?.url ?? null,
            } : null,
            position_ms: current ? readPositionMs() : 0,
            timestamp_ms: Date.now(),
            playing: Boolean(current) && !isAdPlaying() && readPlaying(),
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
        ({ store, queueService, playerApi, network } = await waitForApp());
        noteGeneration();
        subscribe();
        bridge.ready = true;
        sendEvent("Ready");
        console.info("[Attune] YouTube Music adapter ready");
    }

    async function dispose() {
        clearTimeout(eventTimer);
        clearInterval(pollTimer);
        eventTimer = null;
        pollTimer = null;
        unsubscribe?.();
        unsubscribe = null;
        store = null;
        queueService = null;
        playerApi = null;
        network = null;
        baseline = null;
        sessionSettings = null;
        entryIdByKey.clear();
        resolvedVideoIdCache.clear();
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
        settle,
        prepareForSession,
        restoreAfterSession,

        // Existing controller commands.
        getPlayState,
        playTrack: track => asRemoteChange(async () => {
            const videoId = await resolveTrack(track);
            await replaceAndStart([{ entryId: crypto.randomUUID(), videoId }]);
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
            const queue = readQueue();
            if (!queue.length) throw new Error("Nothing is playing in YouTube Music");
            const videoIds = await Promise.all(tracks.map(resolveTrack));
            await writeUpcoming([
                ...queue.slice(1).map(entry => ({ entryId: entryIdFor(entry.key), videoId: entry.videoId })),
                ...videoIds.map(videoId => ({ entryId: crypto.randomUUID(), videoId })),
            ]);
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
        console.error("[Attune] YouTube Music adapter initialization failed", error);
        sendEvent("Error", error?.message ?? String(error));
    }

    return bridge;
})();
