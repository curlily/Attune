import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

const adapterPath = new URL("../src/adapters/apple_music.js", import.meta.url);

// Songs the mock catalog knows, by song ID.
const CATALOG = {
    cur: { name: "Current Song", artistName: "Artist One", artists: 1, albumName: "First Album", durationInMillis: 180_000, isrc: "CUR000000001" },
    a: { name: "Song A", artistName: "Artist A", artists: 1, albumName: "Album A", durationInMillis: 180_000, isrc: "AAA000000001" },
    b: { name: "Song B", artistName: "Artist B & Guest", artists: 2, albumName: "Album B", durationInMillis: 245_000, isrc: "BBB000000001" },
    // The same recording on a compilation: same ISRC, other album.
    bComp: { name: "Song B", artistName: "Artist B & Guest", artists: 2, albumName: "Hits", durationInMillis: 245_000, isrc: "BBB000000001" },
    c: { name: "Song C", artistName: "Artist C", artists: 1, albumName: "Album C", durationInMillis: 180_000, isrc: "CCC000000001" },
    duo: { name: "Duo Song", artistName: "Simon & Garfunkel", artists: 1, albumName: "Duo", durationInMillis: 200_000, isrc: "DUO000000001" },
    hotel: { name: "Hotel California", artistName: "Eagles", artists: 1, albumName: "Hotel California", durationInMillis: 391_000, isrc: "USEE11300195" },
    // Found by search, but MusicKit can't queue it.
    broken: { name: "Broken Song", artistName: "Artist X", artists: 1, albumName: "X", durationInMillis: 100_000, isrc: "XXX000000001", unqueueable: true },
};
const STATE = { NONE: 0, LOADING: 1, PLAYING: 2, PAUSED: 3, STOPPED: 4, ENDED: 5, SEEKING: 6 };
// Verified live: a seek lands 250–340 ms after it is asked for.
const SEEK_DEBOUNCE_MS = 300;

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

function catalogSong(id) {
    const song = CATALOG[id];
    return {
        id,
        type: "songs",
        attributes: {
            name: song.name,
            artistName: song.artistName,
            albumName: song.albumName,
            durationInMillis: song.durationInMillis,
            isrc: song.isrc,
            artwork: { url: `https://img.example/${id}/{w}x{h}bb.jpg` },
            playParams: { id, kind: "song" },
        },
        relationships: { artists: { data: Array.from({ length: song.artists }, (_, index) => ({ id: `${id}-${index}` })) } },
    };
}

class MKError extends Error {}

/**
 * A model of MusicKit JS v3 in music.apple.com as observed live (see
 * PROVIDER_ADAPTERS.md):
 * - `setQueue` puts a new queue object in place, with new item objects;
 *   one ID it can't find fails the whole call and changes nothing,
 * - `setQueue` without playing loads nothing (`position` -1), and a seek
 *   then does nothing,
 * - `changeToMediaAtIndex` starts an item from 0, playing,
 * - seeks are debounced by 250 ms and a new one cancels the one waiting,
 * - muting the <audio> sets MusicKit's volume to 0, and unmuting doesn't
 *   bring it back,
 * - with `continuous` off (signed out) the player stops after each song.
 */
