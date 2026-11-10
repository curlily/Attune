/*
 * MyJam adapter for TIDAL Desktop.
 *
 * This script runs inside TIDAL's renderer. It deliberately keeps all TIDAL
 * implementation details here; Rust only talks to the shared bridge contract.
 * TIDAL's Redux interfaces are private and may change between app releases, so
 * every discovered dependency is checked before it is used.
 */
(async () => {
    "use strict";

    const BRIDGE_KEY = "__MYJAM_BRIDGE_V1__";
    const BRIDGE_VERSION = 1;
    const EVENT_BINDING = "__jamEvent";
    const STORE_TIMEOUT_MS = 15_000;
    const SEARCH_TIMEOUT_MS = 12_000;
    const STORE_EVENT_DEBOUNCE_MS = 180;
    const REMOTE_CHANGE_SUPPRESSION_MS = 1_200;
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

    function getSearchResultIds(query) {
        const key = `search/tracks/${encodeURIComponent(query)}`;
        const list = store.getState()?.content?.trackLists?.[key];
        const ids = list?.sorted?.defaultSort?.items;
        return Array.isArray(ids) ? ids : null;
    }

    async function searchTrackIds(query) {
        const existing = getSearchResultIds(query);
        if (existing?.length) return existing;

        dispatch("search/SEARCH_FOR", {
            query,
            limit: 20,
            offset: 0,
            types: ["TRACKS"],
        });

        const deadline = performance.now() + SEARCH_TIMEOUT_MS;
        while (performance.now() < deadline) {
            const ids = getSearchResultIds(query);
            if (ids?.length) return ids;
            await new Promise(resolve => setTimeout(resolve, 100));
        }

        throw new Error(`TIDAL search timed out for "${query}"`);
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
            throw new Error(`No matching TIDAL recording found for "${track.title}"`);
        }
        if (ranked.length > 1 && ranked[0].score === ranked[1].score) {
            throw new Error(`TIDAL returned an ambiguous match for "${track.title}"; add an ISRC or album`);
        }

        const productId = ranked[0].productId;
        resolvedTrackCache.set(cacheKey, productId);
        trackCache.set(productId, ranked[0].track);
        return productId;
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

        // TIDAL retains played entries before currentIndex. MyJam's queue is
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

    function getPlayback(state) {
        const controls = state?.playbackControls ?? {};
        const positionSeconds = Number(controls.latestCurrentTime ?? 0);
        return {
            isPlaying: controls.playbackState === "PLAYING",
            positionMs: Number.isFinite(positionSeconds)
                ? Math.max(0, Math.round(positionSeconds * 1000))
                : 0,
            // Capture locally. TIDAL's private sync timestamp has not been
            // verified to use the same units in every client build.
            positionCapturedAtMs: Date.now(),
        };
    }

    function stateFromRedux(reduxState) {
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
        // TIDAL's internal seek action uses seconds, while MyJam uses ms.
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

    async function replaceQueue(entries, currentEntryId, playback) {
        if (!Array.isArray(entries)) {
            throw new TypeError("Queue entries must be an array");
        }

        if (entries.length === 0) {
            // We have verified queue replacement, but not a TIDAL action that
            // safely unloads the active item. Do not invent one or claim that
            // an empty session queue was applied when playback still exists.
            if (store.getState()?.playbackControls?.mediaProduct) {
                throw new Error("TIDAL cannot safely unload the active track with the verified bridge actions");
            }
            setPlaying(false);
            return;
        }

        const selectedIndex = currentEntryId == null
            ? 0
            : entries.findIndex(entry => entry.entryId === currentEntryId);
        if (selectedIndex < 0) {
            throw new Error(`Current queue entry was not found: ${currentEntryId}`);
        }

        // Entries before currentEntryId are history, not upcoming queue items.
        const orderedEntries = entries.slice(selectedIndex);

        // Resolve every recording before replacing the active queue. A lookup
        // failure therefore leaves the user's current queue intact.
        const mediaItemIds = await Promise.all(
            orderedEntries.map(entry => resolveTrack(entry.track))
        );

        setRemoteMutationWindow();
        dispatch("playQueue/ADD_MEDIA_ITEMS_TO_QUEUE", {
            mediaItemIds,
            options: { overwritePlayQueue: true },
            position: "now",
            sourceContext: SOURCE_CONTEXT,
        });
        rememberAppliedEntryIds(orderedEntries);

        if (playback?.positionMs > 0) seek(playback.positionMs);
        if (playback) setPlaying(playback.isPlaying);
    }

    async function applyState(nextState) {
        if (!nextState || !Array.isArray(nextState.queue) || !nextState.playback) {
            throw new TypeError("Invalid MyJam music state");
        }

        setRemoteMutationWindow();
        await replaceQueue(
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
            throw new TypeError("Invalid MyJam operation");
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
                await replaceQueue(operation.entries, currentEntryId, playback);
                return;
            }

            case "queue.insert": {
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
                await replaceQueue(state.queue, state.currentEntryId, state.playback);
                return;
            }

            case "queue.remove": {
                const state = cloneState(getState());
                const index = state.queue.findIndex(entry => entry.entryId === operation.entryId);
                if (index < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                state.queue.splice(index, 1);
                if (state.currentEntryId === operation.entryId) {
                    state.currentEntryId = state.queue[0]?.entryId ?? null;
                    state.playback.positionMs = 0;
                }
                await replaceQueue(state.queue, state.currentEntryId, state.playback);
                return;
            }

            case "queue.move": {
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
                await replaceQueue(state.queue, state.currentEntryId, state.playback);
                return;
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
                await replaceQueue(state.queue, targetId, state.playback);
                return;
            }

            case "playback.seek": {
                const state = getState();
                if (operation.entryId !== state.currentEntryId) {
                    throw new Error("Cannot seek a track that is not currently playing");
                }
                seek(operation.positionMs);
                return;
            }

            case "playback.setPlaying":
                setPlaying(Boolean(operation.isPlaying));
                return;

            default:
                throw new Error(`Unsupported MyJam operation: ${operation.type}`);
        }
    }

    // ---------------------------------------------------------------------
    // Local state observation and operation emission
    // ---------------------------------------------------------------------

    function sameQueue(left, right) {
        return left.length === right.length &&
            left.every((entry, index) => entry.entryId === right[index].entryId);
    }

    function positionWasSought(before, after) {
        if (before.currentEntryId !== after.currentEntryId) return false;
        if (before.playback.isPlaying !== after.playback.isPlaying) return false;

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

        return {
            track: track ? {
                ...track,
                provider: "Tidal",
                provider_track_id: String(productId),
                cover_url: null,
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
            console.warn("[MyJam] CDP event binding is unavailable", type);
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
        console.info("[MyJam] TIDAL adapter ready");
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
        console.error("[MyJam] TIDAL adapter initialization failed", error);
        sendEvent("Error", error?.message ?? String(error));
    }

    return bridge;
})();
