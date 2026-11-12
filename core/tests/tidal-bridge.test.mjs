import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

const adapterPath = new URL("../src/adapters/tidal.js", import.meta.url);
const STORE_EVENT_SETTLE_MS = 250; // > the adapter's 180ms debounce
// Loading a queue: the first track loads this long, then starts playing.
const TRACK_LOAD_MS = 60;

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

// Tracks TIDAL knows but hasn't loaded into `content.mediaItems` yet: found
// by ISRC through the open API, loaded with LOAD_SINGLE_MEDIA_ITEM. Text
// search never finds them, so a match proves the ISRC path.
const CATALOG = {
    "404": {
        title: "Fourth Track (feat. Guest)",
        artists: [{ name: "Artist Four" }, { name: "Guest" }],
        album: { title: "Fourth Album" },
        duration: 210,
        isrc: "ISRC-FOURTH",
        streamReady: true,
    },
};

function createStore() {
    let state = {
        view: { appReady: true },
        content: {
            mediaItems: {
                "101": {
                    title: "Current Track",
                    artists: [{ name: "Artist One" }],
                    album: { title: "Current Album" },
                    duration: 180,
                    isrc: "ISRC-CURRENT",
                },
                "202": {
                    title: "Repeated Track",
                    artists: [{ name: "Artist Two" }],
                    album: { title: "Second Album" },
                    duration: 200,
                    isrc: "ISRC-REPEAT",
                    streamReady: true,
                },
                "303": {
                    title: "Third Track",
                    artists: [{ name: "Artist Three" }],
                    album: { title: "Third Album" },
                    duration: 150,
                },
            },
            trackLists: {},
        },
        settings: { autoPlay: true },
        playQueue: {
            type: "local",
            shuffleModeEnabled: false,
            repeatMode: 0,
            currentIndex: 1,
            elements: [
                { uid: "history", mediaItemId: "999" },
                { uid: "current", mediaItemId: "101" },
                { uid: "upcoming-a", mediaItemId: "202" },
                { uid: "upcoming-b", mediaItemId: "202" },
            ],
        },
        playbackControls: {
            mediaProduct: { productId: "101", referenceId: "current" },
            playbackState: "PLAYING",
            latestCurrentTime: 12.5,
            latestCurrentTimeSyncTimestamp: 123,
        },
    };

    const listeners = new Set();
    let generatedUid = 0;
    let pendingSearch = null;
    let pendingStart = null;
    let loadTimers = [];
    let playRequested = false;
    const searches = [];
    const isrcRequests = [];
    // Tracks the player loaded (a reload restarts a song).
    const loads = [];

    function notify() {
        for (const listener of [...listeners]) listener();
    }

    function update(change) {
        state = structuredClone(state);
        change(state);
        notify();
    }

    // Position extrapolated from TIDAL's sync point, in seconds.
    function livePosition(controls) {
        const elapsed = controls.playbackState === "PLAYING"
            ? Math.max(0, Date.now() - controls.latestCurrentTimeSyncTimestamp) / 1000
            : 0;
        return controls.latestCurrentTime + elapsed;
    }

    // TIDAL's player SDK: its own position, which the store doesn't always
    // hear about.
    const sdk = {
        activePlayer: {
            position: 0, // seconds
            prefetched: false,
            getPosition() {
                const controls = state.playbackControls;
                return controls.playbackState === "PLAYING" ? livePosition(controls) : this.position;
            },
        },
    };
    const player = sdk.activePlayer;

    // Like the real player saga after playQueue/RESET (verified live): load
    // the current element paused ("prefetched"); announce it once loaded;
    // then the player finishes loading and puts its position back to 0,
    // undoing a seek made in between, without the store hearing of it.
    function loadCurrentPaused(productId, referenceId) {
        loadTimers.forEach(clearTimeout);
        loads.push(productId);
        playRequested = false;
        player.position = 0;
        player.prefetched = true;
        state.playbackControls.mediaProduct = { productId, referenceId };
        state.playbackControls.prefilled = true;
        loadTimers = [
            setTimeout(() => update(s => {
                const c = s.playbackControls;
                c.prefilled = false;
                c.latestCurrentTime = 0;
                c.latestCurrentTimeSyncTimestamp = Date.now();
                c.playbackState = "NOT_PLAYING";
                if (playRequested) startPrefetched(c);
            }), TRACK_LOAD_MS),
            setTimeout(() => {
                if (state.playbackControls.playbackState !== "PLAYING") player.position = 0;
            }, TRACK_LOAD_MS * 2),
        ];
    }

    // PLAY on a prefetched track reloads it from the player's own position.
    function startPrefetched(controls) {
        player.prefetched = false;
        controls.latestCurrentTime = player.position;
        controls.latestCurrentTimeSyncTimestamp = Date.now();
        controls.playbackState = "PLAYING";
    }

    // TIDAL's open API client (RTK Query): `initiate` returns a thunk.
    const openApi = {
        reducerPath: "tidalOpenPlatformApi",
        endpoints: {
            getTracks: {
                initiate: args => async () => {
                    const isrcs = args.filter?.isrc ?? [];
                    isrcRequests.push([...isrcs]);
                    await sleep(5);
                    if (isrcs.length > 20) {
                        return { error: { status: 400, data: { errors: [{ code: "INVALID_ARRAY_LENGTH" }] } } };
                    }
                    const known = { ...state.content.mediaItems, ...CATALOG };
                    // ISRCs are compared without separators ("US-AB1-…").
                    const plain = value => value.replace(/-/g, "");
                    const hits = Object.entries(known).filter(([, item]) => item.isrc && isrcs.includes(plain(item.isrc)));
                    return {
                        data: {
                            data: hits.map(([id, item]) => ({
                                id,
                                type: "tracks",
                                attributes: {
                                    title: item.title,
                                    isrc: item.isrc,
                                    duration: `PT${Math.floor(item.duration / 60)}M${item.duration % 60}S`,
                                    availability: ["STREAM", "DJ"],
                                },
                                relationships: {
                                    artists: { data: item.artists.map((_, index) => ({ id: `${id}-artist-${index}`, type: "artists" })) },
                                    albums: { data: [{ id: `${id}-album`, type: "albums" }] },
                                },
                            })),
                            included: hits.flatMap(([id, item]) => [
                                ...item.artists.map((artist, index) => ({ id: `${id}-artist-${index}`, type: "artists", attributes: { name: artist.name } })),
                                { id: `${id}-album`, type: "albums", attributes: { title: item.album.title } },
                            ]),
                        },
                    };
                },
            },
        },
    };

    const store = {
        searches,
        isrcRequests,
        loads,
        openApi,
        sdk,
        getState: () => state,
        subscribe(listener) {
            listeners.add(listener);
            return () => listeners.delete(listener);
        },
        dispatch(action) {
            if (typeof action === "function") return action(store.dispatch, store.getState);
            const { type, payload } = action;
            // Like Redux, never mutate the previous state object: the adapter
            // keeps it to compare before/after.
            state = structuredClone(state);

            if (type === "search/SEARCH_FOR") {
                // A query naming a known track returns it; "ghost" queries finish
                // with no results (like real TIDAL); anything else returns
                // "Current Track", which then fails the title match.
                // Like real TIDAL, results arrive asynchronously and a new
                // search cancels one still in flight.
                searches.push(payload.query);
                clearTimeout(pendingSearch);
                const query = payload.query.toLowerCase();
                const id = query.includes("repeated track") ? "202" : query.includes("third track") ? "303" : "101";
                const items = query.includes("ghost") ? [] : [id];
                const queryKey = `search/tracks/${encodeURIComponent(payload.query)}`;
                pendingSearch = setTimeout(() => update(s => {
                    s.content.trackLists[queryKey] = { sorted: { defaultSort: { items, loading: false } } };
                }), 10);
            } else if (type === "playQueue/ADD_MEDIA_ITEMS_TO_QUEUE") {
                state.playQueue = {
                    type: "local",
                    currentIndex: 0,
                    elements: payload.mediaItemIds.map(id => ({
                        uid: `new-${++generatedUid}`,
                        mediaItemId: String(id),
                    })),
                };
                state.playbackControls.mediaProduct = payload.mediaItemIds.length
                    ? { productId: String(payload.mediaItemIds[0]) }
                    : null;
                state.playbackControls.latestCurrentTime = 0;
                // Like TIDAL (verified live): the previous track's PLAYING
                // lingers for a moment, then the new track loads, then starts
                // playing from 0, undoing a pause or seek made before that.
                state.playbackControls.desiredPlaybackState = "PLAYING";
                clearTimeout(pendingStart);
                setTimeout(() => update(s => {
                    s.playbackControls.playbackState = "STALLED";
                }), TRACK_LOAD_MS / 3);
                pendingStart = setTimeout(() => update(s => {
                    s.playbackControls.playbackState = "PLAYING";
                    s.playbackControls.latestCurrentTime = 0;
                    s.playbackControls.latestCurrentTimeSyncTimestamp = Date.now();
                }), TRACK_LOAD_MS);
            } else if (type === "playQueue/RESET") {
                // Even the song already playing is loaded again, paused
                // (TIDAL's "player has it" check reads a value saved only
                // when the window is hidden; verified live).
                state.playQueue.elements = payload.elements;
                state.playQueue.currentIndex = payload.currentIndex ?? state.playQueue.currentIndex;
                const current = payload.elements[state.playQueue.currentIndex];
                if (current) loadCurrentPaused(String(current.mediaItemId), current.uid);
            } else if (type === "playQueue/CLEAR_UPCOMING") {
                const queue = state.playQueue;
                queue.elements = queue.elements.slice(queue.currentIndex, queue.currentIndex + 1);
                queue.currentIndex = 0;
            } else if (type === "playQueue/APPEND_ELEMENTS") {
                state.playQueue.elements.push(...payload.elements);
            } else if (type === "playQueue/CLEAR_QUEUE") {
                loadTimers.forEach(clearTimeout);
                Object.assign(state.playQueue, { elements: [], currentIndex: 0, repeatMode: 0, shuffleModeEnabled: false });
                Object.assign(state.playbackControls, {
                    mediaProduct: null,
                    playbackState: "IDLE",
                    desiredPlaybackState: "NOT_PLAYING",
                    latestCurrentTime: 0,
                    latestCurrentTimeSyncTimestamp: Date.now(),
                });
            } else if (type === "playbackControls/TIME_UPDATE") {
                state.playbackControls.latestCurrentTime = payload;
                state.playbackControls.latestCurrentTimeSyncTimestamp = Date.now();
            } else if (type === "content/LOAD_SINGLE_MEDIA_ITEM") {
                const item = CATALOG[payload.id];
                if (item) setTimeout(() => update(s => { s.content.mediaItems[payload.id] = item; }), 10);
            } else if (type === "playQueue/ADD_AT_INDEX") {
                state.playQueue.elements.splice(payload.index, 0, ...payload.mediaItemIds.map(id => ({
                    uid: `new-${++generatedUid}`,
                    mediaItemId: String(id),
                })));
            } else if (type === "playQueue/REMOVE_ELEMENT") {
                const queue = state.playQueue;
                const index = queue.elements.findIndex(element => element.uid === payload.uid);
                if (index >= 0 && index !== queue.currentIndex) {
                    queue.elements.splice(index, 1);
                    if (index < queue.currentIndex) queue.currentIndex -= 1;
                }
            } else if (type === "playQueue/MOVE_TRACK") {
                // Like TIDAL: insert at toIndex, then remove the original;
                // the moved element gets a new uid.
                const elements = state.playQueue.elements;
                const from = payload.fromIndex;
                const to = Math.min(payload.toIndex, elements.length);
                elements.splice(to, 0, { ...elements[from], uid: `moved-${++generatedUid}` });
                elements.splice(from < to ? from : from + 1, 1);
            } else if (type === "playQueue/DISABLE_SHUFFLE_MODE") {
                state.playQueue.shuffleModeEnabled = false;
            } else if (type === "playQueue/ENABLE_SHUFFLE_MODE") {
                state.playQueue.shuffleModeEnabled = true;
            } else if (type === "playQueue/SET_REPEAT_MODE") {
                state.playQueue.repeatMode = payload;
            } else if (type === "settings/SET_AUTOPLAY") {
                state.settings.autoPlay = payload;
            } else if (type === "test/SET_QUEUE") {
                state.playQueue.elements = payload.elements;
                state.playQueue.currentIndex = 0;
                state.playbackControls.mediaProduct = payload.elements.length
                    ? { productId: payload.elements[0].mediaItemId }
                    : null;
            } else if (type === "test/SET_PLAYBACK") {
                Object.assign(state.playbackControls, payload);
            } else if (type === "playbackControls/SEEK") {
                // Before the player has the new track a seek is lost.
                if (!state.playbackControls.prefilled) {
                    player.position = payload;
                    state.playbackControls.latestCurrentTime = payload;
                    state.playbackControls.latestCurrentTimeSyncTimestamp = Date.now();
                }
            } else if (type === "playbackControls/PAUSE") {
                const controls = state.playbackControls;
                if (controls.playbackState === "PLAYING") player.position = livePosition(controls);
                controls.latestCurrentTime = livePosition(controls);
                controls.latestCurrentTimeSyncTimestamp = Date.now();
                controls.desiredPlaybackState = "NOT_PLAYING";
                if (!controls.prefilled) controls.playbackState = "NOT_PLAYING";
                playRequested = false;
            } else if (type === "playbackControls/PLAY") {
                const controls = state.playbackControls;
                controls.desiredPlaybackState = "PLAYING";
                // A play during the load waits for it.
                if (controls.prefilled) {
                    playRequested = true;
                } else if (player.prefetched) {
                    startPrefetched(controls);
                } else if (controls.playbackState !== "PLAYING") {
                    controls.playbackState = "PLAYING";
                    controls.latestCurrentTimeSyncTimestamp = Date.now();
                }
            }

            notify();
        },
    };
    return store;
}