function createMusicKit() {
    const listeners = new Map();
    const calls = [];
    const requests = [];
    // What could be heard: [positionMs, muted] samples while playing.
    const heard = [];

    function emit(type, data) {
        for (const listener of [...(listeners.get(type) ?? [])]) listener(data);
    }

    function makeItem(id) {
        const data = catalogSong(id);
        return {
            ...data,
            title: data.attributes.name,
            artistName: data.attributes.artistName,
            albumName: data.attributes.albumName,
            isrc: data.attributes.isrc,
        };
    }

    class Queue {
        constructor(ids = []) {
            this.items = ids.map(makeItem);
            this._queueItems = this.items.map(item => ({ isAutoplay: false, item }));
            this.position = -1;
        }
        get length() {
            return this.items.length;
        }
        remove(index) {
            if (!Number.isInteger(index) || index < 0 || index >= this.items.length) {
                throw new MKError("[mk-007] INVALID_ARGUMENTS");
            }
            calls.push(`remove:${this.items[index].id}`);
            this.items.splice(index, 1);
            this._queueItems.splice(index, 1);
            if (index < this.position) this.position -= 1;
            emit("queueItemsDidChange", this.items);
        }
        insert(index, ids) {
            const items = ids.map(makeItem);
            this.items.splice(index, 0, ...items);
            this._queueItems.splice(index, 0, ...items.map(item => ({ isAutoplay: false, item })));
            if (index <= this.position) this.position += items.length;
            emit("queueItemsDidChange", this.items);
        }
    }

    const player = { positionMs: 0, since: Date.now(), state: STATE.NONE, item: null };
    const position = () => player.positionMs + (player.state === STATE.PLAYING ? Date.now() - player.since : 0);

    const audio = {
        id: "apple-music-player",
        _muted: false,
        get currentTime() {
            return position() / 1000;
        },
        get muted() {
            return this._muted;
        },
        set muted(value) {
            this._muted = Boolean(value);
            // The page follows the element's mute into the volume.
            if (value) music._volume = 0;
        },
    };

    const mediaListeners = new Map();
    const media = {
        addEventListener(type, listener) {
            if (!mediaListeners.has(type)) mediaListeners.set(type, new Set());
            mediaListeners.get(type).add(listener);
        },
        removeEventListener(type, listener) {
            mediaListeners.get(type)?.delete(listener);
        },
        fire(type) {
            for (const listener of [...(mediaListeners.get(type) ?? [])]) listener({ type });
        },
    };

    function setState(state) {
        if (player.state === STATE.PLAYING) heard.push([position(), audio.muted]);
        player.positionMs = position();
        player.since = Date.now();
        const oldState = player.state;
        player.state = state;
        if (state === STATE.PLAYING) heard.push([player.positionMs, audio.muted]);
        if (oldState !== state) emit("playbackStateDidChange", { oldState, state });
    }

    function load(index, startMs = 0) {
        const queue = music.queue;
        queue.position = index;
        player.item = queue.items[index];
        player.positionMs = startMs;
        player.since = Date.now();
        emit("queuePositionDidChange", { position: index });
        emit("nowPlayingItemDidChange", { item: player.item });
        setState(STATE.LOADING);
        return new Promise(resolve => setTimeout(() => {
            setState(STATE.PLAYING);
            media.fire("playing");
            resolve();
        }, 10));
    }

    let pendingSeek = null;
    function requireKnown(ids) {
        const unknown = ids.filter(id => !CATALOG[id] || CATALOG[id].unqueueable);
        if (unknown.length) throw new MKError(`[mk-007] NOT_FOUND; One or more items could not be resolved: ${unknown.join(",")}`);
    }

    const music = {
        storefrontId: "us",
        _volume: 1,
        get volume() {
            return this._volume;
        },
        set volume(value) {
            this._volume = value;
        },
        autoplayEnabled: true,
        repeatMode: 0,
        shuffleMode: 0,
        continuous: false,
        queue: new Queue(),
        get playbackState() {
            return player.state;
        },
        get nowPlayingItem() {
            return player.item ?? undefined;
        },
        get currentPlaybackTime() {
            return Math.floor(position() / 1000);
        },
        addEventListener(type, listener) {
            if (!listeners.has(type)) listeners.set(type, new Set());
            listeners.get(type).add(listener);
        },
        removeEventListener(type, listener) {
            listeners.get(type)?.delete(listener);
        },
        async setQueue({ songs, song, startPlaying = false, startTime = 0 }) {
            const ids = songs ?? [song];
            calls.push(`setQueue:${ids.join(",")}`);
            requireKnown(ids);
            if (player.item) {
                player.item = null;
                emit("nowPlayingItemDidChange", { item: undefined });
                setState(STATE.STOPPED);
            }
            music.queue = new Queue(ids);
            emit("queueItemsDidChange", music.queue.items);
            if (startPlaying) await load(0, startTime * 1000);
        },
        async changeToMediaAtIndex(index) {
            calls.push(`change:${music.queue.items[index]?.id}`);
            await load(index, 0);
        },
        async playAt(index, { song }) {
            calls.push(`playAt:${index}:${song}`);
            requireKnown([song]);
            music.queue.insert(index, [song]);
        },
        async playNext({ song }) {
            music.queue.insert(Math.max(0, music.queue.position) + 1, [song]);
        },
        async playLater({ songs, song }) {
            const ids = songs ?? [song];
            calls.push(`playLater:${ids.join(",")}`);
            requireKnown(ids);
            music.queue.insert(music.queue.items.length, ids);
        },
        async play() {
            calls.push("play");
            if (player.item) setState(STATE.PLAYING);
        },
        async pause() {
            calls.push("pause");
            if (player.item) setState(STATE.PAUSED);
        },
        async stop() {
            calls.push("stop");
            player.item = null;
            emit("nowPlayingItemDidChange", { item: undefined });
            setState(STATE.STOPPED);
        },
        async clearQueue() {
            music.queue = new Queue();
            emit("queueItemsDidChange", []);
        },
        seekToTime(seconds) {
            calls.push(`seek:${Math.round(seconds)}`);
            if (!player.item) return Promise.resolve();
            // Debounced: a new seek cancels the one waiting.
            if (pendingSeek) {
                clearTimeout(pendingSeek.timer);
                pendingSeek.resolve();
            }
            return new Promise(resolve => {
                const timer = setTimeout(() => {
                    pendingSeek = null;
                    player.positionMs = seconds * 1000;
                    player.since = Date.now();
                    media.fire("seeked");
                    resolve();
                }, SEEK_DEBOUNCE_MS);
                pendingSeek = { timer, resolve };
            });
        },
        api: {
            async music(path, params) {
                if (path.endsWith("/songs")) {
                    requests.push(`isrc:${params["filter[isrc]"]}`);
                    const data = Object.keys(CATALOG).filter(id => CATALOG[id].isrc === params["filter[isrc]"]).map(catalogSong);
                    return { data: { data } };
                }
                requests.push(`search:${params.term}`);
                const data = Object.keys(CATALOG)
                    .filter(id => params.term.toLowerCase().includes(CATALOG[id].name.toLowerCase()))
                    .map(catalogSong);
                return { data: { results: { songs: { data } } } };
            },
        },
    };

    const page = {
        music,
        audio,
        media,
        calls,
        requests,
        heard,
        player,
        position,
        /** What the user does in the page: play a new list. */
        userPlays: (...ids) => music.setQueue({ songs: ids, startPlaying: true }),
        userQueuesNext: id => music.playNext({ song: id }),
        userRemoves: index => music.queue.remove(index),
        async userSeeks(positionMs) {
            await music.seekToTime(positionMs / 1000);
        },
        /** The user presses shuffle: what follows the current song is reordered. */
        userShuffles() {
            const start = music.queue.position + 1;
            const upcoming = music.queue.items.slice(start).reverse();
            music.queue.items.splice(start, upcoming.length, ...upcoming);
            music.shuffleMode = 1;
            emit("shuffleModeDidChange", {});
            emit("queueItemsDidChange", music.queue.items);
        },
        /** The user presses repeat (MusicKit: 1 one, 2 all). */
        userRepeats(mode = 2) {
            music.repeatMode = mode;
            emit("repeatModeDidChange", { mode });
        },
        /** The current song plays to its end. */
        songEnds() {
            setState(STATE.ENDED);
            const next = music.queue.position + 1;
            if (music.continuous && next < music.queue.items.length) {
                load(next, 0);
            } else {
                player.item = null;
                emit("nowPlayingItemDidChange", { item: undefined });
                setState(STATE.STOPPED);
            }
        },
    };
    return page;
}

