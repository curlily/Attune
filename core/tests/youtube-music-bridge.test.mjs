import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

const adapterPath = new URL("../src/adapters/youtube_music.js", import.meta.url);

// Songs the mock YouTube Music knows, by video ID.
const CATALOG = {
    cur: { title: "Current Song", artists: ["Artist One"], album: "First Album", length: "3:00" },
    a: { title: "Song A", artists: ["Artist A"], album: "Album A", length: "3:00" },
    b: { title: "Song B", artists: ["Artist B", "Guest"], album: "Album B", length: "4:05" },
    c: { title: "Song C", artists: ["Artist C"], album: "Album C", length: "3:00" },
    // A music video: its byline has no links.
    video: { title: "Video Song", artists: ["Band X", "Band Y"], album: null, length: "2:30", plain: true },
};
const PLAYER = { UNSTARTED: -1, ENDED: 0, PLAYING: 1, PAUSED: 2 };

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

function byline(song) {
    if (song.plain) return { runs: [{ text: `${song.artists.join(" & ")} • 2.1B views` }] };
    const link = pageType => ({
        browseEndpoint: { browseEndpointContextSupportedConfigs: { browseEndpointContextMusicConfig: { pageType } } },
    });
    const runs = [];
    song.artists.forEach((name, index) => {
        if (index) runs.push({ text: " & " });
        runs.push({ text: name, navigationEndpoint: link("MUSIC_PAGE_TYPE_ARTIST") });
    });
    runs.push({ text: " • " }, { text: song.album, navigationEndpoint: link("MUSIC_PAGE_TYPE_ALBUM") });
    runs.push({ text: " • " }, { text: "2019" });
    return { runs };
}

/** A queue item as `music/get_queue` returns it (no queue number yet). */
function queueItem(videoId) {
    const song = CATALOG[videoId];
    return {
        playlistPanelVideoRenderer: {
            title: { runs: [{ text: song.title }] },
            longBylineText: byline(song),
            lengthText: { runs: [{ text: song.length }] },
            videoId,
            selected: false,
            navigationEndpoint: { watchEndpoint: { videoId } },
            thumbnail: { thumbnails: [{ url: `https://img.example/${videoId}.jpg` }] },
        },
    };
}

const numberOf = item => item.playlistPanelVideoRenderer.navigationEndpoint.watchEndpoint.index;

/**
 * A model of YouTube Music as observed live (see PROVIDER_ADAPTERS.md):
 * - the queue lives in a store, and the player follows it,
 * - queue numbers start again from 0 after a clear,
 * - `selectedItemIndex` is left behind when the queue is replaced,
 * - selecting an item always starts it from 0, playing,
 * - at the end of the queue the player just stops (autoplay off),
 * - starting a song turns autoplay on unless the user chose the setting.
 */
