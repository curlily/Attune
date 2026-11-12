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
    const TRACK_HOLD_PAUSED_STABLE_MS = 1_000;
    const TRACK_HOLD_RETRY_MS = 100;
    const PLAY_STATE_RETRY_MS = 1_000;
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
    let openApi = null;
    let getPlayerPosition = null;
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

    // TIDAL's client for its open API (an RTK Query API object). Found by its
    // reducer path, not by minified export names. Optional: without it,
    // tracks are found by text search only.
    function findOpenApi(modules) {
        for (const module of modules) {
            for (const exported of Object.values(module)) {
                try {
                    if (exported && typeof exported === "object" &&
                        exported.reducerPath === "tidalOpenPlatformApi" &&
                        typeof exported.endpoints?.getTracks?.initiate === "function") {
                        return exported;
                    }
                } catch {
                    // A getter on some export may throw. Ignore it.
                }
            }
        }
        return null;
    }

    // TIDAL's player SDK exports a getter for the active player's position
    // (`return x.activePlayer?.getPosition()??0`), found by that code.
    // Optional: the store's position is used without it.
    function findPlayerPosition(modules) {
        for (const module of modules) {
            for (const exported of Object.values(module)) {
                if (typeof exported !== "function") continue;
                const source = Function.prototype.toString.call(exported);
                if (source.length < 200 && source.includes(".activePlayer?.getPosition()")) return exported;
            }
        }
        return null;
    }

    async function discoverStore() {
        const modules = await importTidalModules();
        openApi ??= findOpenApi(modules);
        getPlayerPosition ??= findPlayerPosition(modules);
        for (const module of modules) {
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

    /**
     * Resolve to true once `predicate(state)` holds, or false after
     * `timeoutMs`. Checked on every store change rather than on a timer.
     *
     * Verified live: while TIDAL's window is hidden and nothing plays,
     * Chromium runs the page's timers once a second (a 25 ms timeout took
     * 1000 ms), so polling waits took up to a second each step. Store
     * changes come from TIDAL's own media and network events and aren't
     * delayed.
     */
    function waitFor(predicate, timeoutMs) {
        return new Promise(resolve => {
            let done = false;
            let timer = null;
            let unsubscribe = () => {};
            const holds = () => {
                try {
                    return Boolean(store && predicate(store.getState()));
                } catch {
                    return false;
                }
            };
            const finish = value => {
                if (done) return;
                done = true;
                clearTimeout(timer);
                unsubscribe();
                resolve(value);
            };
            if (holds()) return finish(true);
            unsubscribe = store.subscribe(() => {
                if (holds()) finish(true);
            });
            timer = setTimeout(() => finish(holds()), timeoutMs);
        });
    }

    function getMediaItem(state, productId) {
        const record = state?.content?.mediaItems?.[String(productId)];
        return record?.item ?? record?.mediaItem ?? record ?? null;
    }

    // ---------------------------------------------------------------------
    // Track metadata and matching
    // ---------------------------------------------------------------------

    // A recording could not be identified (no match, search timed out, or
    // TIDAL couldn't load it). Unlike other failures this is expected and per-track: the rest of
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

    // Services disagree on where featured artists go: "Shoong!" on one is
    // "Shoong! (feat. LISA of BLACKPINK)" on another (verified live, same
    // rule as the Spotify adapter).
    function normalizeTitle(value) {
        return normalize(String(value ?? "")
            .replace(/[([][^)\]]*\b(feat|ft|featuring|with)\b[^)\]]*[)\]]/gi, " ")
            .replace(/\s[-–]\s*(feat|ft|featuring)\b.*$/i, " "));
    }

    function artistOverlap(trackA, trackB) {
        const artistsA = new Set((trackA.artists ?? []).map(normalize).filter(Boolean));
        return (trackB.artists ?? []).map(normalize).some(name => artistsA.has(name));
    }

    function scoreCandidate(source, candidate) {
        if (normalizeTitle(source.title) !== normalizeTitle(candidate.track.title)) return -1;
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

        if (await waitFor(() => getSearchResultIds(query) != null, SEARCH_TIMEOUT_MS)) {
            return getSearchResultIds(query);
        }
        throw new TrackLookupError(`TIDAL search timed out for "${query}"`);
    }

    // -----------------------------------------------------------------
    // ISRC lookup through TIDAL's open API (verified live, TIDAL 2.48)
    // -----------------------------------------------------------------
    //
    // `GET /tracks?filter[isrc]=a,b,…&include=artists,albums` takes at most
    // 20 values (more is a 400 INVALID_ARRAY_LENGTH) and answered 20 in
    // 360 ms, where each text search takes about 500 ms and has to wait for
    // the previous one. Results come back in any order; one ISRC can have
    // several tracks (single and album). Lookups may run in parallel.

    const ISRC_BATCH_SIZE = 20;
    const MEDIA_ITEM_TIMEOUT_MS = 8_000;
    // ISRC -> Promise<candidates[]>
    const isrcCandidates = new Map();

    function normalizeIsrc(value) {
        return typeof value === "string" ? value.replace(/[^A-Za-z0-9]/g, "").toUpperCase() : "";
    }

    // ISO 8601 durations as the open API gives them ("PT3M41S").
    function isoDurationMs(value) {
        const match = /^PT(?:(\d+(?:\.\d+)?)H)?(?:(\d+(?:\.\d+)?)M)?(?:(\d+(?:\.\d+)?)S)?$/.exec(String(value ?? ""));
        if (!match) return null;
        const [hours, minutes, seconds] = match.slice(1).map(part => Number(part ?? 0));
        return Math.round(((hours * 60 + minutes) * 60 + seconds) * 1000);
    }

    // Open API result (JSON:API) -> candidates with the same descriptor
    // shape as TIDAL's media items.
    function candidatesFromDocument(document) {
        const included = new Map((document?.included ?? []).map(item => [`${item.type}:${item.id}`, item]));
        const related = (track, name) => (track.relationships?.[name]?.data ?? [])
            .map(link => included.get(`${link.type}:${link.id}`))
            .filter(Boolean);
        return (document?.data ?? [])
            .filter(track => track?.type === "tracks" && track.attributes)
            .map(track => ({
                productId: String(track.id),
                streamable: !Array.isArray(track.attributes.availability) ||
                    track.attributes.availability.includes("STREAM"),
                track: {
                    title: track.attributes.title,
                    artists: related(track, "artists").map(artist => artist.attributes?.name).filter(Boolean),
                    isrc: track.attributes.isrc ?? null,
                    album: related(track, "albums")[0]?.attributes?.title ?? null,
                    durationMs: isoDurationMs(track.attributes.duration),
                },
            }));
    }

    async function fetchIsrcBatch(isrcs) {
        const countryCode = store.getState()?.session?.countryCode;
        const result = await store.dispatch(openApi.endpoints.getTracks.initiate(
            { countryCode, filter: { isrc: isrcs }, include: ["artists", "albums"] },
            { subscribe: false },
        ));
        if (result?.error) {
            throw new Error(`TIDAL ISRC lookup failed: ${JSON.stringify(result.error).slice(0, 200)}`);
        }
        return candidatesFromDocument(result?.data);
    }

    /**
     * Look up ISRCs not looked up before, in batches. Fills `isrcCandidates`;
     * a failed batch leaves an empty list for its ISRCs (text search then
     * takes over).
     */
    function prefetchIsrcs(values) {
        if (!openApi) return;
        const wanted = [...new Set(values.map(normalizeIsrc).filter(Boolean))]
            .filter(isrc => !isrcCandidates.has(isrc));
        for (let start = 0; start < wanted.length; start += ISRC_BATCH_SIZE) {
            const batch = wanted.slice(start, start + ISRC_BATCH_SIZE);
            const request = fetchIsrcBatch(batch).catch(error => {
                console.warn("[Attune]", error.message);
                return [];
            });
            batch.forEach(isrc => isrcCandidates.set(isrc, request.then(candidates =>
                candidates.filter(candidate => normalizeIsrc(candidate.track.isrc) === isrc))));
        }
    }

    /**
     * The TIDAL track for `track` by its ISRC, or null. An exact ISRC is the
     * same recording even when the titles differ between services; among
     * several (single, album) the closest title, artist, album and length win.
     */
    async function resolveByIsrc(track) {
        const isrc = normalizeIsrc(track.isrc);
        if (!isrc || !openApi) return null;
        prefetchIsrcs([isrc]);
        const candidates = (await isrcCandidates.get(isrc)).filter(candidate => candidate.streamable);
        if (!candidates.length) return null;

        const score = candidate => {
            const sameTitle = normalizeTitle(track.title) === normalizeTitle(candidate.track.title);
            const sameArtist = artistOverlap(track, candidate.track);
            // An ISRC hit with neither the title nor an artist in common is
            // more likely a data error than the same song.
            if (!sameTitle && !sameArtist) return -1;
            let value = (sameTitle ? 10 : 0) + (sameArtist ? 5 : 0);
            if (track.album && candidate.track.album &&
                normalize(track.album) === normalize(candidate.track.album)) value += 3;
            if (Number.isFinite(track.durationMs) && Number.isFinite(candidate.track.durationMs) &&
                Math.abs(track.durationMs - candidate.track.durationMs) <= 2_000) value += 2;
            return value;
        };
        return candidates
            .map(candidate => ({ ...candidate, score: score(candidate) }))
            .filter(candidate => candidate.score >= 0)
            .sort((a, b) => b.score - a.score)[0] ?? null;
    }

    /**
     * Make sure TIDAL has its own metadata for these tracks
     * (`content.mediaItems`). The player can't load a queue element without
     * it. `content/LOAD_SINGLE_MEDIA_ITEM` fetches one (150 ms for three in
     * parallel, verified live). Resolves to the IDs still missing.
     */
    async function ensureMediaItems(productIds) {
        const present = id => getMediaItem(store.getState(), id) != null;
        const missing = [...new Set(productIds.map(String))].filter(id => !present(id));
        if (!missing.length) return [];
        missing.forEach(id => dispatch("content/LOAD_SINGLE_MEDIA_ITEM", { id, itemType: "track" }));
        await waitFor(() => missing.every(present), MEDIA_ITEM_TIMEOUT_MS);
        return missing.filter(id => !present(id));
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

        // The exact recording by ISRC first; text search if that finds
        // nothing TIDAL can play.
        const byIsrc = await resolveByIsrc(track);
        if (byIsrc && (await ensureMediaItems([byIsrc.productId])).length === 0) {
            resolvedTrackCache.set(cacheKey, byIsrc.productId);
            return byIsrc.productId;
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
        // Equal scores used to be rejected as ambiguous, which is why
        // "Shoong!" was never found (verified live): TIDAL lists the same
        // recording on the single and the album (same ISRC, title, artists,
        // length). Like the Spotify adapter, take TIDAL's best-ranked one
        // (the sort is stable).

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
        // One ISRC request per 20 songs, instead of a search each.
        prefetchIsrcs(entries
            .filter(entry => !known.has(entry.entryId))
            .map(entry => entry.track?.isrc));
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

    // Queue elements Attune writes carry the session entry ID in their uid,
    // so they keep it across a bridge reload (TIDAL saves its queue, uids
    // included). TIDAL itself only parses uids starting with `pq__` in its
    // cloud queue code, as keys it may not know; ours keep that prefix.
    const ATTUNE_UID_PREFIX = "pq__attune__";

    function uidForEntry(entryId) {
        return ATTUNE_UID_PREFIX + entryId;
    }

    function makeEntryId(queueElement, productId, index) {
        const uid = queueElement?.uid;
        // A kept current element may carry an older entry's uid; the
        // mapping made when keeping it wins.
        if (uid != null && entryIdByQueueUid.has(String(uid))) {
            return entryIdByQueueUid.get(String(uid));
        }
        if (typeof uid === "string" && uid.startsWith(ATTUNE_UID_PREFIX)) {
            return uid.slice(ATTUNE_UID_PREFIX.length);
        }
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
    // `desiredPlaybackState` can be PLAYING after a PAUSE action (a STALLED
    // or PLAYING report from the player after it sets it back). Neither
    // alone is reliable. From TIDAL's reducer (TIDAL 2.48): only PAUSE, STOP
    // and SET_DESIRED_PAUSE_STATE set the desired state to NOT_PLAYING, and
    // the player confirms with `playbackState` up to a second later
    // (verified live). Reading PLAYING meanwhile published our own pause as
    // the user's, a second late. So a requested pause wins; otherwise trust
    // the explicit states, and the intended one in between (loading,
    // stalled).
    function isPlaying(controls) {
        if (controls.desiredPlaybackState === "NOT_PLAYING") return false;
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

    /**
     * After a remote change: compare later store changes with the state it
     * produced, not with the state before it.
     *
     * Verified live: TIDAL's store keeps changing for about a second after
     * a PAUSE or CLEAR_QUEUE (player confirmations, other slices), which kept
     * pushing the 180 ms debounce back until the suppression window had
     * passed. The one comparison then made was against the state from
     * before the change, so the change itself was published as the user's.
     */
    function rebaseLocalChanges() {
        if (!store) return;
        clearTimeout(storeEventTimer);
        storeEventTimer = null;
        pendingStoreState = null;
        previousStoreState = store.getState();
        noteActiveTrack(previousStoreState);
        setRemoteMutationWindow();
    }

    function setPlaying(isPlaying) {
        dispatch(isPlaying ? "playbackControls/PLAY" : "playbackControls/PAUSE");
    }

    /**
     * Before resuming: put the player where the store (and so the session)
     * says it is paused.
     *
     * Verified live: on a track loaded paused ("prefetched"), PLAY reloads
     * the track from the player's position, and then TIDAL's play saga
     * sometimes loads it once more, from 0 (seen with every song loaded
     * right after another one had played). A seek right before PLAY covers
     * the first; the hold in `playback.setPlaying` catches the second.
     * `startLoadedTrack` leaves the store at the session's position.
     */
    function alignPlayerBeforePlay() {
        if (!store) return;
        const state = store.getState();
        const playback = getPlayback(state);
        if (playback.isPlaying || !state?.playbackControls?.mediaProduct) return;
        // Not only when the player's position looks wrong: that position
        // is cached by the SDK and was right in runs that still started at
        // 0. While paused, a seek to the same spot can't be heard.
        seek(playback.positionMs);
    }

    // Where Attune's last seek should land, so the position jump it causes
    // isn't taken for the user's. Verified live: TIDAL writes the new
    // position only once the player has sought, up to a second later.
    const OWN_SEEK_MEMORY_MS = 5_000;
    let ownSeek = null;

    function seek(positionMs, { own = true } = {}) {
        if (!Number.isFinite(positionMs) || positionMs < 0) {
            throw new TypeError("positionMs must be a non-negative number");
        }
        if (own) ownSeek = { positionMs, at: Date.now() };
        // TIDAL's internal seek action uses seconds, while Attune uses ms.
        dispatch("playbackControls/SEEK", positionMs / 1000);
    }

    function isOwnSeek(playback) {
        if (!ownSeek || Date.now() - ownSeek.at > OWN_SEEK_MEMORY_MS) return false;
        const expected = ownSeek.positionMs +
            (playback.isPlaying ? Math.max(0, playback.positionCapturedAtMs - ownSeek.at) : 0);
        return Math.abs(playback.positionMs - expected) <= TRACK_HOLD_TOLERANCE_MS;
    }

    /** Seek, and wait (at most 1.5 s) until TIDAL reports the new position. */
    async function seekAndWait(positionMs) {
        seek(positionMs);
        const at = Date.now();
        await waitFor(state => {
            const now = getPlayback(state);
            const target = positionMs + (now.isPlaying ? Date.now() - at : 0);
            return Math.abs(livePositionMs(now) - target) <= TRACK_HOLD_TOLERANCE_MS;
        }, 1_500);
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
        assertLocalPlayback();
        // The session keeps playing while songs are looked up and loaded.
        const calledAt = Date.now();

        if (entries.length === 0) {
            // Verified live: CLEAR_QUEUE empties the queue, unloads the
            // current track (media product null, IDLE) and stops the sound
            // within 0.3 s. It also resets shuffle and repeat, which are off
            // during a session anyway.
            setRemoteMutationWindow();
            if (store.getState()?.playbackControls?.mediaProduct ||
                (store.getState()?.playQueue?.elements ?? []).length) {
                dispatch("playQueue/CLEAR_QUEUE");
            }
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

        // The player needs TIDAL's metadata for every element.
        const missing = new Set(await ensureMediaItems(loaded.map(result => result.productId)));
        if (missing.has(String(current.productId))) {
            throw trackUnavailableError([{
                entry: current.entry,
                error: new TrackLookupError(`TIDAL could not load track ${current.productId}`),
            }]);
        }
        const playable = loaded.filter(result => !missing.has(String(result.productId)));
        const unloadable = loaded.filter(result => missing.has(String(result.productId)));

        writeQueue(playable);
        if (playback) await startLoadedTrack(current.productId, playback, calledAt);

        return {
            unavailableEntryIds: [...failures, ...unloadable].map(failure => failure.entry.entryId),
        };
    }

    // Read in TIDAL's reducer (not tested live): while casting (TIDAL
    // Connect, Chromecast, Sonos) the queue type is "cloud" and the queue
    // lives on the other device. TIDAL ignores most queue actions then, but
    // RESET would still overwrite the local copy. The cloud queue
    // ("cloudV2", behind a server flag that was off) keeps the queue on
    // TIDAL's servers and is untested.
    function assertLocalPlayback() {
        if (store.getState()?.playQueue?.type === "cloud") {
            throw new Error("TIDAL is playing on another device (TIDAL Connect or casting); Attune can only follow TIDAL playing on this computer");
        }
    }

    function queueElement(result) {
        return {
            context: SOURCE_CONTEXT,
            mediaItemId: String(result.productId),
            priority: "priority_none",
            uid: uidForEntry(result.entry.entryId),
        };
    }

    /**
     * Make `results` ({ entry, productId }, current first) TIDAL's whole
     * queue. This replaced `ADD_MEDIA_ITEMS_TO_QUEUE` with
     * `overwritePlayQueue`, which restarted the current song and started a
     * new one playing. Verified live (TIDAL 2.48):
     *
     * - **The current song stays** (TIDAL's current element is that track
     *   and the player has it): `CLEAR_UPCOMING` keeps only the current
     *   element, `APPEND_ELEMENTS` adds the rest. Neither reaches the
     *   player; the song played on without a gap.
     * - **Another song**: `RESET` sets all elements. The player saga then
     *   loads the current one *without playing it*, so a paused join is
     *   never heard; `startLoadedTrack` then seeks and plays.
     *   RESET doesn't keep a song that is already playing: TIDAL's "player
     *   has it" check reads `localStorage.playerState`, which TIDAL only
     *   writes when the window is hidden or shown. So it reloaded the song
     *   paused from 0 in most tries, which is why the case above exists.
     *
     * Either way we choose the uids, so every element carries its session
     * entry ID, which also survives a bridge reload.
     */
    function writeQueue(results) {
        const state = store.getState();
        const queue = state?.playQueue;
        const controls = state?.playbackControls;
        const firstId = String(results[0].productId);
        const currentElement = queue?.elements?.[queue?.currentIndex];
        const keepsTrack = currentElement != null &&
            String(currentElement.mediaItemId) === firstId &&
            String(controls?.mediaProduct?.productId) === firstId &&
            controls?.mediaProduct?.referenceId === currentElement.uid &&
            !controls?.prefilled;

        setRemoteMutationWindow();
        if (keepsTrack) {
            dispatch("playQueue/CLEAR_UPCOMING");
            if (results.length > 1) {
                dispatch("playQueue/APPEND_ELEMENTS", { elements: results.slice(1).map(queueElement) });
            }
            entryIdByQueueUid.set(String(currentElement.uid), results[0].entry.entryId);
        } else {
            dispatch("playQueue/RESET", { currentIndex: 0, elements: results.map(queueElement) });
        }
    }

    function livePositionMs(playback) {
        return playback.positionMs + (playback.isPlaying ? Math.max(0, Date.now() - playback.positionCapturedAtMs) : 0);
    }

    /**
     * Where the player really is, in ms: the SDK's own position if it was
     * found, else the store's.
     *
     * Verified live: a track loaded paused (RESET) is "prefetched", and
     * PLAY on it reloads it from the player's own position. A seek made
     * before the player had finished loading was undone by the load (to 0)
     * without the store hearing of it: the store said 12 s, the song then
     * started at 0. Only the player's position shows that.
     */
    function actualPositionMs(state) {
        if (getPlayerPosition) {
            try {
                const seconds = Number(getPlayerPosition());
                if (Number.isFinite(seconds)) return Math.round(seconds * 1000);
            } catch {
                // Fall back to the store.
            }
        }
        return livePositionMs(getPlayback(state));
    }

    /**
     * Bring a freshly loaded track to the session's position and play state,
     * and keep it there until it stays put.
     *
     * Verified live (TIDAL 2.48, after `playQueue/RESET`):
     * - The new track loads paused. `playbackControls.prefilled` is true from
     *   the RESET until the player has it (MEDIA_PRODUCT_TRANSITION, 40–200
     *   ms). A SEEK before that goes to the previous track and is lost; a
     *   PLAY waits for the load.
     * - Seeking a track that was loaded paused worked (15.000 s, then PLAY
     *   continued from there), but once a seek was undone about 230 ms later,
     *   when the player finished loading. So the hold re-applies until the
     *   state stayed put; while paused it waits longer, as there is nothing
     *   to hear meanwhile.
     * - The seek can leave `playbackState` at STALLED, which sets
     *   `desiredPlaybackState` back to PLAYING; the PAUSE dispatched after it
     *   wins (`isPlaying` then reads false).
     * Local changes aren't reported meanwhile: none of this is the user's
     * doing.
     */
    async function startLoadedTrack(productId, playback, calledAt) {
        // `positionMs` is valid at `positionCapturedAtMs` (local clock; the
        // runtime stamps snapshots so). Internal callers pass TIDAL's own
        // playback, whose sync point can be seconds old: anchoring those
        // at the call made a kept song jump back (verified live, 0.8 s).
        const capturedAt = Number(playback.positionCapturedAtMs);
        const anchor = Number.isFinite(capturedAt) && Math.abs(Date.now() - capturedAt) < MAX_SYNC_SKEW_MS
            ? capturedAt
            : calledAt;
        const target = () => (playback.positionMs ?? 0) + (playback.isPlaying ? Math.max(0, Date.now() - anchor) : 0);
        const wantPlaying = Boolean(playback.isPlaying);
        const loaded = state =>
            String(state?.playbackControls?.mediaProduct?.productId) === String(productId) &&
            !state?.playbackControls?.prefilled;
        // Verified live: a track loaded paused by RESET keeps the previous
        // track's `desiredPlaybackState: PLAYING`, so `isPlaying` reads true
        // although nothing plays. Playing counts only once the player says
        // so; a pause once it was asked for.
        const playStateHolds = state => wantPlaying
            ? state.playbackControls.playbackState === "PLAYING"
            : !isPlaying(state.playbackControls);
        // The player's own position and the store's can disagree either way
        // after a paused load (verified live: player 0 and store 12, and
        // player 12 and store 0). The session follows the store and PLAY
        // follows the player, so both have to be right.
        const near = value => Math.abs(value - target()) <= TRACK_HOLD_TOLERANCE_MS;
        const playerHolds = state => near(actualPositionMs(state));
        const storeHolds = state => near(livePositionMs(getPlayback(state)));
        const positionHolds = state => playerHolds(state) && storeHolds(state);
        let commandedAt = null;

        await holdUntil({
            timeoutMs: TRACK_START_TIMEOUT_MS,
            stableMs: wantPlaying ? TRACK_HOLD_STABLE_MS : TRACK_HOLD_PAUSED_STABLE_MS,
            matches: state => loaded(state) && playStateHolds(state) && positionHolds(state),
            correct(state) {
                if (!loaded(state)) return false; // a seek now would be lost
                // First: seek, then play or pause (PLAY makes the player
                // load the track for real, keeping the position: 170 ms).
                if (commandedAt == null) {
                    if (!positionHolds(state)) seek(target());
                    setPlaying(wantPlaying);
                    commandedAt = Date.now();
                    return true;
                }
                if (!playStateHolds(state)) {
                    // A PLAY on a stopped player reloads the track, so
                    // don't repeat it while that's under way.
                    if (Date.now() - commandedAt < PLAY_STATE_RETRY_MS) return false;
                    setPlaying(wantPlaying);
                    commandedAt = Date.now();
                    return true;
                }
                if (!playerHolds(state)) {
                    seek(target());
                    return true;
                }
                if (!storeHolds(state)) {
                    // The player is right: only tell the store (no seek).
                    dispatch("playbackControls/TIME_UPDATE", target() / 1000);
                    return true;
                }
                return false;
            },
        });
        // If the player still isn't there, the store at least says where the
        // session is; `alignPlayerBeforePlay` puts the player there on PLAY.
        if (!wantPlaying && store && loaded(store.getState()) && !storeHolds(store.getState())) {
            dispatch("playbackControls/TIME_UPDATE", target() / 1000);
        }
    }

    /**
     * Keep correcting TIDAL toward a target until it has matched for
     * `stableMs`, or `timeoutMs` passes. Its store states don't reliably say
     * when an action has taken effect (a loading track can still undo a pause,
     * a reload restores and resumes by itself), so instead of waiting for one
     * signal, re-apply whenever it differs. Local changes aren't reported
     * meanwhile: none of this is the user's doing.
     */
    //
    // Checked on every store change, plus a timer for the stable period and
    // retries (see `waitFor`: timers run once a second while TIDAL is hidden
    // and silent, which is exactly while a track loads paused). `correct`
    // returns whether it dispatched anything; only then is the next
    // correction held back by TRACK_HOLD_RETRY_MS.
    function holdUntil({ timeoutMs, stableMs, matches, correct }) {
        suppressLocalChangesUntil = Date.now() + timeoutMs + REMOTE_CHANGE_SUPPRESSION_MS;
        const deadline = Date.now() + timeoutMs;
        return new Promise((resolve, reject) => {
            let matchingSince = null;
            let lastCorrected = 0;
            let timer = null;
            let done = false;
            let scheduled = false;
            let unsubscribe = () => {};

            const finish = (error, value) => {
                if (done) return;
                done = true;
                clearTimeout(timer);
                unsubscribe();
                setRemoteMutationWindow();
                if (error) reject(error);
                else resolve(value);
            };
            const check = () => {
                scheduled = false;
                if (done) return;
                if (!store) return finish(null, false);
                const now = Date.now();
                if (now >= deadline) return finish(null, false);
                const state = store.getState();
                if (matches(state)) {
                    matchingSince ??= now;
                    if (now - matchingSince >= stableMs) return finish(null, true);
                } else {
                    matchingSince = null;
                    if (now - lastCorrected >= TRACK_HOLD_RETRY_MS) {
                        try {
                            if (correct(state)) lastCorrected = Date.now();
                        } catch (error) {
                            return finish(error);
                        }
                    }
                }
                clearTimeout(timer);
                const wait = matchingSince != null
                    ? stableMs - (Date.now() - matchingSince)
                    : TRACK_HOLD_RETRY_MS - (Date.now() - lastCorrected);
                timer = setTimeout(check, Math.max(TRACK_START_POLL_MS, Math.min(wait, deadline - Date.now())));
            };
            // Not inside TIDAL's dispatch: our corrections dispatch too.
            const schedule = () => {
                if (scheduled || done) return;
                scheduled = true;
                Promise.resolve().then(check);
            };
            unsubscribe = store.subscribe(schedule);
            check();
        });
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
                correct: () => {
                    setPlaying(false);
                    return true;
                },
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

    // The playback is re-stamped as of now, so callers can overwrite
    // `positionMs` with a position meant as "now".
    function cloneState(state) {
        return {
            queue: state.queue.map(entry => ({
                entryId: entry.entryId,
                track: { ...entry.track, artists: [...entry.track.artists] },
            })),
            currentEntryId: state.currentEntryId,
            playback: {
                ...state.playback,
                positionMs: livePositionMs(state.playback),
                positionCapturedAtMs: Date.now(),
            },
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
                const playback = cloneState(currentState).playback;
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
                await seekAndWait(operation.positionMs);
                return { unavailableEntryIds: [] };
            }

            case "playback.setPlaying": {
                const state = store.getState();
                const productId = state?.playbackControls?.mediaProduct?.productId;
                const paused = !getPlayback(state).isPlaying;
                if (operation.isPlaying && paused && productId != null) {
                    // Resuming can reload the track (see alignPlayerBeforePlay),
                    // so hold it at the position like a freshly loaded one.
                    const positionMs = getPlayback(state).positionMs;
                    alignPlayerBeforePlay();
                    const now = Date.now();
                    await startLoadedTrack(productId, { isPlaying: true, positionMs, positionCapturedAtMs: now }, now);
                } else {
                    setPlaying(Boolean(operation.isPlaying));
                }
                return { unavailableEntryIds: [] };
            }

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
        // Without a current song (e.g. after CLEAR_QUEUE TIDAL still writes
        // the old position, verified live) there is nothing to seek.
        if (after.currentEntryId == null) return false;
        if (before.currentEntryId !== after.currentEntryId) return false;
        if (isOwnSeek(after.playback)) return false;
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
        openApi = null;
        getPlayerPosition = null;
        previousStoreState = null;
        pendingStoreState = null;
        entryIdByQueueUid.clear();
        trackCache.clear();
        resolvedTrackCache.clear();
        isrcCandidates.clear();
        bridge.ready = false;

        if (window[BRIDGE_KEY] === bridge) delete window[BRIDGE_KEY];
        if (window.bridge === bridge) delete window.bridge;
    }

    // Methods that change TIDAL on the session's behalf.
    function remoteChange(apply) {
        return async (...args) => {
            try {
                return await apply(...args);
            } finally {
                rebaseLocalChanges();
            }
        };
    }

    const bridge = {
        version: BRIDGE_VERSION,
        ready: false,
        initialise,
        dispose,
        sendEvent,
        getState,
        getSnapshot: getState,
        applySnapshot: remoteChange(applyState),
        applyOperation: remoteChange(applyOperation),
        settle: remoteChange(settle),
        prepareForSession: remoteChange(prepareForSession),
        restoreAfterSession: remoteChange(restoreAfterSession),

        // Existing controller commands. Keeping these avoids breaking the
        // current Tauri UI while the session commands are integrated.
        getPlayState,
        playTrack,
        pause: async () => setPlaying(false),
        resume: async () => setPlaying(true),
        // A seek from Attune's own controls is published like the user's.
        seek: async positionMs => seek(positionMs, { own: false }),
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