// Verified live: while a Chromium page is hidden and silent, its timers
// run once a second.
const backgroundSetTimeout = (callback, ms, ...args) => setTimeout(callback, Math.max(1_000, ms ?? 0), ...args);

async function loadAdapter(setup, { pageSetTimeout = setTimeout } = {}) {
    const page = createMusicKit();
    // Start as a page that already played one song and is on its second.
    await page.music.setQueue({ songs: ["a", "cur"], startPlaying: true });
    await page.music.changeToMediaAtIndex(1);
    page.player.positionMs = 1_000;
    page.calls.length = 0;
    await setup?.(page);

    const events = [];
    let randomId = 0;
    const context = vm.createContext({
        window: {
            MusicKit: { getInstance: () => page.music },
            addEventListener: page.media.addEventListener,
            removeEventListener: page.media.removeEventListener,
        },
        document: { getElementById: id => (id === page.audio.id ? page.audio : null) },
        performance: { now: () => Date.now() },
        crypto: { randomUUID: () => `generated-${++randomId}` },
        setTimeout: pageSetTimeout,
        clearTimeout,
        setInterval,
        clearInterval,
        console: { info() {}, log() {}, warn() {}, error() {} },
    });
    context.window.__jamEvent = payload => events.push(JSON.parse(payload));

    const source = await readFile(adapterPath, "utf8");
    const script = new vm.Script(source, { filename: fileURLToPath(adapterPath) });
    const bridge = await script.runInContext(context);
    return { bridge, page, events, window: context.window };
}