async function loadAdapter({ store = createStore(), withOpenApi = true } = {}) {
    const events = [];
    const context = { observerDisconnected: false };
    let randomId = 0;

    // The renderer script dynamically imports TIDAL chunks. Replace only that
    // import site so this test can supply a deterministic mock store accessor.
    const original = await readFile(adapterPath, "utf8");
    const source = original.replace("await import(url)", "await loadModule(url)");
    assert.notEqual(source, original, "test import hook was not applied");

    function getGlobalStore() {
        if (!store) throw new Error("No global store set");
        return store;
    }
    // Same shape as the SDK's export, which the adapter finds by its code.
    const sdk = store.sdk;
    function getPosition() { return sdk.activePlayer?.getPosition() ?? 0; }

    const vmContext = vm.createContext({
        window: {},
        document: { querySelectorAll: () => [] },
        performance: {
            now: () => Date.now(),
            getEntriesByType: () => [{ name: "mock://tidal/renderer.js" }],
        },
        PerformanceObserver: class {
            observe() {}
            disconnect() { context.observerDisconnected = true; }
        },
        crypto: { randomUUID: () => `generated-${++randomId}` },
        loadModule: async () => withOpenApi
            ? { getGlobalStore, getPosition, openApi: store.openApi }
            : { getGlobalStore, getPosition },
        structuredClone,
        setTimeout,
        clearTimeout,
        console: { info() {}, log() {}, warn() {}, error() {} },
    });
    vmContext.window.__jamEvent = payload => events.push(JSON.parse(payload));

    const script = new vm.Script(source, { filename: fileURLToPath(adapterPath) });
    const bridge = await script.runInContext(vmContext);
    return { bridge, store, events, context, window: vmContext.window };
}

