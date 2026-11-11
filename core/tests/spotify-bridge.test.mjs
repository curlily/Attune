import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

const adapterPath = new URL("../src/adapters/spotify.js", import.meta.url);

// Tracks the mock Spotify catalogue knows, by URI.
const CATALOG = {
    "spotify:track:current": { title: "Current Song", artist: "Artist One", isrc: "ISRC0000001" },
    "spotify:track:a": { title: "Song A", artist: "Artist A", isrc: "ISRC000000A" },
    "spotify:track:b": { title: "Song B", artist: "Artist B", isrc: "ISRC000000B" },
    "spotify:track:c": { title: "Song C", artist: "Artist C", isrc: "ISRC000000C" },
    // Found only by its ISRC: the title differs from what other services call it.
    "spotify:track:feat": { title: "Duet (feat. Guest)", artist: "Artist D", isrc: "ISRC000000D" },
};
const DURATION_MS = 180_000;

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

/**
 * A model of Spotify's player as observed live (see PROVIDER_ADAPTERS.md):
 * - the state only changes with an `update` event, a moment after a call,
 * - the queue is written as a whole against a revision,
 * - the state lists at most 50 upcoming items,
 * - pause while paused / resume while playing are refused,
 * - when the queue runs out it returns to the context's track, paused.
 */