const entry = (entryId, id, extra = {}) => ({
    entryId,
    track: { title: CATALOG[id].name, artists: [CATALOG[id].artistName.split(" & ")[0]], ...extra },
});
const ghost = entryId => ({ entryId, track: { title: "Ghost Song", artists: ["Nobody"] } });

function playback(isPlaying = true, positionMs = 0) {
    return { isPlaying, positionMs, positionCapturedAtMs: Date.now() };
}

const entryIds = state => Array.from(state.queue, item => item.entryId);
const operations = events => events.filter(event => event.type === "Operation").map(event => event.data);
// Local changes are reported after a debounce, seeks at the next poll.
const settle = () => sleep(800);

async function loadSession(bridge, ids = ["a", "b", "c"], play = playback()) {
    return bridge.applySnapshot({
        queue: ids.map(id => entry(`session-${id}`, id)),
        currentEntryId: `session-${ids[0]}`,
        playback: play,
    });
}

async function withAdapter(setup, run, options) {
    const loaded = await loadAdapter(typeof setup === "function" && run ? setup : undefined, options);
    try {
        await (run ?? setup)(loaded);
    } finally {
        // Stops the adapter's timers so the test process can exit.
        await loaded.bridge.dispose();
    }
}

test("Apple Music adapter exposes the shared bridge API", () => withAdapter(async ({ bridge, events, window }) => {
    assert.equal(bridge.version, 1);
    assert.equal(window.__ATTUNE_BRIDGE_V1__, bridge);
    assert.ok(events.some(event => event.type === "Ready"));

    const state = bridge.getState();
    assert.equal(state.queue.length, 1, "the song that already played is history");
    assert.deepEqual({ ...state.queue[0].track, artists: [...state.queue[0].track.artists] }, {
        title: "Current Song",
        artists: ["Artist One"],
        isrc: "CUR000000001",
        album: "First Album",
        durationMs: 180_000,
        coverUrl: "https://img.example/cur/600x600bb.jpg",
    });
    assert.equal(state.playback.isPlaying, true);

    const outcome = await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: false });
    assert.deepEqual([...outcome.unavailableEntryIds], []);
    assert.equal(bridge.getState().playback.isPlaying, false);
}));

