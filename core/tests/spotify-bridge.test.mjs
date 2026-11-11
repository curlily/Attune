import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

const adapterPath = new URL("../src/adapters/spotify.js", import.meta.url);

// Tracks the mock Spotify catalogue can find, by URI.
const CATALOG = {
    "spotify:track:current": { title: "Current Song", artist: "Artist One" },
    "spotify:track:a": { title: "Song A", artist: "Artist A" },
    "spotify:track:b": { title: "Song B", artist: "Artist B" },
};

function catalogItem(uri, uid) {
    const { title, artist } = CATALOG[uri];
    return { uri, uid, provider: "queue", name: title, artists: [{ name: artist }], duration_ms: 180_000 };
}

/** Minimal PlayerAPI covering the calls made by the session methods. */
function createPlayer() {
    let generatedUid = 0;
    const listeners = { action: new Set(), update: new Set() };
    const calls = [];

    const player = {
        calls,
        state: {
            item: { ...catalogItem("spotify:track:current", "uid-current"), provider: "context" },
            nextItems: [],
            isPaused: false,
            isBuffering: false,
            positionAsOfTimestamp: 1_000,
            timestamp: Date.now(),
        },
        getState() {
            return player.state;
        },
        getEvents() {
            return {
                addListener(type, listener) {
                    listeners[type].add(listener);
                    return () => listeners[type].delete(listener);
                },
            };
        },
        /** Simulate Spotify pushing an update event with a changed state. */
        emitUpdate(change) {
            player.state = { ...player.state, ...change, timestamp: Date.now() };
            for (const listener of listeners.update) listener({ data: player.state });
        },
        async clearQueue() {
            calls.push("clearQueue");
            player.state = { ...player.state, nextItems: [] };
        },
        async play({ track_uri }) {
            calls.push(`play:${track_uri}`);
            player.state = {
                ...player.state,
                item: catalogItem(track_uri, `uid-${++generatedUid}`),
                isPaused: false,
                positionAsOfTimestamp: 0,
            };
        },
        async addToQueue(items) {
            calls.push(`addToQueue:${items.length}`);
            player.state = {
                ...player.state,
                nextItems: [
                    ...player.state.nextItems,
                    ...items.map(item => catalogItem(item.uri, `uid-${++generatedUid}`)),
                ],
            };
        },
        async getQueue() {
            return { queued: player.state.nextItems };
        },
        async pause() {
            calls.push("pause");
            player.state = { ...player.state, isPaused: true };
        },
        async resume() {
            calls.push("resume");
            player.state = { ...player.state, isPaused: false };
        },
        async seekTo(positionMs) {
            calls.push(`seek:${positionMs}`);
            player.state = { ...player.state, positionAsOfTimestamp: positionMs };
        },
    };
    return player;
}

/** Mimics the shape of Spotify's `searchTopResultsList` GraphQL response. */
async function search(query) {
    const itemsV2 = Object.entries(CATALOG)
        .filter(([, track]) => query.includes(track.title))
        .map(([uri, track]) => ({
            item: {
                data: {
                    __typename: "Track",
                    id: uri.replace("spotify:track:", ""),
                    name: track.title,
                    artists: { items: [{ profile: { name: track.artist } }] },
                },
            },
        }));
    return { data: { searchV2: { topResultsV2: { itemsV2 } } } };
}

async function loadAdapter() {
    const player = createPlayer();
    const events = [];
    let randomId = 0;

    // The adapter finds PlayerAPI through React fiber props when Spotify's
    // service registry is unavailable; expose the mock that way.
    const element = { __reactFiber$mock: { memoizedProps: { playerAPI: player }, return: null } };

    const context = vm.createContext({
        window: {},
        document: { querySelectorAll: () => [element] },
        performance: { now: () => Date.now() },
        crypto: { randomUUID: () => `generated-${++randomId}` },
        setTimeout,
        clearTimeout,
        console: { info() {}, log() {}, warn() {}, error() {} },
    });
    context.window.__jamEvent = payload => events.push(JSON.parse(payload));

    const source = await readFile(adapterPath, "utf8");
    const script = new vm.Script(source, { filename: fileURLToPath(adapterPath) });
    const bridge = await script.runInContext(context);

    // Spotify's search operation is discovered from webpack modules at
    // runtime; substitute the search function itself. Matching logic
    // (searchTrack) and lookup-error handling still run for real.
    bridge.getSearchFunction = async () => search;

    return { bridge, player, events, window: context.window };
}

