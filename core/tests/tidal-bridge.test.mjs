import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

const adapterPath = new URL("../src/adapters/tidal.js", import.meta.url);

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

            if (type === "search/SEARCH_FOR") {
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

test("TIDAL adapter exposes the shared API and normalizes queue history/duplicates", async () => {
    const store = createStore();
    const events = [];
    let observerDisconnected = false;
    let randomId = 0;

    // The renderer script dynamically imports TIDAL chunks. Replace only that
    // import site so this test can supply a deterministic mock store accessor.
    const source = (await readFile(adapterPath, "utf8"))
        .replace("await import(url)", "await loadModule(url)");
    assert.notEqual(source, await readFile(adapterPath, "utf8"), "test import hook was not applied");

    function getGlobalStore() {
        if (!store) throw new Error("No global store set");
        return store;
    }

    const context = vm.createContext({
        window: {},
        document: { querySelectorAll: () => [] },
        performance: {
            now: () => Date.now(),
            getEntriesByType: () => [{ name: "mock://tidal/renderer.js" }],
        },
        PerformanceObserver: class {
            observe() {}
            disconnect() { observerDisconnected = true; }
        },
        crypto: { randomUUID: () => `generated-${++randomId}` },
        loadModule: async () => ({ getGlobalStore }),
        setTimeout,
        clearTimeout,
        console: { info() {}, log() {}, warn() {}, error() {} },
    });
    context.window.__jamEvent = payload => events.push(JSON.parse(payload));

    const script = new vm.Script(source, { filename: fileURLToPath(adapterPath) });
    const bridge = await script.runInContext(context);

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

    const repeatedTrack = {
        title: "Repeated Track",
        artists: ["Artist Two"],
        isrc: "ISRC-REPEAT",
        album: "Second Album",
        durationMs: 200_000,
    };

    // A contradictory ISRC must fail before the current queue is replaced.
    await assert.rejects(
        bridge.applySnapshot({
            queue: [{
                entryId: "wrong-recording",
                track: { ...repeatedTrack, isrc: "ISRC-DIFFERENT" },
            }],
            currentEntryId: "wrong-recording",
            playback: { isPlaying: true, positionMs: 0, positionCapturedAtMs: Date.now() },
        }),
        /No matching TIDAL recording found/,
    );
    assert.equal(bridge.getState().queue[0].track.title, "Current Track");

    await bridge.applySnapshot({
        queue: [
            { entryId: "session-current", track: repeatedTrack },
            { entryId: "session-copy", track: repeatedTrack },
        ],
        currentEntryId: "session-current",
        playback: {
            isPlaying: false,
            positionMs: 5_000,
            positionCapturedAtMs: Date.now(),
        },
    });

    const appliedState = bridge.getState();
    assert.deepEqual(Array.from(appliedState.queue, entry => entry.entryId), [
        "session-current",
        "session-copy",
    ]);
    assert.equal(appliedState.playback.positionMs, 5_000);
    assert.equal(appliedState.playback.isPlaying, false);

    await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: true });
    assert.equal(bridge.getState().playback.isPlaying, true);

    await bridge.dispose();
    assert.equal(bridge.ready, false);
    assert.equal(observerDisconnected, true);
    assert.equal(context.window.__MYJAM_BRIDGE_V1__, undefined);
    assert.equal(context.window.bridge, undefined);
});