test("A credit is split into artists only when the count matches", () => withAdapter(async ({ bridge, page }) => {
    await page.userPlays("b", "duo");
    const [song, duo] = bridge.getState().queue;
    assert.deepEqual([...song.track.artists], ["Artist B", "Guest"]);
    assert.deepEqual([...duo.track.artists], ["Simon & Garfunkel"]);
}));

test("An ISRC finds the recording without a search, on the same album", () => withAdapter(async ({ bridge, page }) => {
    await bridge.applySnapshot({
        queue: [{ entryId: "s1", track: { title: "Song B - 2016 Remaster", artists: ["Artist B"], isrc: "BBB000000001", album: "Hits" } }],
        currentEntryId: "s1",
        playback: playback(),
    });
    assert.deepEqual(page.requests, ["isrc:BBB000000001"]);
    assert.deepEqual(page.calls.filter(call => call.startsWith("setQueue")), ["setQueue:bComp"]);
}));

test("Remaster tags still match the same recording by text", () => withAdapter(async ({ bridge, page }) => {
    await bridge.applySnapshot({
        queue: [{ entryId: "s1", track: { title: "Hotel California - 2013 Remaster", artists: ["Eagles"] } }],
        currentEntryId: "s1",
        playback: playback(),
    });
    assert.equal(bridge.getState().queue[0].track.title, "Hotel California");
    assert.equal(page.music.nowPlayingItem.id, "hotel");
}));

test("Apple Music skips upcoming tracks it cannot find or queue, and reports them", () => withAdapter(async ({ bridge, page }) => {
    const outcome = await bridge.applySnapshot({
        queue: [entry("s-a", "a"), ghost("s-ghost"), entry("s-b", "b"), entry("s-broken", "broken"), entry("s-c", "c")],
        currentEntryId: "s-a",
        playback: playback(),
    });
    assert.deepEqual([...outcome.unavailableEntryIds].sort(), ["s-broken", "s-ghost"]);
    assert.deepEqual(entryIds(bridge.getState()), ["s-a", "s-b", "s-c"]);
    assert.deepEqual([...page.music.queue.items.map(item => item.id)], ["a", "b", "c"]);
}));

test("Apple Music rejects an unplayable current track and changes nothing", () => withAdapter(async ({ bridge, page }) => {
    const before = page.music.queue;
    await assert.rejects(
        bridge.applySnapshot({ queue: [ghost("s-ghost"), entry("s-a", "a")], currentEntryId: "s-ghost", playback: playback() }),
        error => error.code === "trackUnavailable" && error.details.entryIds.includes("s-ghost"),
    );
    assert.equal(page.music.queue, before);
    assert.equal(page.music.nowPlayingItem.id, "cur");
}));

test("Joining a paused session ends up paused there, silent, and at the user's volume", () => withAdapter(async ({ bridge, page, events }) => {
    page.music.volume = 0.7;
    page.heard.length = 0;
    await loadSession(bridge, ["a", "b"], playback(false, 30_000));

    const state = bridge.getState();
    assert.equal(state.currentEntryId, "session-a");
    assert.equal(state.playback.isPlaying, false);
    assert.ok(Math.abs(state.playback.positionMs - 30_000) <= 1_000, `at ${state.playback.positionMs}`);
    assert.ok(page.heard.every(([, muted]) => muted), "the start that is paused at once isn't heard");
    assert.equal(page.audio.muted, false);
    assert.equal(page.music.volume, 0.7, "muting the element set the volume to 0; it is put back");
    await settle();
    assert.deepEqual(operations(events), []);
}));