const repeatedTrack = {
    title: "Repeated Track",
    artists: ["Artist Two"],
    isrc: "ISRC-REPEAT",
    album: "Second Album",
    durationMs: 200_000,
};
const ghostTrack = { title: "Ghost Song", artists: ["Nobody"] };
const thirdTrack = { title: "Third Track", artists: ["Artist Three"], album: "Third Album", durationMs: 150_000 };

function playback(isPlaying = true, positionMs = 0) {
    return { isPlaying, positionMs, positionCapturedAtMs: Date.now() };
}

const entryIds = state => Array.from(state.queue, entry => entry.entryId);
const operations = events => events.filter(event => event.type === "Operation");

test("TIDAL adapter exposes the shared API and normalizes queue history/duplicates", async () => {
    const { bridge, events, context, window } = await loadAdapter();

    assert.equal(bridge.version, 1);
    assert.equal(bridge.ready, true);
    assert.ok(events.some(event => event.type === "Ready"));

    const initialState = bridge.getState();
    assert.equal(initialState.queue.length, 3, "played history should be excluded");
    assert.equal(initialState.queue[0].track.title, "Current Track");
    assert.notEqual(
        initialState.queue[1].entryId,
        initialState.queue[2].entryId,
        "duplicate tracks must keep distinct queue-entry identities",
    );

    const outcome = await bridge.applySnapshot({
        queue: [
            { entryId: "session-current", track: repeatedTrack },
            { entryId: "session-copy", track: repeatedTrack },
        ],
        currentEntryId: "session-current",
        playback: playback(false, 5_000),
    });
    assert.deepEqual([...outcome.unavailableEntryIds], []);

    const appliedState = bridge.getState();
    assert.deepEqual(entryIds(appliedState), ["session-current", "session-copy"]);
    assert.equal(appliedState.playback.positionMs, 5_000);
    assert.equal(appliedState.playback.isPlaying, false);

    const playOutcome = await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: true });
    assert.deepEqual([...playOutcome.unavailableEntryIds], []);
    assert.equal(bridge.getState().playback.isPlaying, true);

    await bridge.dispose();
    assert.equal(bridge.ready, false);
    assert.equal(context.observerDisconnected, true);
    assert.equal(window.__ATTUNE_BRIDGE_V1__, undefined);
    assert.equal(window.bridge, undefined);
});