function createSpotify() {
    let uidCounter = 0;
    const listeners = { update: new Set(), queue_update: new Set() };
    const calls = [];
    const model = {
        current: null,
        contextHead: null,
        next: [],
        revision: 1n,
        isPaused: true,
        positionMs: 0,
        playbackId: 0,
        shuffle: false,
        repeat: 0,
        autoPlay: true,
        refuseNextQueueWrite: false,
    };
    let state = null;
    let internal = { queueRevision: 1n, track: null, nextTracks: [], prevTracks: [] };

    const internalTrack = (uri, provider, uid = `${provider === "queue" ? "q" : "c"}${++uidCounter}`) => ({
        contextTrack: {
            uri,
            uid,
            metadata: {
                title: CATALOG[uri].title,
                artist_name: CATALOG[uri].artist,
                album_title: "Album",
                duration: String(DURATION_MS),
            },
        },
        removed: [],
        blocked: [],
        provider,
    });
    const decorated = track => ({
        type: "track",
        uri: track.contextTrack.uri,
        uid: track.contextTrack.uid,
        name: CATALOG[track.contextTrack.uri].title,
        artists: [{ name: CATALOG[track.contextTrack.uri].artist }],
        album: { name: "Album" },
        duration: { milliseconds: DURATION_MS },
        provider: track.provider,
        metadata: {},
    });

    /** Make the model visible, as Spotify does with an update event. */
    function publish() {
        state = {
            timestamp: Date.now(),
            item: model.current ? decorated(model.current) : null,
            nextItems: model.next.slice(0, 50).map(decorated),
            previousItems: [],
            isPaused: model.current ? model.isPaused : false,
            isBuffering: false,
            positionAsOfTimestamp: model.positionMs,
            duration: model.current ? DURATION_MS : 0,
            shuffle: model.shuffle,
            repeat: model.repeat,
            playbackId: `playback-${model.playbackId}`,
        };
        internal = { queueRevision: model.revision, track: model.current, nextTracks: [...model.next], prevTracks: [] };
        for (const listener of listeners.update) listener({ data: state });
        for (const listener of listeners.queue_update) listener({ data: {} });
    }
    const later = change => new Promise(resolve => setTimeout(() => {
        change();
        publish();
        resolve();
    }, 5));

    const contextPlayer = {
        async play(request) {
            const { context, options } = request.preparePlayRequest;
            calls.push({ play: context.uri, paused: Boolean(options.initiallyPaused), seekTo: Number(options.seekTo?.value ?? 0) });
            later(() => {
                model.current = internalTrack(context.uri, "context", `ctx:${context.uri}`);
                model.contextHead = model.current;
                model.isPaused = Boolean(options.initiallyPaused);
                model.positionMs = Number(options.seekTo?.value ?? 0);
                model.playbackId += 1;
            });
            return { error: 0, reasons: "" };
        },
        async setQueue({ nextTracks, queueRevision }) {
            if (model.refuseNextQueueWrite || queueRevision !== model.revision) {
                model.refuseNextQueueWrite = false;
                calls.push({ setQueue: "refused" });
                return { error: 3, reasons: "" };
            }
            // The adapter's arrays belong to its own script context.
            calls.push({ setQueue: Array.from(nextTracks, track => track.contextTrack.uri) });
            later(() => {
                model.next = Array.from(nextTracks, track => track.contextTrack.uid
                    ? track
                    : internalTrack(track.contextTrack.uri, "queue"));
                model.revision += 1n;
            });
            return { error: 0, reasons: "" };
        },
        async stop() {
            calls.push({ stop: true });
            later(() => {
                model.current = null;
                model.next = [];
                model.revision += 1n;
            });
            return { error: 0, reasons: "" };
        },
    };

    const player = {
        _contextPlayer: contextPlayer,
        _defaultFeatureVersion: "test",
        _queue: { getInternalQueue: () => internal },
        getState: () => state,
        getQueue: () => ({ queued: [], nextUp: [] }),
        getEvents: () => ({
            addListener(type, listener) {
                listeners[type]?.add(listener);
                return () => listeners[type]?.delete(listener);
            },
        }),
        async pause() {
            if (model.isPaused) throw new Error("Command failed with code '1' and reasons 'already_paused'");
            calls.push({ pause: true });
            await later(() => { model.isPaused = true; });
        },
        async resume() {
            if (!model.isPaused) throw new Error("Command failed with code '1' and reasons 'not_paused'");
            calls.push({ resume: true });
            await later(() => { model.isPaused = false; });
        },
        async seekTo(positionMs) {
            calls.push({ seek: positionMs });
            await later(() => { model.positionMs = positionMs; });
        },
        async skipToNext() {
            calls.push({ skipToNext: true });
            await later(() => spotify.advance());
        },
        async skipToPrevious() {},
        async setShuffle(value) {
            calls.push({ setShuffle: value });
            await later(() => { model.shuffle = value; });
        },
        async setRepeat(value) {
            calls.push({ setRepeat: value });
            await later(() => { model.repeat = value; });
        },
    };

    const settings = {
        autoPlay: {
            getValue: async () => model.autoPlay,
            setValue: async value => {
                calls.push({ setAutoPlay: value });
                model.autoPlay = value;
            },
        },
    };

    const searches = [];
    /** Mimics the shape of Spotify's `searchTracks` GraphQL response. */
    async function graphql(operation, variables) {
        assert.equal(operation.name, "searchTracks");
        searches.push(variables.searchTerm);
        const isrc = variables.searchTerm.match(/^isrc:(.+)$/)?.[1];
        const items = Object.entries(CATALOG)
            .filter(([, track]) => isrc ? track.isrc === isrc : variables.searchTerm.includes(track.title))
            .map(([uri, track]) => ({
                item: {
                    data: {
                        __typename: "Track",
                        uri,
                        name: track.title,
                        artists: { items: [{ profile: { name: track.artist } }] },
                        albumOfTrack: { name: "Album" },
                        duration: { totalMilliseconds: DURATION_MS },
                        playability: { playable: true },
                    },
                },
            }));
        return { data: { searchV2: { tracksV2: { items } } } };
    }

    const spotify = {
        player,
        graphql,
        settings,
        calls,
        searches,
        model,
        publish,
        /** What the user (or Spotify itself) does, followed by an update. */
        async act(change) {
            change(model);
            publish();
        },
        /** The current track ends, or the user skips. */
        advance() {
            model.playbackId += 1;
            model.positionMs = 0;
            if (model.next.length) {
                model.current = model.next.shift();
                model.revision += 1n;
            } else {
                // Verified live: back to the context's track, paused.
                model.current = model.contextHead;
                model.isPaused = true;
            }
        },
        userQueues(uri) {
            return spotify.act(now => {
                now.next.push(internalTrack(uri, "queue"));
                now.revision += 1n;
            });
        },
        internalTrack,
    };

    // Start with one track playing from a playlist.
    model.current = internalTrack("spotify:track:current", "context");
    model.contextHead = model.current;
    model.isPaused = false;
    model.positionMs = 1_000;
    publish();
    return spotify;
}