test("A held position lands although MusicKit debounces and cancels seeks", () => withAdapter(async ({ bridge, page }) => {
    await loadSession(bridge, ["a", "b"]);
    const startedAt = Date.now();
    await bridge.applyOperation({ type: "playback.advance", targetEntryId: "session-b", positionMs: 40_000, isPlaying: true });
    const tookMs = Date.now() - startedAt;
    const state = bridge.getState();
    assert.equal(state.currentEntryId, "session-b");
    assert.ok(Math.abs(state.playback.positionMs - 40_000 - tookMs) <= 1_000, `at ${state.playback.positionMs}`);
    assert.ok(tookMs < 3_000, `took ${tookMs} ms`);
}));

test("Queue edits keep the current track playing", () => withAdapter(async ({ bridge, page }) => {
    await loadSession(bridge, ["a", "b", "c"]);
    page.calls.length = 0;

    await bridge.applyOperation({ type: "queue.insert", index: 1, entry: entry("s-cur", "cur") });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "s-cur", "session-b", "session-c"]);
    await bridge.applyOperation({ type: "queue.remove", entryId: "session-b" });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "s-cur", "session-c"]);
    await bridge.applyOperation({ type: "queue.move", entryId: "session-c", toIndex: 1 });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-c", "s-cur"]);

    assert.ok(!page.calls.some(call => call.startsWith("setQueue") || call.startsWith("change")), page.calls.join(" "));
    assert.equal(page.music.nowPlayingItem.id, "a");
    assert.equal(bridge.getState().playback.isPlaying, true);
}));

test("The same song twice gets two entries", () => withAdapter(async ({ bridge }) => {
    await bridge.applySnapshot({
        queue: [entry("one", "a"), entry("two", "b"), entry("three", "a")],
        currentEntryId: "one",
        playback: playback(),
    });
    assert.deepEqual(entryIds(bridge.getState()), ["one", "two", "three"]);
}));

test("An empty session queue clears the page's queue", () => withAdapter(async ({ bridge, page }) => {
    await bridge.applySnapshot({ queue: [], currentEntryId: null, playback: playback(false) });
    assert.equal(page.music.queue.items.length, 0);
    assert.equal(bridge.getState().queue.length, 0);
    assert.equal(bridge.getState().playback.isPlaying, false);
}));

test("Apple Music publishes local changes but not the effects of a remote apply", () => withAdapter(async ({ bridge, page, events }) => {
    await loadSession(bridge, ["a", "b"]);
    await bridge.applyOperation({ type: "playback.seek", entryId: "session-a", positionMs: 20_000 });
    await settle();
    assert.deepEqual(operations(events), [], "remote changes aren't echoed");

    await page.music.pause();
    await settle();
    assert.deepEqual(operations(events).map(op => op.type), ["playback.setPlaying"]);
    events.length = 0;

    await page.music.play();
    await page.userSeeks(90_000);
    await settle();
    const ops = operations(events);
    assert.deepEqual(ops.map(op => op.type), ["playback.setPlaying", "playback.seek"]);
    assert.ok(Math.abs(ops[1].positionMs - 90_000) <= 1_000);
}));

test("The user's queue edits are published as small operations", () => withAdapter(async ({ bridge, page, events }) => {
    await loadSession(bridge, ["a", "b", "c"]);
    await settle();
    events.length = 0;

    await page.userQueuesNext("cur");
    await settle();
    const [insert] = operations(events);
    assert.equal(insert.type, "queue.insert");
    assert.equal(insert.index, 1);
    assert.equal(insert.entry.track.title, "Current Song");
    events.length = 0;

    page.userRemoves(page.music.queue.position + 2);
    await settle();
    assert.deepEqual(operations(events), [{ type: "queue.remove", entryId: "session-b" }]);
}));