test("TIDAL skips upcoming tracks it cannot find and reports them", async () => {
    const { bridge } = await loadAdapter();

    // "Third Track" is searched before "Ghost Song". If searches overlapped,
    // the later one would cancel it (as real TIDAL does) and it would time out.
    const outcome = await bridge.applySnapshot({
        queue: [
            { entryId: "session-current", track: repeatedTrack },
            { entryId: "session-third", track: thirdTrack },
            { entryId: "session-ghost", track: ghostTrack },
            { entryId: "session-copy", track: repeatedTrack },
        ],
        currentEntryId: "session-current",
        playback: playback(),
    });

    assert.deepEqual([...outcome.unavailableEntryIds], ["session-ghost"]);
    assert.deepEqual(
        entryIds(bridge.getState()),
        ["session-current", "session-third", "session-copy"],
        "loaded entries keep their session IDs even after a skipped one",
    );
});

test("TIDAL does not search for entries already in its queue", async () => {
    const { bridge, store } = await loadAdapter();
    const state = bridge.getState();

    // Same entries, reordered: everything is known by entry ID.
    const [current, upcomingA, upcomingB] = state.queue;
    const outcome = await bridge.applySnapshot({
        queue: [current, upcomingB, upcomingA],
        currentEntryId: current.entryId,
        playback: playback(),
    });

    assert.deepEqual([...outcome.unavailableEntryIds], []);
    assert.deepEqual([...store.searches], []);
    assert.deepEqual(entryIds(bridge.getState()), [current.entryId, upcomingB.entryId, upcomingA.entryId]);
});

test("TIDAL rejects an unplayable current track with a structured error and changes nothing", async () => {
    const { bridge, store } = await loadAdapter();
    const before = entryIds(bridge.getState());

    await assert.rejects(
        bridge.applySnapshot({
            queue: [
                { entryId: "session-ghost", track: ghostTrack },
                { entryId: "session-wrong-isrc", track: { ...repeatedTrack, isrc: "ISRC-DIFFERENT" } },
                { entryId: "session-ok", track: repeatedTrack },
            ],
            currentEntryId: "session-ghost",
            playback: playback(),
        }),
        error => {
            assert.equal(error.code, "trackUnavailable");
            assert.deepEqual([...error.details.entryIds], ["session-ghost"]);
            assert.match(error.message, /No matching TIDAL recording found for "Ghost Song"/);
            return true;
        },
    );
    assert.deepEqual(entryIds(bridge.getState()), before, "TIDAL queue must be untouched");
    assert.deepEqual([...store.searches], ["Ghost Song Nobody"], "the rest is not searched once the current fails");
});