async function loadAdapter(setup) {
    const spotify = createSpotify();
    setup?.(spotify);
    const events = [];
    let randomId = 0;

    // The adapter finds Spotify's service registry on a React fiber prop.
    const registry = {
        _map: new Map([
            [Symbol("PlayerAPI"), { instance: spotify.player }],
            [Symbol("GraphQLLoader"), { instance: spotify.graphql }],
            [Symbol("SettingsAPI"), { instance: spotify.settings }],
        ]),
    };
    const element = {
        __reactFiber$mock: { memoizedProps: { platform: { getRegistry: () => registry } }, return: null },
    };

    const context = vm.createContext({
        window: {},
        document: { querySelectorAll: () => [element] },
        location: { origin: "https://xpui.app.spotify.com" },
        fetch: async () => ({ ok: false }),
        URL,
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

    return { bridge, spotify, events, window: context.window };
}

const entry = (entryId, uri, extra = {}) => ({
    entryId,
    track: { title: CATALOG[uri].title, artists: [CATALOG[uri].artist], ...extra },
});
const ghost = entryId => ({ entryId, track: { title: "Ghost Song", artists: ["Nobody"] } });

function playback(isPlaying = true, positionMs = 0) {
    return { isPlaying, positionMs, positionCapturedAtMs: Date.now() };
}

const entryIds = state => Array.from(state.queue, item => item.entryId);
const operations = events => events.filter(event => event.type === "Operation").map(event => event.data);
// Local changes are reported after a short debounce.
const settle = () => sleep(250);

async function loadSession(bridge, uris = ["a", "b", "c"], play = playback()) {
    return bridge.applySnapshot({
        queue: uris.map(name => entry(`session-${name}`, `spotify:track:${name}`)),
        currentEntryId: `session-${uris[0]}`,
        playback: play,
    });
}

test("Spotify adapter exposes the shared bridge API", async () => {
    const { bridge, events, window } = await loadAdapter();

    assert.equal(bridge.version, 1);
    assert.equal(window.__MYJAM_BRIDGE_V1__, bridge);
    assert.ok(events.some(event => event.type === "Ready"));

    const state = bridge.getState();
    assert.equal(state.queue.length, 1);
    assert.equal(state.queue[0].track.title, "Current Song");
    assert.deepEqual([...state.queue[0].track.artists], ["Artist One"]);
    assert.equal(state.queue[0].track.durationMs, DURATION_MS);
    assert.equal(state.currentEntryId, state.queue[0].entryId);
    assert.equal(state.playback.isPlaying, true);

    const outcome = await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: false });
    assert.deepEqual([...outcome.unavailableEntryIds], []);
    assert.equal(bridge.getState().playback.isPlaying, false);

    await bridge.dispose();
    assert.equal(window.__MYJAM_BRIDGE_V1__, undefined);
});

test("Spotify reports playlist items and the whole queue, but not recommendations", async () => {
    const { bridge } = await loadAdapter(spotify => {
        const { model, internalTrack } = spotify;
        model.next = [
            internalTrack("spotify:track:a", "queue"),
            ...Array.from({ length: 60 }, () => internalTrack("spotify:track:b", "context")),
            internalTrack("spotify:track:c", "autoplay"),
        ];
        spotify.publish();
    });

    const state = bridge.getState();
    assert.equal(state.queue.length, 62, "current + 1 queued + 60 playlist items; the state alone lists 50");
    assert.equal(new Set(entryIds(state)).size, 62, "every occurrence has its own entry ID");
    assert.equal(state.queue.at(-1).track.title, "Song B");
    assert.deepEqual([...state.queue.at(-1).track.artists], ["Artist B"]);
});

test("Spotify skips upcoming tracks it cannot find and reports them", async () => {
    const { bridge, spotify } = await loadAdapter();

    const outcome = await bridge.applySnapshot({
        queue: [entry("session-a", "spotify:track:a"), ghost("session-ghost"), entry("session-b", "spotify:track:b")],
        currentEntryId: "session-a",
        playback: playback(),
    });

    assert.deepEqual([...outcome.unavailableEntryIds], ["session-ghost"]);
    assert.deepEqual(spotify.calls, [
        { play: "spotify:track:a", paused: false, seekTo: 0 },
        { setQueue: ["spotify:track:b"] },
    ]);
    assert.deepEqual(
        entryIds(bridge.getState()),
        ["session-a", "session-b"],
        "loaded entries keep their session IDs even after a skipped one",
    );
});