test("A shuffle during a session is shared as a queue change, and keeps its order with the mode off", () => withAdapter(async ({ bridge, page, events }) => {
    await bridge.prepareForSession();
    await loadSession(bridge, ["a", "b", "c"]);
    await settle();
    events.length = 0;

    page.userShuffles();
    await settle();
    await settle();
    const ops = operations(events);
    assert.ok(ops.length > 0 && ops.every(op => op.type.startsWith("queue.")), JSON.stringify(ops));
    assert.equal(page.music.shuffleMode, 0, "MusicKit's shuffle is off again");
    const titles = Array.from(page.music.queue.items.slice(page.music.queue.position + 1), item => item.title);
    assert.deepEqual(titles, ["Song C", "Song B"], "the shuffled order stays");
}));

test("A repeat press during a session is reported, and MusicKit's own repeat goes off again", () => withAdapter(async ({ bridge, page, events }) => {
    await bridge.prepareForSession();
    await loadSession(bridge, ["a", "b"]);
    await settle();
    events.length = 0;

    page.userRepeats();
    await settle();
    assert.deepEqual(operations(events), [{ type: "playback.setRepeat", mode: "one" }]);
    assert.equal(page.music.repeatMode, 0);
}));

test("With a session's settings a song end is an advance, and they are put back", () => withAdapter(async ({ bridge, page, events }) => {
    const previous = await bridge.prepareForSession();
    assert.deepEqual({ ...previous }, { autoplay: true, repeatMode: 0, shuffleMode: 0, continuous: false });
    assert.equal(page.music.continuous, true, "signed out, MusicKit stops after every song");
    assert.equal(page.music.autoplayEnabled, false);

    await loadSession(bridge, ["a", "b"]);
    await settle();
    events.length = 0;
    page.songEnds();
    await settle();
    const [advance] = operations(events);
    assert.equal(advance.type, "playback.advance");
    assert.equal(advance.targetEntryId, "session-b");
    events.length = 0;

    // The end of the queue is only a pause.
    page.songEnds();
    await settle();
    assert.deepEqual(operations(events).map(op => op.type), ["playback.setPlaying"]);

    await bridge.restoreAfterSession(previous);
    assert.equal(page.music.continuous, false);
    assert.equal(page.music.autoplayEnabled, true);
}));

test("The user starting another list replaces the session queue", () => withAdapter(async ({ bridge, page, events }) => {
    await loadSession(bridge, ["a", "b"]);
    await settle();
    events.length = 0;
    await page.userPlays("c", "hotel");
    await settle();
    const [replace] = operations(events);
    assert.equal(replace.type, "queue.replace");
    assert.deepEqual(replace.entries.map(item => item.track.title), ["Song C", "Hotel California"]);
}));

test("A new queue starts its current song before the rest is looked up", () => withAdapter(async ({ bridge, page }) => {
    const order = [];
    const search = page.music.api.music;
    page.music.api.music = async (path, params) => {
        order.push(`lookup:${params.term ?? params["filter[isrc]"]}`);
        if (params.term?.includes("Song C")) {
            await sleep(300);
            order.push("found:Song C");
        }
        return search(path, params);
    };
    const setQueue = page.music.setQueue;
    page.music.setQueue = async options => {
        order.push(`start:${options.songs.join(",")}`);
        return setQueue(options);
    };
    await loadSession(bridge, ["a", "b", "c"]);
    const start = order.indexOf("start:a");
    assert.ok(start >= 0 && start < order.indexOf("found:Song C"), order.join(" "));
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-b", "session-c"]);
}));

test("Holds and waits follow the page, also while its timers run once a second", () => withAdapter(undefined, async ({ bridge }) => {
    const startedAt = Date.now();
    await loadSession(bridge, ["a", "b"], playback(false, 20_000));
    const state = bridge.getState();
    assert.equal(state.playback.isPlaying, false);
    assert.ok(Math.abs(state.playback.positionMs - 20_000) <= 1_000, `at ${state.playback.positionMs}`);
    assert.ok(Date.now() - startedAt < 8_000, `took ${Date.now() - startedAt} ms`);
}, { pageSetTimeout: backgroundSetTimeout }));
