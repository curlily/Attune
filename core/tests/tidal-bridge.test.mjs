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
            },
            trackLists: {},
        },
        playQueue: {
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

    function notify() {
        for (const listener of [...listeners]) listener();
    }

    return {
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
                // Every query returns one result: "Repeated Track" for queries
                // that mention it, otherwise "Current Track". Any other title
                // therefore has no matching recording.
                const queryKey = `search/tracks/${encodeURIComponent(payload.query)}`;
                const isRepeatedTrack = payload.query.toLowerCase().includes("repeated track");
                const id = isRepeatedTrack ? "202" : "101";
                state.content.trackLists[queryKey] = {
                    sorted: { defaultSort: { items: [id] } },
                };
            } else if (type === "playQueue/ADD_MEDIA_ITEMS_TO_QUEUE") {
                state.playQueue = {
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

    const outcome = await bridge.applySnapshot({
        queue: [
            { entryId: "session-current", track: repeatedTrack },
            { entryId: "session-ghost", track: ghostTrack },
            { entryId: "session-copy", track: repeatedTrack },
        ],
        currentEntryId: "session-current",
        playback: playback(),
    });

    assert.deepEqual([...outcome.unavailableEntryIds], ["session-ghost"]);
    assert.deepEqual(
        entryIds(bridge.getState()),
        ["session-current", "session-copy"],
        "loaded entries keep their session IDs even after a skipped one",
    );
});

test("TIDAL rejects an unplayable current track with a structured error and changes nothing", async () => {
    const { bridge } = await loadAdapter();
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
            assert.deepEqual([...error.details.entryIds], ["session-ghost", "session-wrong-isrc"]);
            assert.match(error.message, /No matching TIDAL recording found for "Ghost Song"/);
            return true;
        },
    );
    assert.deepEqual(entryIds(bridge.getState()), before, "TIDAL queue must be untouched");
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