test("Spotify rejects an unplayable current track with a structured error and changes nothing", async () => {
    const { bridge, spotify } = await loadAdapter();

    await assert.rejects(
        bridge.applySnapshot({
            queue: [ghost("session-ghost"), entry("session-a", "spotify:track:a")],
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
    assert.deepEqual(spotify.calls, [], "Spotify must not be touched");
    assert.deepEqual(spotify.searches, ["Ghost Song Nobody"], "the rest isn't looked up once the current song is missing");
});

test("Spotify looks a track up by ISRC first", async () => {
    const { bridge, spotify } = await loadAdapter();

    // Another service calls this track "Duet"; only the ISRC and artist agree.
    const outcome = await bridge.applySnapshot({
        queue: [{ entryId: "session-duet", track: { title: "Duet", artists: ["Artist D"], isrc: "ISRC000000D" } }],
        currentEntryId: "session-duet",
        playback: playback(),
    });

    assert.deepEqual([...outcome.unavailableEntryIds], []);
    assert.deepEqual(spotify.searches, ["isrc:ISRC000000D"]);
    assert.equal(bridge.getState().queue[0].track.title, "Duet (feat. Guest)");
});

test("Spotify matches titles that differ only in a featured artist", async () => {
    const { bridge } = await loadAdapter();

    const outcome = await bridge.applySnapshot({
        queue: [{ entryId: "session-duet", track: { title: "Duet (feat. Guest)", artists: ["Artist D"] } }],
        currentEntryId: "session-duet",
        playback: playback(),
    });
    assert.deepEqual([...outcome.unavailableEntryIds], []);
    assert.deepEqual(entryIds(bridge.getState()), ["session-duet"]);
});

test("Joining a paused session loads the track paused at the session's position", async () => {
    const { bridge, spotify, events } = await loadAdapter();

    await loadSession(bridge, ["a", "b"], playback(false, 42_000));

    assert.deepEqual(spotify.calls[0], { play: "spotify:track:a", paused: true, seekTo: 42_000 });
    assert.ok(!spotify.calls.some(call => call.resume || call.pause || call.seek), "no correcting afterwards");
    const state = bridge.getState();
    assert.equal(state.playback.isPlaying, false);
    assert.equal(state.playback.positionMs, 42_000);
    await settle();
    assert.deepEqual(operations(events), []);
});

test("Queue edits keep the current track playing", async () => {
    const { bridge, spotify } = await loadAdapter();
    await loadSession(bridge);
    spotify.calls.length = 0;

    await bridge.applyOperation({ type: "queue.insert", index: 1, entry: entry("session-new", "spotify:track:current") });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-new", "session-b", "session-c"]);

    await bridge.applyOperation({ type: "queue.move", entryId: "session-c", toIndex: 1 });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-c", "session-new", "session-b"]);

    await bridge.applyOperation({ type: "queue.remove", entryId: "session-new" });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-c", "session-b"]);

    await bridge.applyOperation({
        type: "queue.replace",
        currentEntryId: "session-a",
        entries: [entry("session-a", "spotify:track:a"), entry("session-b", "spotify:track:b")],
    });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-b"]);

    assert.ok(spotify.calls.every(call => call.setQueue), "only queue writes: the track was never restarted");
    assert.deepEqual(spotify.searches.filter(term => term.includes("Song B")).length, 1, "queued tracks aren't searched again");
});

test("A refused queue write is retried with the new revision", async () => {
    const { bridge, spotify } = await loadAdapter();
    await loadSession(bridge, ["a", "b"]);
    spotify.calls.length = 0;
    spotify.model.refuseNextQueueWrite = true;

    await bridge.applyOperation({ type: "queue.remove", entryId: "session-b" });

    assert.deepEqual(spotify.calls, [{ setQueue: "refused" }, { setQueue: [] }]);
    assert.deepEqual(entryIds(bridge.getState()), ["session-a"]);
});

test("Advancing skips to the next track; an advance Spotify already made is left alone", async () => {
    const { bridge, spotify } = await loadAdapter();
    await loadSession(bridge);
    spotify.calls.length = 0;

    await bridge.applyOperation({ type: "playback.advance", targetEntryId: "session-b", positionMs: 0, isPlaying: true });
    assert.deepEqual(spotify.calls, [{ skipToNext: true }]);
    assert.deepEqual(entryIds(bridge.getState()), ["session-b", "session-c"]);

    // Every client reports the same song end.
    spotify.calls.length = 0;
    await bridge.applyOperation({ type: "playback.advance", targetEntryId: "session-b", positionMs: 200, isPlaying: true });
    assert.deepEqual(spotify.calls, []);
});

test("An empty session queue unloads Spotify's track", async () => {
    const { bridge, spotify } = await loadAdapter();

    await bridge.applySnapshot({ queue: [], currentEntryId: null, playback: playback(false) });

    assert.deepEqual(spotify.calls, [{ stop: true }]);
    const state = bridge.getState();
    assert.deepEqual([...state.queue], []);
    assert.equal(state.currentEntryId, null);
    assert.equal(state.playback.isPlaying, false);
});

test("Spotify publishes local changes but not the effects of a remote apply", async () => {
    const { bridge, spotify, events } = await loadAdapter();

    await loadSession(bridge);
    await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: false });
    await bridge.applyOperation({ type: "playback.seek", entryId: "session-a", positionMs: 60_000 });
    await settle();
    assert.deepEqual(operations(events), []);

    // Positive control: the user resumes and seeks in Spotify.
    await sleep(300);
    await spotify.act(model => { model.isPaused = false; });
    await settle();
    await spotify.act(model => { model.positionMs = 5_000; });
    await settle();
    assert.deepEqual(operations(events), [
        { type: "playback.setPlaying", isPlaying: true },
        { type: "playback.seek", entryId: "session-a", positionMs: 5_000 },
    ]);
});

test("The user's queue edits are published as small operations", async () => {
    const { bridge, spotify, events } = await loadAdapter();
    await loadSession(bridge);
    await sleep(400);

    await spotify.userQueues("spotify:track:current");
    await settle();
    const [insert] = operations(events);
    assert.equal(insert.type, "queue.insert");
    assert.equal(insert.index, 3);
    assert.equal(insert.entry.track.title, "Current Song");

    events.length = 0;
    await spotify.act(model => {
        model.next.splice(0, 1);
        model.revision += 1n;
    });
    await settle();
    assert.deepEqual(operations(events), [{ type: "queue.remove", entryId: "session-b" }]);

    events.length = 0;
    await spotify.act(model => {
        model.next.reverse();
        model.revision += 1n;
    });
    await settle();
    // Swapping two entries is one move; either entry describes it.
    assert.deepEqual(operations(events), [{ type: "queue.move", entryId: "session-c", toIndex: 2 }]);
});

test("A song end is an advance; the end of the queue is only a pause", async () => {
    const { bridge, spotify, events } = await loadAdapter();
    await loadSession(bridge, ["a", "b"]);
    await sleep(400);

    await spotify.act(() => spotify.advance());
    await settle();
    assert.deepEqual(operations(events), [
        { type: "playback.advance", targetEntryId: "session-b", positionMs: 0, isPlaying: true },
    ]);

    // The last track plays to its end: Spotify returns to the track the
    // context started with, paused. That isn't the user choosing a song.
    events.length = 0;
    await spotify.act(model => { model.positionMs = DURATION_MS - 1_000; });
    await settle();
    events.length = 0;
    await spotify.act(() => spotify.advance());
    await settle();
    assert.deepEqual(operations(events), [{ type: "playback.setPlaying", isPlaying: false }]);
    assert.deepEqual(entryIds(bridge.getState()), ["session-b"], "the finished entry stays current");

    // Resuming there plays that other track: now it is the user's choice.
    events.length = 0;
    await spotify.act(model => { model.isPaused = false; });
    await settle();
    const [replace] = operations(events);
    assert.equal(replace.type, "queue.replace");
    assert.equal(replace.entries[0].track.title, "Song A");
    assert.notEqual(replace.currentEntryId, "session-a", "a new occurrence, not the entry that already played");
});

test("Session settings are switched off and put back", async () => {
    const { bridge, spotify, events } = await loadAdapter(now => {
        now.model.shuffle = true;
        now.model.repeat = 1;
        now.publish();
    });

    const previous = await bridge.prepareForSession();
    assert.deepEqual({ ...previous }, { shuffle: true, repeat: 1, autoPlay: true });
    assert.equal(spotify.model.shuffle, false);
    assert.equal(spotify.model.repeat, 0);
    assert.equal(spotify.model.autoPlay, false);

    await bridge.restoreAfterSession(previous);
    assert.equal(spotify.model.shuffle, true);
    assert.equal(spotify.model.repeat, 1);
    assert.equal(spotify.model.autoPlay, true);

    await settle();
    assert.deepEqual(operations(events), []);
});

test("Settling after attaching pauses Spotify", async () => {
    const { bridge, spotify, events } = await loadAdapter();

    await bridge.settle({ pause: true });

    assert.equal(spotify.model.isPaused, true);
    await settle();
    assert.deepEqual(operations(events), []);
});