test("TIDAL publishes local changes but not the effects of a remote apply", async () => {
    const { bridge, store, events } = await loadAdapter();

    // Positive control: a user pausing in TIDAL becomes a session operation.
    store.dispatch({ type: "playbackControls/PAUSE" });
    await sleep(STORE_EVENT_SETTLE_MS);
    assert.deepEqual(
        operations(events).map(event => event.data),
        [{ type: "playback.setPlaying", isPlaying: false }],
    );

    // A remote apply changes the queue and play state, but must not echo.
    events.length = 0;
    await bridge.applySnapshot({
        queue: [{ entryId: "session-current", track: repeatedTrack }],
        currentEntryId: "session-current",
        playback: playback(),
    });
    await sleep(STORE_EVENT_SETTLE_MS);
    assert.deepEqual(operations(events), []);
});

test("TIDAL position is reported with the time it was captured, not the time it was read", async () => {
    const { bridge, store } = await loadAdapter();

    // TIDAL's position is a sync point: valid at its timestamp, frozen since.
    const syncedAt = Date.now() - 5_000;
    store.getState().playbackControls.latestCurrentTime = 10;
    store.getState().playbackControls.latestCurrentTimeSyncTimestamp = syncedAt;
    const playback = bridge.getState().playback;
    assert.equal(playback.positionMs, 10_000);
    assert.equal(playback.positionCapturedAtMs, syncedAt);

    // An implausible timestamp (other unit, missing) falls back to "now".
    store.getState().playbackControls.latestCurrentTimeSyncTimestamp = 123;
    const before = Date.now();
    assert.ok(bridge.getState().playback.positionCapturedAtMs >= before);
});

test("TIDAL play state: loading is not a pause, a pause is a pause", async () => {
    const { bridge, store } = await loadAdapter();
    const controls = store.getState().playbackControls;

    // While the next track loads, playbackState leaves PLAYING but the
    // intended state stays PLAYING.
    controls.playbackState = "STALLED";
    controls.desiredPlaybackState = "PLAYING";
    assert.equal(bridge.getState().playback.isPlaying, true);

    // After a PAUSE action TIDAL keeps desiredPlaybackState at PLAYING.
    controls.playbackState = "PAUSED";
    controls.desiredPlaybackState = "PLAYING";
    assert.equal(bridge.getState().playback.isPlaying, false);
});

test("TIDAL edits upcoming entries in place without restarting the current track", async () => {
    const { bridge, store } = await loadAdapter();
    const dispatched = [];
    const dispatch = store.dispatch.bind(store);
    store.dispatch = action => { dispatched.push(action.type); dispatch(action); };

    const [current, upcomingA, upcomingB] = bridge.getState().queue.map(entry => entry.entryId);
    store.getState().playbackControls.latestCurrentTime = 42;

    await bridge.applyOperation({ type: "queue.insert", index: 1, entry: { entryId: "session-third", track: thirdTrack } });
    assert.deepEqual(entryIds(bridge.getState()), [current, "session-third", upcomingA, upcomingB]);

    await bridge.applyOperation({ type: "queue.move", entryId: "session-third", toIndex: 3 });
    assert.deepEqual(entryIds(bridge.getState()), [current, upcomingA, upcomingB, "session-third"]);

    await bridge.applyOperation({ type: "queue.move", entryId: upcomingB, toIndex: 1 });
    assert.deepEqual(entryIds(bridge.getState()), [current, upcomingB, upcomingA, "session-third"]);

    await bridge.applyOperation({ type: "queue.remove", entryId: upcomingA });
    assert.deepEqual(entryIds(bridge.getState()), [current, upcomingB, "session-third"]);

    assert.ok(!dispatched.some(type => type === "playQueue/ADD_MEDIA_ITEMS_TO_QUEUE" || type === "playQueue/RESET"),
        `queue was replaced: ${dispatched}`);
    assert.equal(bridge.getState().playback.positionMs, 42_000, "current track kept its position");
});

test("TIDAL reports an unfindable inserted song without changing the queue", async () => {
    const { bridge } = await loadAdapter();
    const before = entryIds(bridge.getState());
    const outcome = await bridge.applyOperation({ type: "queue.insert", index: 1, entry: { entryId: "session-ghost", track: ghostTrack } });
    assert.deepEqual([...outcome.unavailableEntryIds], ["session-ghost"]);
    assert.deepEqual(entryIds(bridge.getState()), before);
});

test("TIDAL resetting the position when a track starts is not a seek", async () => {
    const { store, events } = await loadAdapter();
    const set = payload => store.dispatch({ type: "test/SET_PLAYBACK", payload });

    // The next track is requested: position 0 as of now...
    set({ mediaProduct: { productId: "202", referenceId: "upcoming-a" }, latestCurrentTime: 0, latestCurrentTimeSyncTimestamp: Date.now() - 3_000 });
    await sleep(STORE_EVENT_SETTLE_MS);
    events.length = 0;
    // ...and only starts playing 3 s later, when TIDAL syncs to 0 again.
    set({ latestCurrentTime: 0, latestCurrentTimeSyncTimestamp: Date.now() });
    await sleep(STORE_EVENT_SETTLE_MS);
    assert.deepEqual(operations(events).map(event => event.data.type), []);

    // A real seek is still reported.
    set({ latestCurrentTime: 90, latestCurrentTimeSyncTimestamp: Date.now() });
    await sleep(STORE_EVENT_SETTLE_MS);
    assert.deepEqual(operations(events).map(event => event.data.type), ["playback.seek"]);
});