function createYouTubeMusic() {
    const listeners = new Set();
    const actions = [];
    const requests = [];
    const calls = [];
    let state = {
        queue: {
            items: [],
            selectedItemIndex: 0,
            nextQueueItemId: 0,
            autoplay: true,
            hasUserChangedDefaultAutoplayMode: false,
            repeatMode: "NONE",
        },
        player: { adPlaying: false, isPlaying: false },
    };
    const player = { videoId: null, state: PLAYER.UNSTARTED, positionMs: 0, since: Date.now() };

    const numbered = (items, from) => items.map((item, offset) => {
        const copy = structuredClone(item);
        copy.playlistPanelVideoRenderer.navigationEndpoint.watchEndpoint.index = from + offset;
        return copy;
    });
    const select = (items, index) => items.map((item, position) => ({
        playlistPanelVideoRenderer: { ...item.playlistPanelVideoRenderer, selected: position === index },
    }));

    function reduce(queue, { type, payload }) {
        switch (type) {
            case "CLEAR":
                // The real reducer leaves `selectedItemIndex` as it was.
                return { ...queue, items: [], nextQueueItemId: 0 };
            case "ADD_ITEMS": {
                const added = payload.shouldAssignIds ? numbered(payload.items, payload.nextQueueItemId) : payload.items;
                const items = [...queue.items];
                items.splice(payload.index, 0, ...added);
                return { ...queue, items, nextQueueItemId: queue.nextQueueItemId + payload.items.length };
            }
            case "UPDATE_ITEMS": {
                const added = payload.shouldAssignIds ? numbered(payload.items, payload.nextQueueItemId) : payload.items;
                return {
                    ...queue,
                    items: [...queue.items.slice(0, payload.currentIndex + 1), ...added],
                    nextQueueItemId: queue.nextQueueItemId + payload.items.length,
                };
            }
            case "MOVE_ITEM": {
                const items = [...queue.items];
                const [moved] = items.splice(payload.fromIndex, 1);
                items.splice(payload.toIndex, 0, moved);
                return { ...queue, items };
            }
            case "REMOVE_ITEM":
                return { ...queue, items: queue.items.filter((_, index) => index !== payload) };
            case "SET_INDEX":
                return { ...queue, items: select(queue.items, payload), selectedItemIndex: payload };
            case "SET_AUTOPLAY_ENABLED":
                return { ...queue, autoplay: payload };
            case "HAS_USER_CHANGED_DEFAULT_AUTOPLAY_MODE":
                return { ...queue, hasUserChangedDefaultAutoplayMode: payload };
            case "SET_REPEAT":
                return { ...queue, repeatMode: payload };
            default:
                return queue;
        }
    }

    const store = {
        getState: () => state,
        dispatch(action) {
            actions.push(action.type);
            let player_ = state.player;
            if (action.type === "SET_AD_PLAYING") player_ = { ...player_, adPlaying: action.payload };
            if (action.type === "SET_IS_PLAYING") player_ = { ...player_, isPlaying: action.payload };
            state = { queue: reduce(state.queue, action), player: player_ };
            for (const listener of [...listeners]) listener();
        },
        subscribe(listener) {
            listeners.add(listener);
            return () => listeners.delete(listener);
        },
    };

    const currentIndex = () => state.queue.items.findIndex(item => item.playlistPanelVideoRenderer.selected);
    const position = () =>
        player.positionMs + (player.state === PLAYER.PLAYING ? Date.now() - player.since : 0);
    function setPlayer(change) {
        player.positionMs = position();
        player.since = Date.now();
        Object.assign(player, change);
        store.dispatch({ type: "SET_IS_PLAYING", payload: player.state === PLAYER.PLAYING });
    }
    /** The player loads the selected item a moment later, from 0, playing. */
    function loadSelected() {
        const item = state.queue.items[currentIndex()];
        setPlayer({ state: PLAYER.UNSTARTED });
        setTimeout(() => {
            setPlayer({ videoId: item.playlistPanelVideoRenderer.videoId, state: PLAYER.PLAYING, positionMs: 0 });
            // The app does this after loading a song.
            if (!state.queue.hasUserChangedDefaultAutoplayMode) {
                store.dispatch({ type: "SET_AUTOPLAY_ENABLED", payload: true });
            }
        }, 10);
    }

    const queueService = {
        getCurrentItemIndex: () => Math.max(0, currentIndex()),
        selectQueueItem(item) {
            const index = state.queue.items.findIndex(candidate => numberOf(candidate) === numberOf(item));
            if (index < 0) throw new Error("Attempted to select item that is not in queue.");
            calls.push(`select:${item.playlistPanelVideoRenderer.videoId}`);
            store.dispatch({ type: "SET_INDEX", payload: index });
            loadSelected();
        },
    };

    const playerApi = {
        getPlayerState: () => player.state,
        getCurrentTime: () => position() / 1000,
        getDuration: () => 180,
        getVideoData: () => ({ video_id: player.videoId }),
        playVideo() {
            calls.push("play");
            setPlayer({ state: PLAYER.PLAYING });
        },
        pauseVideo() {
            calls.push("pause");
            if (state.player.adPlaying) store.dispatch({ type: "SET_IS_PLAYING", payload: false });
            else setPlayer({ state: PLAYER.PAUSED });
        },
        seekTo(seconds) {
            calls.push(`seek:${Math.round(seconds)}`);
            setPlayer({ positionMs: seconds * 1000 });
        },
    };

    const network = {
        async fetch(path, params) {
            requests.push(path === "/search" ? `search:${params.query}` : `queue:${params.videoIds.join(",")}`);
            if (path === "/music/get_queue") {
                return { queueDatas: params.videoIds.filter(id => CATALOG[id]).map(id => ({ content: queueItem(id) })) };
            }
            const rows = Object.entries(CATALOG)
                .filter(([, song]) => params.query.includes(song.title))
                .map(([videoId, song]) => ({
                    musicResponsiveListItemRenderer: {
                        playlistItemData: { videoId },
                        flexColumns: [
                            { musicResponsiveListItemFlexColumnRenderer: { text: { runs: [{ text: song.title }] } } },
                            {
                                musicResponsiveListItemFlexColumnRenderer: {
                                    text: { runs: [...byline(song).runs, { text: " • " }, { text: song.length }] },
                                },
                            },
                        ],
                    },
                }));
            return { contents: { sectionListRenderer: { contents: [{ musicShelfRenderer: { contents: rows } }] } } };
        },
    };

    const page = {
        store,
        queueService,
        playerApi,
        network,
        actions,
        requests,
        calls,
        player,
        /** What the user does in the page: replace the queue and play. */
        userPlays(...videoIds) {
            store.dispatch({ type: "CLEAR" });
            store.dispatch({
                type: "ADD_ITEMS",
                payload: { items: videoIds.map(queueItem), index: 0, shouldAssignIds: true, nextQueueItemId: 0 },
            });
            queueService.selectQueueItem(state.queue.items[0]);
        },
        userQueues(videoId, index = state.queue.items.length) {
            store.dispatch({
                type: "ADD_ITEMS",
                payload: { items: [queueItem(videoId)], index, shouldAssignIds: true, nextQueueItemId: state.queue.nextQueueItemId },
            });
        },
        /** The current song plays to its end. */
        songEnds() {
            const next = currentIndex() + 1;
            if (next < state.queue.items.length) {
                store.dispatch({ type: "SET_INDEX", payload: next });
                loadSelected();
            } else {
                setPlayer({ state: PLAYER.ENDED, positionMs: 180_000 });
            }
        },
        setPlayer,
        adBreak(active) {
            store.dispatch({ type: "SET_AD_PLAYING", payload: active });
            store.dispatch({ type: "SET_IS_PLAYING", payload: active });
        },
    };

    // Start as a page that already played one song and is on its second.
    store.dispatch({
        type: "ADD_ITEMS",
        payload: { items: [queueItem("a"), queueItem("cur")], index: 0, shouldAssignIds: true, nextQueueItemId: 0 },
    });
    store.dispatch({ type: "SET_INDEX", payload: 1 });
    Object.assign(player, { videoId: "cur", state: PLAYER.PLAYING, positionMs: 1_000, since: Date.now() });
    actions.length = 0;
    calls.length = 0;
    return page;
}

