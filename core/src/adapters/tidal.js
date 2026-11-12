/*
 * Attune adapter for TIDAL Desktop.
 *
 * This script runs inside TIDAL's renderer. It deliberately keeps all TIDAL
 * implementation details here; Rust only talks to the shared bridge contract.
 * TIDAL's Redux interfaces are private and may change between app releases, so
 * every discovered dependency is checked before it is used.
 */
(async () => {
    "use strict";

    const BRIDGE_KEY = "__ATTUNE_BRIDGE_V1__";
    const BRIDGE_VERSION = 1;
    const EVENT_BINDING = "__jamEvent";
    const STORE_TIMEOUT_MS = 15_000;
    const SEARCH_TIMEOUT_MS = 12_000;
    const STORE_EVENT_DEBOUNCE_MS = 180;
    // How long a loaded track may take to start before its position and
    // play state are applied anyway.
    const TRACK_START_TIMEOUT_MS = 5_000;
    const TRACK_START_POLL_MS = 25;
    // Holding a loaded track at the session's state: done once it matched
    // this long; re-applied at most this often; this close counts as a match.
    const TRACK_HOLD_STABLE_MS = 500;
    const TRACK_HOLD_RETRY_MS = 250;
    const TRACK_HOLD_TOLERANCE_MS = 1_000;
    // Waiting for TIDAL's restore after attaching: done once its queue and play
    // state stopped changing for this long (longer while the queue is empty).
    const SETTLE_TIMEOUT_MS = 8_000;
    const SETTLE_STABLE_MS = 1_000;
    const SETTLE_EMPTY_MS = 3_000;
    // A pause after the restore must hold this long (a late auto-resume).
    const SETTLE_PAUSED_MS = 1_500;
    // TIDAL's repeat modes: off, all, one.
    const REPEAT_OFF = 0;
    // Local changes are ignored this long after applying a remote change, to
    // cover its immediate store updates (one debounced event). Keep it short:
    // a 1.2 s window swallowed real song-end advances (verified live). Later
    // echoes match the session and are dropped by the Rust runtime.
    const REMOTE_CHANGE_SUPPRESSION_MS = 300;
    const SOURCE_CONTEXT = { id: null, type: "UNKNOWN" };

    // A page can be reloaded or the init script can be evaluated more than
    // once. Reuse a compatible installation instead of subscribing twice.
    const existingBridge = window[BRIDGE_KEY];
    if (existingBridge?.version === BRIDGE_VERSION && existingBridge.ready) {
        existingBridge.sendEvent("Ready");
        return existingBridge;
    }
    if (typeof existingBridge?.dispose === "function") {
        await existingBridge.dispose();
    }

    // ---------------------------------------------------------------------
    // Runtime resource discovery
    // ---------------------------------------------------------------------

    const scriptUrls = new Set();

    function rememberScript(url) {
        if (typeof url === "string" && url.endsWith(".js")) {
            scriptUrls.add(url);
        }
    }

    performance.getEntriesByType("resource").forEach(entry => {
        rememberScript(entry.name);
    });

    const resourceObserver = new PerformanceObserver(entries => {
        entries.getEntries().forEach(entry => rememberScript(entry.name));
    });
    resourceObserver.observe({ type: "resource", buffered: true });

    function getScriptUrls() {
        document.querySelectorAll("script[src]").forEach(script => {
            rememberScript(script.src);
        });
        return [...scriptUrls];
    }

    async function importTidalModules() {
        const modules = [];

        for (const url of getScriptUrls()) {
            try {
                modules.push(await import(url));
            } catch {
                // Most page assets are not useful JS modules. Ignore them.
            }
        }

        return modules;
    }

    // ---------------------------------------------------------------------
    // TIDAL store discovery
    // ---------------------------------------------------------------------

    let store = null;
    let storeUnsubscribe = null;
    let storeEventTimer = null;
    let pendingStoreState = null;
    let previousStoreState = null;
    let suppressLocalChangesUntil = 0;

    const entryIdByQueueUid = new Map();
    const trackCache = new Map();
    const resolvedTrackCache = new Map();

    function isReduxStore(candidate) {
        return candidate &&
            typeof candidate.getState === "function" &&
            typeof candidate.dispatch === "function" &&
            typeof candidate.subscribe === "function";
    }

    async function discoverStore() {
        for (const module of await importTidalModules()) {
            for (const exported of Object.values(module)) {
                if (typeof exported !== "function") continue;

                // This string is the semantic signature of TIDAL's global
                // store accessor in the build we inspected. The export name
                // and hashed bundle filename are intentionally not assumed.
                const source = Function.prototype.toString.call(exported);
                if (!source.includes("No global store set")) continue;

                try {
                    const candidate = exported();
                    if (isReduxStore(candidate)) return candidate;
                } catch {
                    // The accessor may be present before the store is created.
                }
            }
        }

        throw new Error("Could not discover TIDAL's Redux store");
    }

    async function waitForStore() {
        const deadline = performance.now() + STORE_TIMEOUT_MS;
        let lastError;

        while (performance.now() < deadline) {
            try {
                store = await discoverStore();
                return store;
            } catch (error) {
                lastError = error;
                await new Promise(resolve => setTimeout(resolve, 200));
            }
        }

        throw new Error(`Timed out waiting for TIDAL's Redux store: ${lastError?.message ?? "unknown error"}`);
    }

    function dispatch(type, payload) {
        if (!store) throw new Error("TIDAL store is not initialized");
        store.dispatch(payload === undefined ? { type } : { type, payload });
    }

    function getMediaItem(state, productId) {
        const record = state?.content?.mediaItems?.[String(productId)];
        return record?.item ?? record?.mediaItem ?? record ?? null;
    }

    // ---------------------------------------------------------------------
    // Track metadata and matching
    // ---------------------------------------------------------------------

    // A recording could not be identified (no match, ambiguous, search timed
    // out). Unlike other failures this is expected and per-track: the rest of
    // the queue can still be loaded.
    class TrackLookupError extends Error {
        constructor(message) {
            super(message);
            this.name = "TrackLookupError";
        }
    }

    // Structured bridge error understood by Rust (Error::TrackUnavailable).
    function trackUnavailableError(failures) {
        const error = new Error(
            "Track unavailable on TIDAL: " + failures.map(failure => failure.error.message).join("; "),
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

    function artistNames(item) {
        const artists = item?.artists ?? item?.artist ?? [];
        if (!Array.isArray(artists)) return [];
        return artists
            .map(artist => typeof artist === "string" ? artist : artist?.name)
            .filter(Boolean);
    }

    function toTrackDescriptor(item, productId) {
        if (!item || typeof item.title !== "string") {
            throw new Error(`TIDAL metadata unavailable for track ${productId}`);
        }

        const durationMs = Number.isFinite(item.durationMs)
            ? Math.round(item.durationMs)
            : Number.isFinite(item.duration)
                ? Math.round(item.duration * 1000)
                : null;

        return {
            title: item.title,
            artists: artistNames(item),
            isrc: item.isrc ?? null,
            album: item.album?.title ?? item.albumTitle ?? null,
            durationMs,
        };
    }

    function getTrackDescriptor(productId, state = store.getState()) {
        const key = String(productId);
        if (trackCache.has(key)) return trackCache.get(key);

        const item = getMediaItem(state, key);
        const descriptor = toTrackDescriptor(item, key);
        trackCache.set(key, descriptor);
        return descriptor;
    }

    function artistOverlap(trackA, trackB) {
        const artistsA = new Set((trackA.artists ?? []).map(normalize).filter(Boolean));
        return (trackB.artists ?? []).map(normalize).some(name => artistsA.has(name));
    }

    function scoreCandidate(source, candidate) {
        if (normalize(source.title) !== normalize(candidate.track.title)) return -1;
        if (!artistOverlap(source, candidate.track)) return -1;

        let score = 10; // Exact title and at least one matching artist.
        if (source.isrc && candidate.track.isrc) {
            // Conflicting ISRCs identify different recordings. Do not select
            // one merely because its title and artist happen to match.
            if (normalize(source.isrc) !== normalize(candidate.track.isrc)) {
                return -1;
            }
            score += 100;
        }
        if (source.album && candidate.track.album &&
            normalize(source.album) === normalize(candidate.track.album)) {
            score += 3;
        }
        if (Number.isFinite(source.durationMs) && Number.isFinite(candidate.track.durationMs)) {
            const difference = Math.abs(source.durationMs - candidate.track.durationMs);
            if (difference <= 2_000) score += 2;
            else if (difference > 8_000) score -= 2;
        }
        return score;
    }

    // Result IDs of a completed search, or null while it is missing/loading.
    // A finished search with no hits is an empty list with `loading: false`
    // (verified live); treat only an explicit `false` as finished.
    function getSearchResultIds(query) {
        const key = `search/tracks/${encodeURIComponent(query)}`;
        const list = store.getState()?.content?.trackLists?.[key];
        const page = list?.sorted?.defaultSort;
        const ids = page?.items;
        if (!Array.isArray(ids)) return null;
        if (ids.length > 0 || page.loading === false) return ids;
        return null;
    }

    // TIDAL keeps only its latest search: dispatching SEARCH_FOR while another
    // search is in flight cancels the earlier one (verified live). Run them
    // strictly one at a time.
    let searchQueue = Promise.resolve();

    function searchTrackIds(query) {
        const run = searchQueue.then(() => searchTrackIdsNow(query));
        searchQueue = run.catch(() => {});
        return run;
    }

    async function searchTrackIdsNow(query) {
        const existing = getSearchResultIds(query);
        if (existing) return existing;

        dispatch("search/SEARCH_FOR", {
            query,
            limit: 20,
            offset: 0,
            types: ["TRACKS"],
        });

        const deadline = performance.now() + SEARCH_TIMEOUT_MS;
        while (performance.now() < deadline) {
            const ids = getSearchResultIds(query);
            if (ids) return ids;
            await new Promise(resolve => setTimeout(resolve, 100));
        }

        throw new TrackLookupError(`TIDAL search timed out for "${query}"`);
    }

    async function resolveTrack(track) {
        const existingProviderId = track.providerTrackId ?? track.provider_track_id;
        if (track.provider === "Tidal" && existingProviderId) {
            return String(existingProviderId);
        }

        const cacheKey = [
            normalize(track.title),
            (track.artists ?? []).map(normalize).join("|"),
            normalize(track.isrc),
            normalize(track.album),
            track.durationMs ?? "",
        ].join("::");

        if (resolvedTrackCache.has(cacheKey)) {
            return resolvedTrackCache.get(cacheKey);
        }

        const query = `${track.title} ${(track.artists ?? []).join(" ")}`.trim();
        const ids = await searchTrackIds(query);
        const state = store.getState();

        const candidates = ids.map(value => {
            const productId = typeof value === "object"
                ? value.id ?? value.mediaItemId
                : value;
            if (productId == null) return null;

            const item = getMediaItem(state, productId);
            if (!item || item.streamReady === false) return null;

            try {
                return {
                    productId: String(productId),
                    track: toTrackDescriptor(item, productId),
                };
            } catch {
                return null;
            }
        }).filter(Boolean);

        const ranked = candidates
            .map(candidate => ({ ...candidate, score: scoreCandidate(track, candidate) }))
            .filter(candidate => candidate.score >= 0)
            .sort((a, b) => b.score - a.score);

        if (!ranked.length) {
            throw new TrackLookupError(`No matching TIDAL recording found for "${track.title}"`);
        }
        if (ranked.length > 1 && ranked[0].score === ranked[1].score) {
            throw new TrackLookupError(`TIDAL returned an ambiguous match for "${track.title}"; add an ISRC or album`);
        }

        const productId = ranked[0].productId;
        resolvedTrackCache.set(cacheKey, productId);
        trackCache.set(productId, ranked[0].track);
        return productId;
    }

    // Resolve every entry independently. Entries already in TIDAL's queue
    // (`known`: entryId -> product ID) need no search. Lookup failures are
    // returned per entry; any other error (e.g. the store disappeared) is
    // rethrown.
    async function resolveEntries(entries, known) {
        return Promise.all(entries.map(async entry => {
            const loadedProductId = known.get(entry.entryId);
            if (loadedProductId != null) return { entry, productId: loadedProductId };
            try {
                return { entry, productId: await resolveTrack(entry.track) };
            } catch (error) {
                if (error instanceof TrackLookupError) return { entry, error };
                throw error;
            }
        }));
    }

    // ---------------------------------------------------------------------
    // Queue normalization and stable entry identity
    // ---------------------------------------------------------------------

    function queueElements(state) {
        const queue = state?.playQueue;
        const elements = Array.isArray(queue?.elements) ? queue.elements : [];
        const currentIndex = Number.isInteger(queue?.currentIndex)
            ? Math.max(0, Math.min(queue.currentIndex, elements.length))
            : 0;

        // TIDAL retains played entries before currentIndex. Attune's queue is
        // current-first, so those history entries are intentionally excluded.
        return elements.slice(currentIndex);
    }

    function mediaItemId(queueElement) {
        return queueElement?.mediaItemId ??
            queueElement?.mediaProduct?.productId ??
            queueElement?.item?.id ?? null;
    }

    function makeEntryId(queueElement, productId, index) {
        const uid = queueElement?.uid;
        const key = uid != null ? String(uid) : `${productId ?? "unknown"}:${index}`;

        if (!entryIdByQueueUid.has(key)) {
            entryIdByQueueUid.set(key, crypto.randomUUID());
        }
        return entryIdByQueueUid.get(key);
    }

    function makeQueueEntry(queueElement, index, state) {
        const productId = mediaItemId(queueElement);
        if (productId == null) {
            throw new Error("TIDAL queue entry has no media item ID");
        }

        return {
            entryId: makeEntryId(queueElement, productId, index),
            track: getTrackDescriptor(productId, state),
        };
    }

    // `latestCurrentTime` is not live: it is the position at
    // `latestCurrentTimeSyncTimestamp` (epoch ms), updated only on play,
    // pause, seek and track changes (verified live). Report that pair so
    // receivers extrapolate from the right moment.
    const MAX_SYNC_SKEW_MS = 24 * 60 * 60 * 1000;

    // Verified live: `playbackState` leaves PLAYING while a track loads, and
    // `desiredPlaybackState` stays PLAYING after a PAUSE action. Neither alone
    // is reliable. Trust the explicit states and fall back to the intended
    // state only in between (loading, stalled).
    function isPlaying(controls) {
        if (controls.playbackState === "PLAYING") return true;
        if (controls.playbackState === "PAUSED") return false;
        if (typeof controls.desiredPlaybackState === "string") {
            return controls.desiredPlaybackState === "PLAYING";
        }
        return false;
    }

    function getPlayback(state) {
        const controls = state?.playbackControls ?? {};
        const positionSeconds = Number(controls.latestCurrentTime ?? 0);
        const now = Date.now();
        const syncedAt = Number(controls.latestCurrentTimeSyncTimestamp);
        // Guard against a different unit or a missing value in other builds.
        const plausible = Number.isFinite(syncedAt) && Math.abs(now - syncedAt) < MAX_SYNC_SKEW_MS;
        return {
            isPlaying: isPlaying(controls),
            positionMs: Number.isFinite(positionSeconds)
                ? Math.max(0, Math.round(positionSeconds * 1000))
                : 0,
            positionCapturedAtMs: plausible ? syncedAt : now,
        };
    }

    // TIDAL queue elements as Attune sees them: current first, no history.
    function currentQueueElements(reduxState) {
        const rawEntries = queueElements(reduxState);
        const activeProductId = reduxState?.playbackControls?.mediaProduct?.productId;

        // A track can be started directly, outside the existing play queue.
        // In that case expose it as the current entry and retain the queue as
        // upcoming items instead of reporting the old queue head as current.
        if (activeProductId != null &&
            String(mediaItemId(rawEntries[0])) !== String(activeProductId)) {
            rawEntries.unshift({
                uid: `active:${activeProductId}`,
                mediaItemId: activeProductId,
            });
        }
        return rawEntries;
    }

    // Session entry ID -> TIDAL product ID for everything in TIDAL's queue.
    function loadedProductIds() {
        const known = new Map();
        currentQueueElements(store.getState()).forEach((element, index) => {
            const productId = mediaItemId(element);
            if (productId != null) known.set(makeEntryId(element, productId, index), String(productId));
        });
        return known;
    }

    function stateFromRedux(reduxState) {
        const rawEntries = currentQueueElements(reduxState);
        const queue = rawEntries.map((entry, index) => makeQueueEntry(entry, index, reduxState));
        return {
            queue,
            currentEntryId: queue[0]?.entryId ?? null,
            playback: getPlayback(reduxState),
        };
    }

    function getState() {
        return stateFromRedux(store.getState());
    }

    function rememberAppliedEntryIds(entries) {
        const currentEntries = queueElements(store.getState());
        entries.forEach((entry, index) => {
            const uid = currentEntries[index]?.uid;
            if (uid != null) entryIdByQueueUid.set(String(uid), entry.entryId);
        });
    }

    // ---------------------------------------------------------------------
    // Incremental queue edits (the current track keeps playing)
    // ---------------------------------------------------------------------
    //
    // Verified live on TIDAL Desktop (playQueue.type "local"):
    // - ADD_AT_INDEX {context, index, mediaItemIds} inserts before an absolute
    //   element index (history included) without moving currentIndex.
    // - REMOVE_ELEMENT {uid} removes a non-current element.
    // - MOVE_TRACK {fromIndex, toIndex} inserts at toIndex, then removes the
    //   original, and may give the moved element a new uid.
    // Replacing the whole queue instead restarts the current track.

    // The Attune queue as positions in TIDAL's element list, or null when it
    // isn't a direct view (another queue type, or a track playing outside
    // the queue). Callers then fall back to replacing the queue.
    function directQueue() {
        const reduxState = store.getState();
        const queue = reduxState?.playQueue;
        if (queue?.type !== "local" || !Number.isInteger(queue.currentIndex)) return null;
        const elements = queueElements(reduxState);
        if (currentQueueElements(reduxState).length !== elements.length) return null;
        return { base: queue.currentIndex, elements };
    }

    function positionOf(view, entryId) {
        return view.elements.findIndex(
            (element, index) => makeEntryId(element, mediaItemId(element), index) === entryId,
        );
    }

    function rememberElementAt(absoluteIndex, entryId) {
        const uid = store.getState()?.playQueue?.elements?.[absoluteIndex]?.uid;
        if (uid != null) entryIdByQueueUid.set(String(uid), entryId);
    }

    /** Insert one entry after the current track. Null if not possible here. */
    async function insertUpcoming(entry, index) {
        const view = directQueue();
        if (!view || index < 1 || index > view.elements.length) return null;
        const [resolved] = await resolveEntries([entry], loadedProductIds());
        if (resolved.error) return { unavailableEntryIds: [entry.entryId] };
        setRemoteMutationWindow();
        dispatch("playQueue/ADD_AT_INDEX", {
            context: SOURCE_CONTEXT,
            index: view.base + index,
            mediaItemIds: [resolved.productId],
        });
        rememberElementAt(view.base + index, entry.entryId);
        return { unavailableEntryIds: [] };
    }

    /** Remove an upcoming entry. Null if not possible here. */
    function removeUpcoming(entryId) {
        const view = directQueue();
        const position = view ? positionOf(view, entryId) : -1;
        const uid = position >= 1 ? view.elements[position].uid : null;
        if (uid == null) return null;
        dispatch("playQueue/REMOVE_ELEMENT", { uid });
        return { unavailableEntryIds: [] };
    }

    /** Move an upcoming entry to another upcoming position. Null if not possible here. */
    function moveUpcoming(entryId, toIndex) {
        const view = directQueue();
        const from = view ? positionOf(view, entryId) : -1;
        if (from < 1 || toIndex < 1 || toIndex >= view.elements.length) return null;
        if (from !== toIndex) {
            const target = view.base + toIndex;
            // MOVE_TRACK inserts before removing, so a forward move targets
            // one past the final position.
            dispatch("playQueue/MOVE_TRACK", {
                fromIndex: view.base + from,
                toIndex: from < toIndex ? target + 1 : target,
            });
            rememberElementAt(target, entryId);
        }
        return { unavailableEntryIds: [] };
    }

    // ---------------------------------------------------------------------
    // Playback and queue actions
    // ---------------------------------------------------------------------

    function setRemoteMutationWindow() {
        suppressLocalChangesUntil = Date.now() + REMOTE_CHANGE_SUPPRESSION_MS;
    }

    function setPlaying(isPlaying) {
        dispatch(isPlaying ? "playbackControls/PLAY" : "playbackControls/PAUSE");
    }

    function seek(positionMs) {
        if (!Number.isFinite(positionMs) || positionMs < 0) {
            throw new TypeError("positionMs must be a non-negative number");
        }
        // TIDAL's internal seek action uses seconds, while Attune uses ms.
        dispatch("playbackControls/SEEK", positionMs / 1000);
    }

    async function playTrack(track) {
        const productId = await resolveTrack(track);
        dispatch("content/FETCH_AND_PLAY_MEDIA_ITEM", {
            itemId: String(productId),
            itemType: "track",
            sourceContext: SOURCE_CONTEXT,
        });
    }

    /**
     * Load `entries` (from `currentEntryId` on) into TIDAL's play queue.
     *
     * Resolves to `{ unavailableEntryIds }`: upcoming entries with no TIDAL
     * match are left out of TIDAL's queue and reported, the rest still plays.
     * If the *current* entry has no match, nothing is changed and a
     * `trackUnavailable` error is thrown.
     */
    async function replaceQueue(entries, currentEntryId, playback) {
        if (!Array.isArray(entries)) {
            throw new TypeError("Queue entries must be an array");
        }
        // The session keeps playing while songs are looked up and loaded.
        const calledAt = Date.now();

        if (entries.length === 0) {
            // We have verified queue replacement, but not a TIDAL action that
            // safely unloads the active item. Do not invent one or claim that
            // an empty session queue was applied when playback still exists.
            if (store.getState()?.playbackControls?.mediaProduct) {
                throw new Error("TIDAL cannot safely unload the active track with the verified bridge actions");
            }
            setPlaying(false);
            return { unavailableEntryIds: [] };
        }

        const selectedIndex = currentEntryId == null
            ? 0
            : entries.findIndex(entry => entry.entryId === currentEntryId);
        if (selectedIndex < 0) {
            throw new Error(`Current queue entry was not found: ${currentEntryId}`);
        }

        // Entries before currentEntryId are history, not upcoming queue items.
        const orderedEntries = entries.slice(selectedIndex);

        // Resolve every recording before replacing the active queue, so an
        // unplayable current track leaves the user's queue intact. The current
        // entry goes first: if it can't play, don't search for the rest.
        const known = loadedProductIds();
        const [current] = await resolveEntries([orderedEntries[0]], known);
        if (current.error) {
            throw trackUnavailableError([current]);
        }
        const upcoming = await resolveEntries(orderedEntries.slice(1), known);
        const failures = upcoming.filter(result => result.error);
        const loaded = [current, ...upcoming.filter(result => !result.error)];

        setRemoteMutationWindow();
        dispatch("playQueue/ADD_MEDIA_ITEMS_TO_QUEUE", {
            mediaItemIds: loaded.map(result => result.productId),
            options: { overwritePlayQueue: true },
            position: "now",
            sourceContext: SOURCE_CONTEXT,
        });
        rememberAppliedEntryIds(loaded.map(result => result.entry));

        if (playback) await startLoadedTrack(current.productId, playback, calledAt);

        return { unavailableEntryIds: failures.map(failure => failure.entry.entryId) };
    }

    /**
     * Bring a freshly loaded track to the session's position and play state,
     * and keep it there until it stays put.
     *
     * Verified live: a queue loaded with position "now" starts its first track
     * once it has loaded, from 0, undoing a pause or seek dispatched before
     * that. Joining a paused jam therefore started playback. Which store
     * states mark "loaded" isn't reliable (the previous track's PLAYING
     * lingers), so instead of waiting for one signal, re-apply the position
     * and play state whenever TIDAL differs, until it has matched for
     * TRACK_HOLD_STABLE_MS. Local changes aren't reported meanwhile: none of
     * this is the user's doing.
     */
    async function startLoadedTrack(productId, playback, calledAt) {
        const target = () => (playback.positionMs ?? 0) + (playback.isPlaying ? Date.now() - calledAt : 0);
        const sameTrack = state =>
            String(state?.playbackControls?.mediaProduct?.productId) === String(productId);

        await holdUntil({
            timeoutMs: TRACK_START_TIMEOUT_MS,
            stableMs: TRACK_HOLD_STABLE_MS,
            matches(state) {
                const now = getPlayback(state);
                const position = now.positionMs + (now.isPlaying ? Date.now() - now.positionCapturedAtMs : 0);
                return sameTrack(state) &&
                    now.isPlaying === Boolean(playback.isPlaying) &&
                    Math.abs(position - target()) <= TRACK_HOLD_TOLERANCE_MS;
            },
            correct(state) {
                if (!sameTrack(state)) return; // still loading
                if (target() > TRACK_HOLD_TOLERANCE_MS) seek(target());
                setPlaying(Boolean(playback.isPlaying));
            },
        });
    }

    /**
     * Keep correcting TIDAL toward a target until it has matched for
     * `stableMs`, or `timeoutMs` passes. Its store states don't reliably say
     * when an action has taken effect (a loading track can still undo a pause,
     * a reload restores and resumes by itself), so instead of waiting for one
     * signal, re-apply whenever it differs. Local changes aren't reported
     * meanwhile: none of this is the user's doing.
     */
    async function holdUntil({ timeoutMs, stableMs, matches, correct }) {
        suppressLocalChangesUntil = Date.now() + timeoutMs + REMOTE_CHANGE_SUPPRESSION_MS;
        const deadline = Date.now() + timeoutMs;
        let matchingSince = null;
        let lastCorrected = 0;
        try {
            while (Date.now() < deadline) {
                const state = store.getState();
                if (matches(state)) {
                    matchingSince ??= Date.now();
                    if (Date.now() - matchingSince >= stableMs) return true;
                } else {
                    matchingSince = null;
                    if (Date.now() - lastCorrected >= TRACK_HOLD_RETRY_MS) {
                        correct(state);
                        lastCorrected = Date.now();
                    }
                }
                await new Promise(resolve => setTimeout(resolve, TRACK_START_POLL_MS));
            }
            return false;
        } finally {
            setRemoteMutationWindow();
        }
    }

    // ---------------------------------------------------------------------
    // Neutral state
    // ---------------------------------------------------------------------

    // TIDAL's queue order and play state as one comparable value.
    function restoreKey(state) {
        const elements = state?.playQueue?.elements ?? [];
        return JSON.stringify([
            elements.map(element => element?.mediaItemId),
            state?.playbackControls?.mediaProduct?.productId ?? null,
            isPlaying(state?.playbackControls ?? {}),
        ]);
    }

    /**
     * After attaching (which reloads TIDAL), wait for TIDAL to finish
     * restoring itself, then pause and keep it paused.
     *
     * Verified live: after the reload TIDAL reports an empty queue, restores
     * its queue, then resumes playback by itself. All of that looks like the
     * user's doing, and a resume would start any jam created from this state.
     * Pausing gives every session a deliberate starting point; the reload has
     * interrupted the music anyway.
     */
    async function settle({ pause = true } = {}) {
        suppressLocalChangesUntil = Date.now() + SETTLE_TIMEOUT_MS + REMOTE_CHANGE_SUPPRESSION_MS;
        const deadline = Date.now() + SETTLE_TIMEOUT_MS;
        let previous = null;
        let unchangedSince = Date.now();
        while (Date.now() < deadline) {
            const state = store.getState();
            const key = restoreKey(state);
            if (key !== previous) {
                previous = key;
                unchangedSince = Date.now();
            }
            // An empty queue may be all there is, but give a restore longer.
            const empty = (state?.playQueue?.elements ?? []).length === 0;
            if (Date.now() - unchangedSince >= (empty ? SETTLE_EMPTY_MS : SETTLE_STABLE_MS)) break;
            await new Promise(resolve => setTimeout(resolve, TRACK_START_POLL_MS));
        }
        if (pause) {
            await holdUntil({
                timeoutMs: TRACK_START_TIMEOUT_MS,
                stableMs: SETTLE_PAUSED_MS,
                matches: state => !isPlaying(state?.playbackControls ?? {}),
                correct: () => setPlaying(false),
            });
        } else {
            setRemoteMutationWindow();
        }
    }

    /**
     * Turn off what a session can't follow: shuffle and repeat (they change
     * what plays next behind Attune's back) and autoplay (it appends similar
     * tracks when the queue ends). Returns the user's settings, for
     * `restoreAfterSession`. Shuffle is turned off without reordering the
     * queue (TIDAL's own button also unshuffles it).
     */
    function prepareForSession() {
        const state = store.getState();
        const previous = {
            shuffle: Boolean(state?.playQueue?.shuffleModeEnabled),
            repeatMode: Number.isInteger(state?.playQueue?.repeatMode) ? state.playQueue.repeatMode : REPEAT_OFF,
            autoPlay: typeof state?.settings?.autoPlay === "boolean" ? state.settings.autoPlay : null,
        };
        setRemoteMutationWindow();
        if (previous.shuffle) dispatch("playQueue/DISABLE_SHUFFLE_MODE");
        if (previous.repeatMode !== REPEAT_OFF) dispatch("playQueue/SET_REPEAT_MODE", REPEAT_OFF);
        if (previous.autoPlay === true) dispatch("settings/SET_AUTOPLAY", false);
        return previous;
    }

    /** Put back the settings `prepareForSession` returned. */
    function restoreAfterSession(previous) {
        if (!previous || typeof previous !== "object") return;
        setRemoteMutationWindow();
        if (previous.shuffle === true) dispatch("playQueue/ENABLE_SHUFFLE_MODE");
        if (Number.isInteger(previous.repeatMode) && previous.repeatMode !== REPEAT_OFF) {
            dispatch("playQueue/SET_REPEAT_MODE", previous.repeatMode);
        }
        if (previous.autoPlay === true) dispatch("settings/SET_AUTOPLAY", true);
    }

    async function applyState(nextState) {
        if (!nextState || !Array.isArray(nextState.queue) || !nextState.playback) {
            throw new TypeError("Invalid Attune music state");
        }

        setRemoteMutationWindow();
        return replaceQueue(
            nextState.queue,
            nextState.currentEntryId ?? nextState.queue[0]?.entryId ?? null,
            nextState.playback,
        );
    }

    function cloneState(state) {
        return {
            queue: state.queue.map(entry => ({
                entryId: entry.entryId,
                track: { ...entry.track, artists: [...entry.track.artists] },
            })),
            currentEntryId: state.currentEntryId,
            playback: { ...state.playback },
        };
    }

    function moveArrayItem(items, fromIndex, toIndex) {
        if (fromIndex < 0 || fromIndex >= items.length || toIndex < 0 || toIndex >= items.length) {
            throw new RangeError("Queue index is out of range");
        }
        const [item] = items.splice(fromIndex, 1);
        items.splice(toIndex, 0, item);
    }

    async function applyOperation(operation) {
        if (!operation || typeof operation.type !== "string") {
            throw new TypeError("Invalid Attune operation");
        }

        setRemoteMutationWindow();

        switch (operation.type) {
            case "queue.replace": {
                const currentState = getState();
                const currentEntryId = operation.currentEntryId ?? operation.entries[0]?.entryId ?? null;
                const playback = { ...currentState.playback };
                if (currentEntryId !== currentState.currentEntryId) {
                    playback.positionMs = 0;
                    playback.isPlaying = true;
                }
                return replaceQueue(operation.entries, currentEntryId, playback);
            }

            case "queue.insert": {
                const incremental = await insertUpcoming(operation.entry, operation.index);
                if (incremental) return incremental;
                const state = cloneState(getState());
                const index = Math.max(0, Math.min(operation.index, state.queue.length));
                state.queue.splice(index, 0, {
                    entryId: operation.entry.entryId,
                    track: { ...operation.entry.track, artists: [...operation.entry.track.artists] },
                });
                if (!state.currentEntryId || index === 0) {
                    state.currentEntryId = operation.entry.entryId;
                    state.playback.positionMs = 0;
                }
                return replaceQueue(state.queue, state.currentEntryId, state.playback);
            }

            case "queue.remove": {
                const incremental = removeUpcoming(operation.entryId);
                if (incremental) return incremental;
                const state = cloneState(getState());
                const index = state.queue.findIndex(entry => entry.entryId === operation.entryId);
                if (index < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                state.queue.splice(index, 1);
                if (state.currentEntryId === operation.entryId) {
                    state.currentEntryId = state.queue[0]?.entryId ?? null;
                    state.playback.positionMs = 0;
                }
                return replaceQueue(state.queue, state.currentEntryId, state.playback);
            }

            case "queue.move": {
                const incremental = moveUpcoming(operation.entryId, operation.toIndex);
                if (incremental) return incremental;
                const state = cloneState(getState());
                const fromIndex = state.queue.findIndex(entry => entry.entryId === operation.entryId);
                if (fromIndex < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                if (state.currentEntryId === operation.entryId && operation.toIndex !== 0) {
                    throw new Error("Moving the currently playing entry behind upcoming tracks is not supported");
                }
                moveArrayItem(state.queue, fromIndex, operation.toIndex);
                if (operation.toIndex === 0) {
                    state.currentEntryId = operation.entryId;
                    state.playback.positionMs = 0;
                }
                return replaceQueue(state.queue, state.currentEntryId, state.playback);
            }

            case "playback.select":
            case "playback.advance": {
                const targetId = operation.entryId ?? operation.targetEntryId;
                const state = cloneState(getState());
                if (!state.queue.some(entry => entry.entryId === targetId)) {
                    throw new Error(`Playback target is not in the queue: ${targetId}`);
                }
                const targetIndex = state.queue.findIndex(entry => entry.entryId === targetId);
                state.queue = state.queue.slice(targetIndex);
                state.currentEntryId = targetId;
                state.playback.positionMs = operation.positionMs ?? 0;
                state.playback.isPlaying = operation.isPlaying ?? true;
                return replaceQueue(state.queue, targetId, state.playback);
            }

            case "playback.seek": {
                const state = getState();
                if (operation.entryId !== state.currentEntryId) {
                    throw new Error("Cannot seek a track that is not currently playing");
                }
                seek(operation.positionMs);
                return { unavailableEntryIds: [] };
            }

            case "playback.setPlaying":
                setPlaying(Boolean(operation.isPlaying));
                return { unavailableEntryIds: [] };

            default:
                throw new Error(`Unsupported Attune operation: ${operation.type}`);
        }
    }

    // ---------------------------------------------------------------------
    // Local state observation and operation emission
    // ---------------------------------------------------------------------

    function sameQueue(left, right) {
        return left.length === right.length &&
            left.every((entry, index) => entry.entryId === right[index].entryId);
    }

    // When a track starts, TIDAL resets its position sync point to 0 a moment
    // after the track was requested (verified live). That looks like a seek
    // back to the start; ignore such jumps shortly after a track change.
    const TRACK_START_GRACE_MS = 10_000;
    const TRACK_START_POSITION_MS = 1_500;
    let activeTrackReference = null;
    let activeTrackChangedAt = 0;

    function noteActiveTrack(reduxState) {
        const product = reduxState?.playbackControls?.mediaProduct;
        const reference = product ? String(product.referenceId ?? product.productId) : null;
        if (reference !== activeTrackReference) {
            activeTrackReference = reference;
            activeTrackChangedAt = Date.now();
        }
    }

    function positionWasSought(before, after) {
        if (before.currentEntryId !== after.currentEntryId) return false;
        if (before.playback.isPlaying !== after.playback.isPlaying) return false;
        if (after.playback.positionMs <= TRACK_START_POSITION_MS &&
            Date.now() - activeTrackChangedAt < TRACK_START_GRACE_MS) {
            return false;
        }

        const actualDelta = after.playback.positionMs - before.playback.positionMs;
        const elapsed = after.playback.positionCapturedAtMs - before.playback.positionCapturedAtMs;
        const expectedDelta = before.playback.isPlaying ? Math.max(0, elapsed) : 0;
        return Math.abs(actualDelta - expectedDelta) > 1_200;
    }

    function emitLocalChanges(beforeReduxState, afterReduxState) {
        if (Date.now() < suppressLocalChangesUntil) return;

        let before;
        let after;
        try {
            before = stateFromRedux(beforeReduxState);
            after = stateFromRedux(afterReduxState);
        } catch (error) {
            sendEvent("Error", `Could not read TIDAL state: ${error.message}`);
            return;
        }

        const currentChanged = before.currentEntryId !== after.currentEntryId;
        const advancedToNextEntry = currentChanged &&
            before.queue[1]?.entryId === after.currentEntryId &&
            sameQueue(before.queue.slice(2), after.queue.slice(1));
        const queueChanged = !sameQueue(before.queue, after.queue) && !advancedToNextEntry;

        if (queueChanged) {
            sendOperation({
                type: "queue.replace",
                entries: after.queue,
                currentEntryId: after.currentEntryId,
            });
        } else if (currentChanged && after.currentEntryId) {
            sendOperation({
                type: "playback.advance",
                targetEntryId: after.currentEntryId,
                positionMs: after.playback.positionMs,
                isPlaying: after.playback.isPlaying,
            });
        }

        if (before.playback.isPlaying !== after.playback.isPlaying) {
            sendOperation({
                type: "playback.setPlaying",
                isPlaying: after.playback.isPlaying,
            });
        }

        if (positionWasSought(before, after)) {
            sendOperation({
                type: "playback.seek",
                entryId: after.currentEntryId,
                positionMs: after.playback.positionMs,
            });
        }

        // Preserve the original event for the current in-app diagnostics/UI.
        if (currentChanged ||
            before.playback.isPlaying !== after.playback.isPlaying ||
            positionWasSought(before, after)) {
            sendEvent("PlaystateChange", getPlayState());
        }
    }

    function subscribeToStore() {
        previousStoreState = store.getState();
        storeUnsubscribe = store.subscribe(() => {
            const nextState = store.getState();
            if (!nextState?.view?.appReady) return;

            // On every change, before debouncing or suppression, so remote
            // track changes count too.
            noteActiveTrack(nextState);
            pendingStoreState = nextState;
            clearTimeout(storeEventTimer);
            storeEventTimer = setTimeout(() => {
                const before = previousStoreState;
                const after = pendingStoreState;
                previousStoreState = after;
                pendingStoreState = null;
                storeEventTimer = null;
                if (before && after) emitLocalChanges(before, after);
            }, STORE_EVENT_DEBOUNCE_MS);
        });
    }

    // ---------------------------------------------------------------------
    // Legacy methods retained for the current Rust/UI call sites
    // ---------------------------------------------------------------------

    function getPlayState() {
        const state = store.getState();
        const productId = state.playbackControls?.mediaProduct?.productId;
        const track = productId != null ? getTrackDescriptor(productId, state) : null;
        const playback = getPlayback(state);
        // TIDAL's API gives album covers as a UUID; its image server takes
        // the UUID with slashes instead of dashes.
        const cover = productId != null ? getMediaItem(state, productId)?.album?.cover : null;

        return {
            track: track ? {
                ...track,
                provider: "Tidal",
                provider_track_id: String(productId),
                cover_url: typeof cover === "string"
                    ? `https://resources.tidal.com/images/${cover.replaceAll("-", "/")}/320x320.jpg`
                    : null,
            } : null,
            position_ms: playback.positionMs,
            timestamp_ms: playback.positionCapturedAtMs,
            playing: playback.isPlaying,
        };
    }

    async function addToQueue(tracks) {
        const mediaItemIds = await Promise.all(tracks.map(resolveTrack));
        setRemoteMutationWindow();
        dispatch("playQueue/ADD_MEDIA_ITEMS_TO_QUEUE", {
            mediaItemIds,
            position: "last",
            sourceContext: SOURCE_CONTEXT,
        });
    }

    // ---------------------------------------------------------------------
    // Lifecycle and public bridge API
    // ---------------------------------------------------------------------

    function sendEvent(type, data) {
        if (typeof window[EVENT_BINDING] !== "function") {
            console.warn("[Attune] CDP event binding is unavailable", type);
            return;
        }

        const event = { type };
        if (data !== undefined) event.data = data;
        window[EVENT_BINDING](JSON.stringify(event));
    }

    function sendOperation(operation) {
        sendEvent("Operation", operation);
    }

    async function initialise() {
        await waitForStore();
        subscribeToStore();
        bridge.ready = true;
        sendEvent("Ready");
        console.info("[Attune] TIDAL adapter ready");
    }

    async function dispose() {
        if (storeEventTimer) clearTimeout(storeEventTimer);
        storeEventTimer = null;
        storeUnsubscribe?.();
        storeUnsubscribe = null;
        resourceObserver.disconnect();
        store = null;
        previousStoreState = null;
        pendingStoreState = null;
        entryIdByQueueUid.clear();
        trackCache.clear();
        resolvedTrackCache.clear();
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
        getState,
        getSnapshot: getState,
        applySnapshot: applyState,
        applyOperation,
        settle,
        prepareForSession,
        restoreAfterSession,

        // Existing controller commands. Keeping these avoids breaking the
        // current Tauri UI while the session commands are integrated.
        getPlayState,
        playTrack,
        pause: async () => setPlaying(false),
        resume: async () => setPlaying(true),
        seek,
        next: async () => dispatch("playbackControls/SKIP_NEXT"),
        previous: async () => dispatch("playQueue/MOVE_PREVIOUS"),
        addToQueue,
        moveQueueItem: async (fromIndex, toIndex) => {
            const state = cloneState(getState());
            const entry = state.queue[fromIndex];
            if (!entry) throw new RangeError("Queue index is out of range");
            await applyOperation({ type: "queue.move", entryId: entry.entryId, toIndex });
        },
        removeFromQueue: async index => {
            const state = getState();
            const entry = state.queue[index];
            if (!entry) throw new RangeError("Queue index is out of range");
            await applyOperation({ type: "queue.remove", entryId: entry.entryId });
        },
    };

    window[BRIDGE_KEY] = bridge;
    window.bridge = bridge;

    try {
        await initialise();
    } catch (error) {
        console.error("[Attune] TIDAL adapter initialization failed", error);
        sendEvent("Error", error?.message ?? String(error));
    }

    return bridge;
})();
