import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

const adapterPath = new URL("../src/adapters/tidal.js", import.meta.url);
const STORE_EVENT_SETTLE_MS = 250; // > the adapter's 180ms debounce

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

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
        playQueue: {
            type: "local",
            currentIndex: 1,
            elements: [
                { uid: "history", mediaItemId: "999" },
                { uid: "current", mediaItemId: "101" },
                { uid: "upcoming-a", mediaItemId: "202" },
                { uid: "upcoming-b", mediaItemId: "202" },
            ],
        },
        playbackControls: {
            mediaProduct: { productId: "101" },
            playbackState: "PLAYING",
            latestCurrentTime: 12.5,
            latestCurrentTimeSyncTimestamp: 123,
        },
    };

    const listeners = new Set();
    let generatedUid = 0;
    let pendingSearch = null;
    const searches = [];

    function notify() {
        for (const listener of [...listeners]) listener();
    }

    function update(change) {
        state = structuredClone(state);
        change(state);
        notify();
    }

    return {
        searches,
        getState: () => state,
        subscribe(listener) {
            listeners.add(listener);
            return () => listeners.delete(listener);
        },
        dispatch(action) {
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
                state.playbackControls.playbackState = "PLAYING";
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
            } else if (type === "test/SET_PLAYBACK") {
                Object.assign(state.playbackControls, payload);
            } else if (type === "playbackControls/SEEK") {
                state.playbackControls.latestCurrentTime = payload;
            } else if (type === "playbackControls/PAUSE") {
                state.playbackControls.playbackState = "PAUSED";
            } else if (type === "playbackControls/PLAY") {
                state.playbackControls.playbackState = "PLAYING";
            }

            notify();
        },
    };
}

async function loadAdapter() {
    const store = createStore();
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
        loadModule: async () => ({ getGlobalStore }),
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
    assert.equal(window.__MYJAM_BRIDGE_V1__, undefined);
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

    assert.ok(!dispatched.includes("playQueue/ADD_MEDIA_ITEMS_TO_QUEUE"), `queue was replaced: ${dispatched}`);
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