test("TIDAL keeps a paused session paused, at its position, once the loaded track starts", async () => {
    const { bridge, store, events } = await loadAdapter();
    const controls = () => store.getState().playbackControls;

    events.length = 0;
    await bridge.applySnapshot({
        queue: [
            { entryId: "session-current", track: repeatedTrack },
            { entryId: "session-next", track: thirdTrack },
        ],
        currentEntryId: "session-current",
        playback: playback(false, 42_000),
    });
    // Long after TIDAL finished loading (and reset the position once).
    await sleep(TRACK_LOAD_MS * 2 + STORE_EVENT_SETTLE_MS);
    assert.equal(bridge.getState().playback.isPlaying, false);
    assert.notEqual(controls().playbackState, "PLAYING");
    assert.equal(controls().latestCurrentTime, 42);
    assert.equal(store.sdk.activePlayer.position, 42, "the player itself is there, not only the store");
    assert.deepEqual(operations(events), [], "the load and corrections are not local changes");

    // When the session plays on, TIDAL starts from the player's position.
    await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: true });
    assert.equal(controls().playbackState, "PLAYING");
    assert.ok(Math.abs(controls().latestCurrentTime - 42) < 0.5, `${controls().latestCurrentTime}`);
});

test("TIDAL replaces the queue without restarting the song that keeps playing", async () => {
    const { bridge, store, events } = await loadAdapter();
    // TIDAL's sync point is a few seconds old, as usual while playing.
    store.dispatch({ type: "test/SET_PLAYBACK", payload: { latestCurrentTime: 10, latestCurrentTimeSyncTimestamp: Date.now() - 2_500 } });
    await sleep(STORE_EVENT_SETTLE_MS);
    events.length = 0;
    const [current, upcomingA, upcomingB] = bridge.getState().queue;

    // Replacing (not an in-place edit): same current song, new upcoming list.
    const outcome = await bridge.applyOperation({
        type: "queue.replace",
        entries: [current, { entryId: "session-third", track: thirdTrack }, upcomingB],
        currentEntryId: current.entryId,
    });

    assert.deepEqual([...outcome.unavailableEntryIds], []);
    assert.deepEqual([...store.loads], [], "the player did not load the song again");
    assert.deepEqual(entryIds(bridge.getState()), [current.entryId, "session-third", upcomingB.entryId]);
    assert.ok(!entryIds(bridge.getState()).includes(upcomingA.entryId));
    const { playback: now } = bridge.getState();
    const position = now.positionMs + (Date.now() - now.positionCapturedAtMs);
    assert.ok(position >= 12_500 && position < 14_000, `position kept, not moved: ${position}`);
    assert.equal(now.isPlaying, true);
    await sleep(STORE_EVENT_SETTLE_MS);
    assert.deepEqual(operations(events), [], "nothing echoed as a local change");
});

test("TIDAL applies an empty session queue by unloading the current track", async () => {
    const { bridge, store } = await loadAdapter();
    const outcome = await bridge.applySnapshot({ queue: [], currentEntryId: null, playback: playback(false) });
    assert.deepEqual([...outcome.unavailableEntryIds], []);
    assert.equal(store.getState().playbackControls.mediaProduct, null);
    assert.deepEqual(bridge.getState().queue, []);
});

test("TIDAL finds songs by ISRC in batches, without text searches", async () => {
    const { bridge, store } = await loadAdapter();
    const fourth = { title: "Fourth Track", artists: ["Artist Four"], isrc: "ISRC-FOURTH", album: "Fourth Album", durationMs: 210_000 };
    // 25 songs TIDAL doesn't have by ISRC, then the one it has.
    const unknown = Array.from({ length: 25 }, (_, index) => ({
        entryId: `session-unknown-${index}`,
        track: { title: `Ghost ${index}`, artists: ["Nobody"], isrc: `ISRC-GHOST-${index}` },
    }));

    const outcome = await bridge.applySnapshot({
        queue: [
            { entryId: "session-current", track: repeatedTrack },
            { entryId: "session-fourth", track: fourth },
            ...unknown,
        ],
        currentEntryId: "session-current",
        playback: playback(),
    });

    assert.deepEqual(entryIds(bridge.getState()), ["session-current", "session-fourth"]);
    assert.equal(bridge.getState().queue[1].track.title, "Fourth Track (feat. Guest)", "TIDAL's own metadata");
    assert.deepEqual([...outcome.unavailableEntryIds].sort(), unknown.map(entry => entry.entryId).sort());
    assert.ok(store.isrcRequests.every(batch => batch.length <= 20), "at most 20 ISRCs per request");
    assert.equal(store.isrcRequests.flat().length, 27, "every ISRC asked once");
    assert.ok(!store.searches.some(query => query.includes("Repeated") || query.includes("Fourth")), `${store.searches}`);
});