async function loadAdapter(setup) {
    const page = createYouTubeMusic();
    setup?.(page);
    const events = [];
    let randomId = 0;

    const elements = {
        "ytmusic-app": { inst: { networkManager: page.network } },
        "ytmusic-player-bar": { inst: { store: page.store, queue: page.queueService, playerApi: page.playerApi } },
    };
    const context = vm.createContext({
        window: {},
        document: { querySelector: selector => elements[selector] ?? null },
        performance: { now: () => Date.now() },
        crypto: { randomUUID: () => `generated-${++randomId}` },
        setTimeout,
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

const entry = (entryId, videoId, extra = {}) => ({
    entryId,
    track: { title: CATALOG[videoId].title, artists: [CATALOG[videoId].artists[0]], ...extra },
});
const ghost = entryId => ({ entryId, track: { title: "Ghost Song", artists: ["Nobody"] } });

function playback(isPlaying = true, positionMs = 0) {
    return { isPlaying, positionMs, positionCapturedAtMs: Date.now() };
}

const entryIds = state => Array.from(state.queue, item => item.entryId);
const operations = events => events.filter(event => event.type === "Operation").map(event => event.data);
// Local changes are reported after a debounce, seeks at the next poll.
const settle = () => sleep(800);

async function loadSession(bridge, videoIds = ["a", "b", "c"], play = playback()) {
    return bridge.applySnapshot({
        queue: videoIds.map(id => entry(`session-${id}`, id)),
        currentEntryId: `session-${videoIds[0]}`,
        playback: play,
    });
}

async function withAdapter(setup, run) {
    const loaded = await loadAdapter(typeof setup === "function" && run ? setup : undefined);
    try {
        await (run ?? setup)(loaded);
    } finally {
        // Stops the adapter's timers so the test process can exit.
        await loaded.bridge.dispose();
    }
}

test("YouTube Music adapter exposes the shared bridge API", () => withAdapter(async ({ bridge, events, window }) => {
    assert.equal(bridge.version, 1);
    assert.equal(window.__MYJAM_BRIDGE_V1__, bridge);
    assert.ok(events.some(event => event.type === "Ready"));

    const state = bridge.getState();
    assert.equal(state.queue.length, 1, "the song that already played is history");
    assert.deepEqual({ ...state.queue[0].track, artists: [...state.queue[0].track.artists] }, {
        title: "Current Song",
        artists: ["Artist One"],
        isrc: null,
        album: "First Album",
        durationMs: 180_000,
    });
    assert.equal(state.currentEntryId, state.queue[0].entryId);
    assert.equal(state.playback.isPlaying, true);

    const outcome = await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: false });
    assert.deepEqual([...outcome.unavailableEntryIds], []);
    assert.equal(bridge.getState().playback.isPlaying, false);
}));

test("YouTube Music reads artists from a byline with and without links", () => withAdapter(async ({ bridge, page }) => {
    page.userPlays("b", "video");
    await sleep(50);
    const [song, video] = bridge.getState().queue;
    assert.deepEqual([...song.track.artists], ["Artist B", "Guest"]);
    assert.equal(song.track.album, "Album B");
    assert.equal(song.track.durationMs, 245_000);
    assert.deepEqual([...video.track.artists], ["Band X", "Band Y"]);
    assert.equal(video.track.album, null);
}));

test("YouTube Music still finds the current song after the user replaced the queue", () => withAdapter(async ({ bridge, page }) => {
    // The page keeps its old selected index (here 1) for a queue of one.
    page.userPlays("b");
    await sleep(50);
    assert.equal(page.store.getState().queue.selectedItemIndex, 0);
    page.store.getState().queue.selectedItemIndex = 6;

    const state = bridge.getState();
    assert.equal(state.queue.length, 1);
    assert.equal(state.queue[0].track.title, "Song B");
}));

test("YouTube Music skips upcoming tracks it cannot find and reports them", () => withAdapter(async ({ bridge, page }) => {
    const outcome = await bridge.applySnapshot({
        queue: [entry("session-a", "a"), ghost("session-ghost"), entry("session-b", "b")],
        currentEntryId: "session-a",
        playback: playback(),
    });

    assert.deepEqual([...outcome.unavailableEntryIds], ["session-ghost"]);
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-b"]);
    assert.deepEqual(page.calls.filter(call => call.startsWith("select")), ["select:a"]);
}));

test("YouTube Music rejects an unplayable current track and changes nothing", () => withAdapter(async ({ bridge, page }) => {
    await assert.rejects(
        bridge.applySnapshot({
            queue: [ghost("session-ghost"), entry("session-a", "a")],
            currentEntryId: "session-ghost",
            playback: playback(),
        }),
        error => {
            assert.equal(error.code, "trackUnavailable");
            assert.deepEqual([...error.details.entryIds], ["session-ghost"]);
            assert.match(error.message, /No matching YouTube Music song found for "Ghost Song"/);
            return true;
        },
    );
    assert.deepEqual(page.actions, [], "the queue must not be touched");
    assert.deepEqual(page.requests, ["search:Ghost Song Nobody"], "the rest isn't looked up");
}));

test("Joining a paused session ends up paused at the session's position", () => withAdapter(async ({ bridge, page, events }) => {
    // The page can only start a song playing from 0; the adapter corrects it.
    await loadSession(bridge, ["a", "b"], playback(false, 42_000));

    const state = bridge.getState();
    assert.equal(state.playback.isPlaying, false);
    assert.ok(Math.abs(state.playback.positionMs - 42_000) < 500, `position ${state.playback.positionMs}`);
    assert.equal(page.player.videoId, "a");
    await settle();
    assert.deepEqual(operations(events), []);
}));

test("Queue edits keep the current track playing", () => withAdapter(async ({ bridge, page }) => {
    await loadSession(bridge);
    page.actions.length = 0;
    page.calls.length = 0;
    page.requests.length = 0;

    await bridge.applyOperation({ type: "queue.insert", index: 1, entry: entry("session-new", "cur") });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-new", "session-b", "session-c"]);

    await bridge.applyOperation({ type: "queue.move", entryId: "session-c", toIndex: 1 });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-c", "session-new", "session-b"]);

    await bridge.applyOperation({ type: "queue.remove", entryId: "session-new" });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-c", "session-b"]);

    await bridge.applyOperation({
        type: "queue.replace",
        currentEntryId: "session-a",
        entries: [entry("session-a", "a"), entry("session-b", "b")],
    });
    assert.deepEqual(entryIds(bridge.getState()), ["session-a", "session-b"]);

    assert.deepEqual([...new Set(page.actions.filter(type => !type.startsWith("SET_")))], ["UPDATE_ITEMS"]);
    assert.deepEqual(page.calls, [], "the player was never touched");
    assert.deepEqual(page.requests, ["search:Current Song Artist One", "queue:cur"], "queued songs aren't looked up again");
    assert.equal(page.player.videoId, "a");
}));

test("The same song twice gets two entries", () => withAdapter(async ({ bridge }) => {
    await bridge.applySnapshot({
        queue: [entry("first", "a"), entry("second", "a"), entry("third", "a")],
        currentEntryId: "first",
        playback: playback(),
    });
    assert.deepEqual(entryIds(bridge.getState()), ["first", "second", "third"]);
}));

test("Advancing selects the next item; an advance the page already made is left alone", () => withAdapter(async ({ bridge, page }) => {
    await loadSession(bridge);
    page.calls.length = 0;

    await bridge.applyOperation({ type: "playback.advance", targetEntryId: "session-b", positionMs: 0, isPlaying: true });
    assert.deepEqual(page.calls, ["select:b"]);
    assert.deepEqual(entryIds(bridge.getState()), ["session-b", "session-c"]);

    // Every client reports the same song end.
    page.calls.length = 0;
    await bridge.applyOperation({ type: "playback.advance", targetEntryId: "session-b", positionMs: 200, isPlaying: true });
    assert.deepEqual(page.calls, []);
}));

test("An empty session queue clears the page's queue", () => withAdapter(async ({ bridge }) => {
    await bridge.applySnapshot({ queue: [], currentEntryId: null, playback: playback(false) });

    const state = bridge.getState();
    assert.deepEqual([...state.queue], []);
    assert.equal(state.currentEntryId, null);
    assert.equal(state.playback.isPlaying, false);
}));

test("YouTube Music publishes local changes but not the effects of a remote apply", () => withAdapter(async ({ bridge, page, events }) => {
    await loadSession(bridge);
    await bridge.applyOperation({ type: "playback.setPlaying", isPlaying: false });
    await bridge.applyOperation({ type: "playback.seek", entryId: "session-a", positionMs: 60_000 });
    await settle();
    assert.deepEqual(operations(events), []);

    // Positive control: the user resumes and seeks in the page.
    page.setPlayer({ state: PLAYER.PLAYING });
    await settle();
    page.setPlayer({ positionMs: 5_000 });
    await settle();
    const [resumed, sought] = operations(events);
    assert.deepEqual(resumed, { type: "playback.setPlaying", isPlaying: true });
    assert.equal(sought.type, "playback.seek");
    assert.equal(sought.entryId, "session-a");
    assert.ok(Math.abs(sought.positionMs - 5_000) < 1_000, `position ${sought.positionMs}`);
}));

test("The user's queue edits are published as small operations", () => withAdapter(async ({ bridge, page, events }) => {
    await loadSession(bridge);
    await settle();

    page.userQueues("cur");
    await settle();
    const [insert] = operations(events);
    assert.equal(insert.type, "queue.insert");
    assert.equal(insert.index, 3);
    assert.equal(insert.entry.track.title, "Current Song");

    events.length = 0;
    page.store.dispatch({ type: "REMOVE_ITEM", payload: 1 });
    await settle();
    assert.deepEqual(operations(events), [{ type: "queue.remove", entryId: "session-b" }]);

    events.length = 0;
    page.store.dispatch({ type: "MOVE_ITEM", payload: { fromIndex: 2, toIndex: 1 } });
    await settle();
    assert.deepEqual(operations(events), [{ type: "queue.move", entryId: "session-c", toIndex: 2 }]);
}));

test("A song end is an advance; the end of the queue is only a pause", () => withAdapter(async ({ bridge, page, events }) => {
    await loadSession(bridge, ["a", "b"]);
    await settle();

    page.songEnds();
    await settle();
    const [advance, ...rest] = operations(events);
    assert.equal(advance.type, "playback.advance");
    assert.equal(advance.targetEntryId, "session-b");
    assert.deepEqual(rest, []);

    events.length = 0;
    page.setPlayer({ positionMs: 179_000 });
    await settle();
    events.length = 0;
    page.songEnds();
    await settle();
    assert.deepEqual(operations(events), [{ type: "playback.setPlaying", isPlaying: false }]);
    assert.deepEqual(entryIds(bridge.getState()), ["session-b"]);
}));

test("The user starting another song replaces the session queue", () => withAdapter(async ({ bridge, page, events }) => {
    await loadSession(bridge);
    await settle();

    page.userPlays("c");
    await settle();
    const [replace] = operations(events);
    assert.equal(replace.type, "queue.replace");
    assert.equal(replace.entries.length, 1);
    assert.equal(replace.entries[0].track.title, "Song C");
    assert.notEqual(replace.currentEntryId, "session-c", "a new occurrence, although its queue number was used before");
}));

test("Autoplay and repeat are switched off for a session, kept off, and put back", () => withAdapter(page => {
    page.store.dispatch({ type: "SET_REPEAT", payload: "ALL" });
}, async ({ bridge, page, events }) => {
    const queue = () => page.store.getState().queue;

    const previous = await bridge.prepareForSession();
    assert.deepEqual({ ...previous }, { autoplay: true, autoplayChosen: false, repeatMode: "ALL" });
    assert.equal(queue().autoplay, false);
    assert.equal(queue().repeatMode, "NONE");

    // Loading a song would turn autoplay back on.
    await loadSession(bridge);
    await settle();
    assert.equal(queue().autoplay, false);

    await bridge.restoreAfterSession(previous);
    assert.equal(queue().autoplay, true);
    assert.equal(queue().hasUserChangedDefaultAutoplayMode, false);
    assert.equal(queue().repeatMode, "ALL");
    assert.deepEqual(operations(events), []);
}));

test("An advertisement is reported as an interruption, and changes wait for it", () => withAdapter(async ({ bridge, page, events }) => {
    await loadSession(bridge);
    await settle();
    const interruptions = () => events.filter(event => event.type === "Interruption").map(event => event.data.active);

    page.adBreak(true);
    await settle();
    assert.deepEqual(interruptions(), [true]);
    assert.equal(bridge.getState().playback.isPlaying, false, "the track itself isn't playing");

    // Nothing the page does during the advertisement is the user's doing.
    page.setPlayer({ positionMs: 90_000 });
    await settle();
    assert.deepEqual(operations(events), []);

    // A session change that arrives meanwhile is applied afterwards.
    let applied = false;
    const pending = bridge.applyOperation({ type: "playback.seek", entryId: "session-a", positionMs: 30_000 })
        .then(() => { applied = true; });
    await sleep(300);
    assert.equal(applied, false);

    page.adBreak(false);
    await pending;
    assert.ok(Math.abs(bridge.getState().playback.positionMs - 30_000) < 1_000);
    await settle();
    assert.deepEqual(interruptions(), [true, false]);
    assert.deepEqual(operations(events), []);
}));

test("Settling after attaching pauses the page, also behind an advertisement", () => withAdapter(async ({ bridge, page, events }) => {
    page.adBreak(true);

    await bridge.settle({ pause: true });

    assert.equal(page.store.getState().player.isPlaying, false);
    assert.ok(page.calls.includes("pause"));
    assert.deepEqual(operations(events), []);
}));