const track = uri => ({
    title: CATALOG[uri]?.title ?? "Ghost Song",
    artists: [CATALOG[uri]?.artist ?? "Nobody"],
});
const ghostTrack = { title: "Ghost Song", artists: ["Nobody"] };

function playback(isPlaying = true, positionMs = 0) {
    return { isPlaying, positionMs, positionCapturedAtMs: Date.now() };
}

const entryIds = state => Array.from(state.queue, entry => entry.entryId);
const operations = events => events.filter(event => event.type === "Operation");

test("Spotify adapter exposes the shared bridge API", async () => {
    const { bridge, events, window } = await loadAdapter();

    assert.equal(bridge.version, 1);
    assert.equal(window.__MYJAM_BRIDGE_V1__, bridge);
    assert.ok(events.some(event => event.type === "Ready"));

    const state = bridge.getState();
    assert.equal(state.queue.length, 1);
    assert.equal(state.queue[0].track.title, "Current Song");
    assert.equal(state.currentEntryId, state.queue[0].entryId);
    assert.equal(state.playback.isPlaying, true);

    const outcome = await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: false });
    assert.deepEqual([...outcome.unavailableEntryIds], []);
    assert.equal(bridge.getState().playback.isPlaying, false);

    bridge.dispose();
    assert.equal(window.__MYJAM_BRIDGE_V1__, undefined);
});

test("Spotify skips upcoming tracks it cannot find and reports them", async () => {
    const { bridge, player } = await loadAdapter();

    const outcome = await bridge.applySnapshot({
        queue: [
            { entryId: "session-a", track: track("spotify:track:a") },
            { entryId: "session-ghost", track: ghostTrack },
            { entryId: "session-b", track: track("spotify:track:b") },
        ],
        currentEntryId: "session-a",
        playback: playback(),
    });

    assert.deepEqual([...outcome.unavailableEntryIds], ["session-ghost"]);
    assert.deepEqual(player.calls, ["clearQueue", "play:spotify:track:a", "addToQueue:1"]);
    assert.deepEqual(
        entryIds(bridge.getState()),
        ["session-a", "session-b"],
        "loaded entries keep their session IDs even after a skipped one",
    );
});

test("Spotify rejects an unplayable current track with a structured error and changes nothing", async () => {
    const { bridge, player } = await loadAdapter();

    await assert.rejects(
        bridge.applySnapshot({
            queue: [
                { entryId: "session-ghost", track: ghostTrack },
                { entryId: "session-a", track: track("spotify:track:a") },
            ],
            currentEntryId: "session-ghost",
            playback: playback(),
        }),
        error => {
            assert.equal(error.code, "trackUnavailable");
            assert.deepEqual([...error.details.entryIds], ["session-ghost"]);
            assert.match(error.message, /No matching Spotify recording found for "Ghost Song"/);
            return true;
        },
    );
    assert.deepEqual(player.calls, [], "Spotify must not be touched");
});

test("Spotify publishes local changes but not the effects of a remote apply", async () => {
    const { bridge, player, events } = await loadAdapter();
    player.emitUpdate({}); // first update only records the baseline

    // A remote apply opens the suppression window; Spotify's resulting
    // update events must not come back as session operations.
    await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: false });
    player.emitUpdate({});
    assert.deepEqual(operations(events), []);

    // Positive control: once the window has passed, a user resuming in
    // Spotify becomes a session operation.
    bridge.suppressSessionEventsUntil = 0;
    player.emitUpdate({ isPaused: false });
    assert.deepEqual(
        operations(events).map(event => event.data),
        [{ type: "playback.setPlaying", isPlaying: true }],
    );
});