test("TIDAL falls back to text search when its open API isn't there", async () => {
    const { bridge, store } = await loadAdapter({ withOpenApi: false });
    await bridge.applySnapshot({
        queue: [{ entryId: "session-current", track: repeatedTrack }],
        currentEntryId: "session-current",
        playback: playback(),
    });
    assert.deepEqual(entryIds(bridge.getState()), ["session-current"]);
    assert.deepEqual([...store.searches], ["Repeated Track Artist Two"]);
});

test("TIDAL queue entries keep their session IDs across a bridge reload", async () => {
    const first = await loadAdapter();
    await first.bridge.applySnapshot({
        queue: [
            { entryId: "session-current", track: repeatedTrack },
            { entryId: "session-next", track: thirdTrack },
        ],
        currentEntryId: "session-current",
        playback: playback(false, 5_000),
    });
    await first.bridge.dispose();

    const second = await loadAdapter({ store: first.store });
    assert.deepEqual(entryIds(second.bridge.getState()), ["session-current", "session-next"]);
});

test("TIDAL refuses to rewrite the queue while it plays on another device", async () => {
    const { bridge, store } = await loadAdapter();
    store.getState().playQueue.type = "cloud";
    await assert.rejects(
        bridge.applySnapshot({
            queue: [{ entryId: "session-current", track: repeatedTrack }],
            currentEntryId: "session-current",
            playback: playback(),
        }),
        /another device/,
    );
});

test("TIDAL lands a playing session where it is by the time the track starts", async () => {
    const { bridge, store } = await loadAdapter();
    const before = Date.now();
    await bridge.applySnapshot({
        queue: [{ entryId: "session-current", track: repeatedTrack }],
        currentEntryId: "session-current",
        playback: playback(true, 30_000),
    });
    const controls = store.getState().playbackControls;
    assert.equal(controls.playbackState, "PLAYING");
    // 30 s plus the time spent loading, not 0 (TIDAL's own start position).
    const waited = (Date.now() - before) / 1000;
    assert.ok(controls.latestCurrentTime >= 30 + TRACK_LOAD_MS / 1000 - 0.01, `${controls.latestCurrentTime}`);
    assert.ok(controls.latestCurrentTime <= 30 + waited + 0.01, `${controls.latestCurrentTime}`);
});

test("TIDAL settles after attaching: waits for its restore, then stays paused", async () => {
    const { bridge, store, events } = await loadAdapter();
    const set = (type, payload) => store.dispatch({ type, payload });
    const playing = () => store.getState().playbackControls.playbackState === "PLAYING";

    // Like the reload (verified live): empty queue, restored queue, then
    // TIDAL resumes by itself, and once more after Attune has paused.
    set("test/SET_QUEUE", { elements: [] });
    events.length = 0;
    const timers = [
        setTimeout(() => set("test/SET_QUEUE", { elements: [{ uid: "r1", mediaItemId: "101" }, { uid: "r2", mediaItemId: "202" }] }), 100),
        setTimeout(() => set("playbackControls/PLAY"), 400),
        setTimeout(() => set("playbackControls/PLAY"), 1_900),
    ];
    const started = Date.now();
    await bridge.settle({ pause: true });
    timers.forEach(clearTimeout);

    assert.ok(Date.now() - started >= 1_900, "waited past the late resume");
    assert.equal(playing(), false);
    assert.deepEqual(store.getState().playQueue.elements.map(e => e.uid), ["r1", "r2"], "queue kept");
    await sleep(STORE_EVENT_SETTLE_MS);
    assert.deepEqual(operations(events), [], "restore, resume and pause are not the user's doing");
});

test("TIDAL turns off shuffle, repeat and autoplay for a session and puts them back", async () => {
    const { bridge, store, events } = await loadAdapter();
    const queue = () => store.getState().playQueue;
    store.dispatch({ type: "playQueue/ENABLE_SHUFFLE_MODE" });
    store.dispatch({ type: "playQueue/SET_REPEAT_MODE", payload: 2 });
    await sleep(STORE_EVENT_SETTLE_MS);
    const order = queue().elements.map(e => e.uid);
    events.length = 0;

    const previous = await bridge.prepareForSession();
    assert.deepEqual(JSON.parse(JSON.stringify(previous)), { shuffle: true, repeatMode: 2, autoPlay: true });
    assert.equal(queue().shuffleModeEnabled, false);
    assert.equal(queue().repeatMode, 0);
    assert.equal(store.getState().settings.autoPlay, false);
    assert.deepEqual(queue().elements.map(e => e.uid), order, "turning shuffle off keeps the order");

    await bridge.restoreAfterSession(previous);
    assert.equal(queue().shuffleModeEnabled, true);
    assert.equal(queue().repeatMode, 2);
    assert.equal(store.getState().settings.autoPlay, true);
    await sleep(STORE_EVENT_SETTLE_MS);
    assert.deepEqual(operations(events), []);
});

test("TIDAL starts the current song before looking up the rest of a new queue", async () => {
    // Text searches run one at a time; a long playlist used to wait for all of them.
    const store = createStore();
    const order = [];
    let release;
    const held = new Promise(resolve => { release = resolve; });
    const dispatch = store.dispatch;
    store.dispatch = action => {
        if (action?.type === "search/SEARCH_FOR" && action.payload.query.startsWith("Third Track")) {
            order.push("search:third");
            // The results arrive only once the test lets them.
            held.then(() => dispatch(action));
            return undefined;
        }
        if (action?.type === "playQueue/RESET") order.push(`reset:${action.payload.elements.length}`);
        if (action?.type === "playQueue/APPEND_ELEMENTS") order.push(`append:${action.payload.elements.length}`);
        return dispatch(action);
    };
    const { bridge } = await loadAdapter({ store });

    const loading = bridge.applySnapshot({
        queue: [
            { entryId: "session-current", track: repeatedTrack },
            { entryId: "session-third", track: thirdTrack },
            { entryId: "session-ghost", track: ghostTrack },
        ],
        currentEntryId: "session-current",
        playback: playback(),
    });
    const deadline = Date.now() + 2_000;
    while (!order.includes("reset:1") && Date.now() < deadline) await sleep(10);
    assert.ok(order.includes("reset:1"), `the current song was loaded while the rest was still being looked up: ${order}`);
    release();
    const outcome = await loading;

    assert.equal(order.at(-1), "append:1", `the rest is appended behind it: ${order}`);
    assert.deepEqual([...outcome.unavailableEntryIds], ["session-ghost"]);
    assert.deepEqual(entryIds(bridge.getState()), ["session-current", "session-third"]);
});

test("TIDAL starts a replace's new song where the replace says (protocol 2)", async () => {
    const { bridge, store } = await loadAdapter();
    await bridge.applySnapshot({
        queue: [{ entryId: "session-current", track: repeatedTrack }],
        currentEntryId: "session-current",
        playback: playback(true, 0),
    });

    await bridge.applyOperation({
        type: "queue.replace",
        entries: [{ entryId: "session-third", track: thirdTrack }],
        currentEntryId: "session-third",
        playback: { positionMs: 48_000, isPlaying: false },
    });
    await sleep(TRACK_LOAD_MS * 2 + STORE_EVENT_SETTLE_MS);

    const state = bridge.getState();
    assert.equal(state.currentEntryId, "session-third");
    assert.equal(state.playback.isPlaying, false);
    assert.equal(store.getState().playbackControls.latestCurrentTime, 48);
});

test("TIDAL searches through its open API in parallel, not one at a time", async () => {
    // Verified live: `search/SEARCH_FOR` cancels a search still in flight,
    // while the open API's /searchResults answered 20 at once in 670 ms.
    const store = createStore();
    let running = 0;
    let most = 0;
    const queries = [];
    store.openApi.endpoints.getSearchResults = {
        initiate: args => async () => {
            queries.push(args.filter.query);
            running += 1;
            most = Math.max(most, running);
            await sleep(30);
            running -= 1;
            const query = args.filter.query.toLowerCase();
            const hits = Object.entries(store.getState().content.mediaItems)
                .filter(([, item]) => query.includes(item.title.toLowerCase()));
            const track = ([id, item]) => ({
                id,
                type: "tracks",
                attributes: {
                    title: item.title,
                    isrc: item.isrc ?? null,
                    duration: `PT${Math.floor(item.duration / 60)}M${item.duration % 60}S`,
                    availability: ["STREAM"],
                },
                relationships: {
                    artists: { data: item.artists.map((_, index) => ({ id: `${id}-artist-${index}`, type: "artists" })) },
                    albums: { data: [{ id: `${id}-album`, type: "albums" }] },
                },
            });
            return {
                data: {
                    data: [{
                        id: "search-id",
                        type: "searchResults",
                        relationships: { tracks: { data: hits.map(([id]) => ({ id, type: "tracks" })) } },
                    }],
                    included: hits.flatMap(([id, item]) => [
                        track([id, item]),
                        ...item.artists.map((artist, index) => ({ id: `${id}-artist-${index}`, type: "artists", attributes: { name: artist.name } })),
                        { id: `${id}-album`, type: "albums", attributes: { title: item.album.title } },
                    ]),
                },
            };
        },
    };
    const { bridge } = await loadAdapter({ store });
    // No ISRCs, as from another provider: every song is a text search.
    const plain = ({ isrc, ...track }) => track;

    const outcome = await bridge.applySnapshot({
        queue: [
            { entryId: "session-third", track: thirdTrack },
            { entryId: "session-repeat", track: plain(repeatedTrack) },
            { entryId: "session-current", track: { title: "Current Track", artists: ["Artist One"] } },
            { entryId: "session-ghost", track: ghostTrack },
        ],
        currentEntryId: "session-third",
        playback: playback(),
    });

    assert.deepEqual([...outcome.unavailableEntryIds], ["session-ghost"]);
    assert.deepEqual(entryIds(bridge.getState()), ["session-third", "session-repeat", "session-current"]);
    assert.deepEqual([...store.searches], [], "the app's one-at-a-time search wasn't used");
    assert.ok(most >= 3, `the upcoming songs were searched at the same time (at most ${most} at once)`);
    assert.ok(queries.includes("Ghost Song Nobody"));
});
