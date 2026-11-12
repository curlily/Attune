/*
 * Attune adapter for Spotify Desktop.
 *
 * This script runs inside Spotify's renderer. All Spotify implementation
 * details stay here; Rust only talks to the shared bridge contract.
 * Everything used below is private to Spotify and may change between
 * releases, so each dependency is checked before it is used. What was
 * observed live (Spotify 1.3.8) is collected in PROVIDER_ADAPTERS.md.
 */
(async () => {
    "use strict";

    const BRIDGE_KEY = "__ATTUNE_BRIDGE_V1__";
    const BRIDGE_VERSION = 1;
    const EVENT_BINDING = "__jamEvent";
    const PLATFORM_TIMEOUT_MS = 30_000;
    const PLAYER_STATE_WAIT_MS = 5_000;
    const SEARCH_TIMEOUT_MS = 12_000;
    // Lookups are independent requests (verified live), so several may run
    // at once.
    const SEARCH_CONCURRENCY = 4;
    // Spotify sends bursts of updates for one change; handle the last one.
    const EVENT_DEBOUNCE_MS = 150;
    // A playing track that is still buffering, or a queue item without its
    // metadata yet, is in between two states: wait this long at most.
    const BUFFERING_WAIT_MS = 3_000;
    // After applying a remote change, local changes are ignored this long,
    // which covers the debounced event of its last update. The change itself
    // is covered exactly: applying waits until Spotify shows it.
    const REMOTE_CHANGE_SUPPRESSION_MS = 300;
    // How long Spotify may take to show a change we asked for.
    const CHANGE_TIMEOUT_MS = 5_000;
    // Waits check on Spotify's events (see `watchPlayer`); this is only a
    // safety net for a change that came without one.
    const FALLBACK_CHECK_MS = 250;
    // How often a hold re-applies its target state at most.
    const HOLD_RETRY_MS = 250;
    // Positions this close to the target are left alone.
    const POSITION_TOLERANCE_MS = 1_000;
    // A position jump larger than this is a seek.
    const SEEK_THRESHOLD_MS = 1_200;
    // The player state lists at most this many upcoming items; the queue
    // itself can be longer (verified live).
    const STATE_NEXT_ITEMS_CAP = 50;
    // Settling after attaching (see `settle`).
    const SETTLE_TIMEOUT_MS = 8_000;
    const SETTLE_STABLE_MS = 1_000;
    const SETTLE_EMPTY_MS = 3_000;
    const SETTLE_PAUSED_MS = 1_500;
    // Spotify's repeat modes: off, context, track.
    const REPEAT_OFF = 0;
    // Queue writes with an outdated revision are refused; read again and retry.
    const QUEUE_WRITE_ATTEMPTS = 4;
    const RECENT_EVENT_LIMIT = 200;
    const PLAY_ORIGIN = { featureIdentifier: "attune", referrerIdentifier: "attune" };
    // Fallback for the `searchTracks` query if it can't be read from
    // Spotify's own code (Spotify 1.3.8).
    const SEARCH_TRACKS_HASH = "b02683192a98dde7966b5e6655a79eeb62713eab703eda9902c932818dd52751";

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

    const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

    // ---------------------------------------------------------------------
    // Spotify service discovery
    // ---------------------------------------------------------------------

    let platform = null;
    let player = null;
    let contextPlayer = null;

    // Spotify hands its platform object (service registry) to React
    // components as a prop; find it on any rendered element.
    function findPlatform() {
        let playerOnly = null;
        for (const element of document.querySelectorAll("*")) {
            const fiberKey = Object.keys(element).find(key => key.startsWith("__reactFiber"));
            if (!fiberKey) continue;
            for (let fiber = element[fiberKey]; fiber; fiber = fiber.return) {
                const props = fiber.memoizedProps;
                if (!props || typeof props !== "object") continue;
                if (typeof props.platform?.getRegistry === "function") return props.platform;
                // Some components get the PlayerAPI directly. Without the
                // platform there is no search or settings, so keep looking.
                if (!playerOnly &&
                    typeof props.playerAPI?.getState === "function" &&
                    typeof props.playerAPI?.getEvents === "function") {
                    playerOnly = { playerAPI: props.playerAPI };
                }
            }
        }
        return playerOnly;
    }

    function getService(name) {
        const registry = platform?.getRegistry?.();
        if (typeof registry?._map?.[Symbol.iterator] !== "function") return null;
        for (const [key, entry] of registry._map) {
            if (key?.description === name) return entry?.instance ?? null;
        }
        return null;
    }

    async function waitForPlayer() {
        const deadline = performance.now() + PLATFORM_TIMEOUT_MS;
        let foundAt = null;
        while (performance.now() < deadline) {
            platform = findPlatform();
            const candidate = platform?.playerAPI ?? getService("PlayerAPI");
            if (typeof candidate?.getState === "function" &&
                typeof candidate?.getEvents === "function") {
                // Its first state arrives a moment later; with nothing ever
                // played there may be none at all.
                foundAt ??= performance.now();
                if (candidate.getState() || performance.now() - foundAt > PLAYER_STATE_WAIT_MS) {
                    return candidate;
                }
            }
            await sleep(100);
        }
        throw new Error("Timed out waiting for Spotify's PlayerAPI");
    }

    // Spotify creates these services lazily, after the adapter is ready
    // (verified live), so look them up when they are needed.
    function getSettings() {
        return getService("SettingsAPI") ?? platform?.getSettingsAPI?.() ?? null;
    }

    function getGraphql() {
        const loader = getService("GraphQLLoader") ?? platform?.getGraphQLLoader?.() ?? null;
        return typeof loader === "function" ? loader : null;
    }

    function requireContextPlayer() {
        if (typeof contextPlayer?.setQueue !== "function" || typeof contextPlayer?.play !== "function") {
            throw new Error("This Spotify version isn't supported yet: its internal player is unavailable");
        }
        return contextPlayer;
    }

    // Spotify's RPCs answer with `{ error, reasons }` instead of throwing.
    function checkRpc(result, action) {
        if (result && result.error) {
            throw new Error(`Spotify refused to ${action} (code ${result.error} ${result.reasons ?? ""})`.trim());
        }
    }

    // ---------------------------------------------------------------------
    // Track metadata and matching
    // ---------------------------------------------------------------------

    // A recording could not be identified. Unlike other failures this is
    // expected and per-track: the rest of the queue can still be loaded.
    class TrackLookupError extends Error {
        constructor(message) {
            super(message);
            this.name = "TrackLookupError";
        }
    }

    // Structured bridge error understood by Rust (Error::TrackUnavailable).
    function trackUnavailableError(failures) {
        const error = new Error(
            "Track unavailable on Spotify: " + failures.map(failure => failure.error.message).join("; "),
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

    // Services disagree on where featured artists go: "Shoong!" on one is
    // "Shoong! (feat. LISA of BLACKPINK)" on another (verified live).
    function normalizeTitle(value) {
        return normalize(String(value ?? "")
            .replace(/[([][^)\]]*\b(feat|ft|featuring|with)\b[^)\]]*[)\]]/gi, " ")
            .replace(/\s[-–]\s*(feat|ft|featuring)\b.*$/i, " "));
    }

    function isTrackUri(uri) {
        return typeof uri === "string" && uri.startsWith("spotify:track:");
    }

    function toTrackUri(id) {
        return isTrackUri(id) ? id : `spotify:track:${id}`;
    }

    function imageUrl(image) {
        if (typeof image !== "string" || !image) return null;
        if (image.startsWith("http")) return image.startsWith("https://") && image.length <= 1024 ? image : null;
        const id = image.split(":")[2];
        return id ? `https://i.scdn.co/image/${id}` : null;
    }

    function finiteOrNull(value) {
        const number = Number(value);
        return value != null && Number.isFinite(number) ? Math.round(number) : null;
    }

    // A track in the player state or in Spotify's decorated queue.
    function descriptorFromItem(item) {
        return {
            title: item.name ?? item.metadata?.title ?? "",
            artists: (item.artists ?? [])
                .map(artist => typeof artist === "string" ? artist : artist?.name)
                .filter(Boolean),
            // Spotify's player doesn't carry ISRCs.
            isrc: null,
            album: item.album?.name ?? item.metadata?.album_title ?? null,
            durationMs: finiteOrNull(item.duration?.milliseconds ?? item.metadata?.duration),
            coverUrl: imageUrl(item.metadata?.image_url ?? item.images?.[0]?.url),
        };
    }

    // A track in Spotify's internal queue: only string metadata.
    function descriptorFromMetadata(metadata = {}) {
        const artists = [metadata.artist_name];
        for (let index = 1; metadata[`artist_name:${index}`]; index += 1) {
            artists.push(metadata[`artist_name:${index}`]);
        }
        return {
            title: metadata.title ?? "",
            artists: artists.filter(Boolean),
            isrc: null,
            album: metadata.album_title ?? null,
            durationMs: finiteOrNull(metadata.duration),
            coverUrl: imageUrl(metadata.image_url ?? metadata.image_large_url),
        };
    }

    // ---------------------------------------------------------------------
    // Search
    // ---------------------------------------------------------------------

    let searchOperation = null;
    const resolvedUriCache = new Map();

    // Spotify's GraphQL queries are sent by name and hash. Read the hash of
    // its own track search from the loaded code, or from the search page's
    // script if that page was never opened.
    async function getSearchOperation() {
        if (searchOperation) return searchOperation;
        const pattern = /"searchTracks"\s*,\s*"query"\s*,\s*"([a-f0-9]{64})"/;
        let hash = null;

        for (const module of Object.values(window.__webpack_modules__ ?? {})) {
            let source;
            try {
                source = Function.prototype.toString.call(module);
            } catch {
                continue;
            }
            hash = source.match(pattern)?.[1] ?? null;
            if (hash) break;
        }
        if (!hash) {
            try {
                const response = await fetch(new URL("/xpui-routes-search.js", location.origin));
                if (response.ok) hash = (await response.text()).match(pattern)?.[1] ?? null;
            } catch {
                // Fall back to the hash of the version this was written for.
            }
        }

        searchOperation = {
            name: "searchTracks",
            operation: "query",
            sha256Hash: hash ?? SEARCH_TRACKS_HASH,
            value: null,
        };
        return searchOperation;
    }

    /** Spotify's track search. Resolves to candidates in Spotify's ranking. */
    async function searchTracks(searchTerm, limit = 10) {
        const graphql = getGraphql();
        if (!graphql) throw new Error("Spotify's search is unavailable in this version");
        const request = graphql(await getSearchOperation(), {
            searchTerm,
            offset: 0,
            limit,
            numberOfTopResults: limit,
            includeAudiobooks: false,
            includeAuthors: false,
            includePreReleases: false,
        });
        const timeout = new Promise((_, reject) => setTimeout(
            () => reject(new TrackLookupError(`Spotify search timed out for "${searchTerm}"`)),
            SEARCH_TIMEOUT_MS,
        ));
        const response = await Promise.race([request, timeout]);
        const items = response?.data?.searchV2?.tracksV2?.items;
        if (!Array.isArray(items)) {
            throw new Error("Spotify's search returned an unexpected response");
        }

        return items.map(wrapper => {
            const data = wrapper?.item?.data;
            if (data?.__typename !== "Track" || !isTrackUri(data.uri)) return null;
            if (data.playability && data.playability.playable === false) return null;
            return {
                uri: data.uri,
                track: {
                    title: data.name ?? "",
                    artists: (data.artists?.items ?? []).map(artist => artist?.profile?.name).filter(Boolean),
                    isrc: null,
                    album: data.albumOfTrack?.name ?? null,
                    durationMs: finiteOrNull(data.duration?.totalMilliseconds),
                },
                coverUrl: data.albumOfTrack?.coverArt?.sources?.[0]?.url ?? null,
            };
        }).filter(Boolean);
    }

    function artistOverlap(trackA, trackB) {
        const artistsA = new Set((trackA.artists ?? []).map(normalize).filter(Boolean));
        return (trackB.artists ?? []).map(normalize).some(name => artistsA.has(name));
    }

    function scoreCandidate(source, candidate) {
        if (normalizeTitle(source.title) !== normalizeTitle(candidate.track.title)) return -1;
        if (!artistOverlap(source, candidate.track)) return -1;

        let score = 10; // Same title and at least one matching artist.
        if (normalize(source.title) === normalize(candidate.track.title)) score += 1;
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

    async function resolveTrack(track) {
        const existingProviderId = track.providerTrackId ?? track.provider_track_id;
        if (track.provider === "Spotify" && existingProviderId) {
            return toTrackUri(String(existingProviderId));
        }

        const cacheKey = [
            normalize(track.title),
            (track.artists ?? []).map(normalize).join("|"),
            normalize(track.isrc),
            normalize(track.album),
            track.durationMs ?? "",
        ].join("::");
        if (resolvedUriCache.has(cacheKey)) return resolvedUriCache.get(cacheKey);

        let uri = null;

        // An ISRC names one recording, and Spotify's search understands
        // `isrc:` (verified live). Still require the title or an artist to
        // agree, in case the code is wrong.
        if (track.isrc) {
            const candidates = await searchTracks(`isrc:${String(track.isrc).trim()}`, 5);
            uri = candidates.find(candidate =>
                normalizeTitle(candidate.track.title) === normalizeTitle(track.title) ||
                artistOverlap(track, candidate.track))?.uri ?? null;
        }

        if (!uri) {
            const query = `${track.title} ${(track.artists ?? []).slice(0, 2).join(" ")}`.trim();
            const candidates = await searchTracks(query, 10);
            // Sorting is stable: equal scores keep Spotify's ranking. Equal
            // candidates are the same recording on different releases
            // (single, album, deluxe), so the first is as good as any.
            const ranked = candidates
                .map(candidate => ({ ...candidate, score: scoreCandidate(track, candidate) }))
                .filter(candidate => candidate.score >= 0)
                .sort((a, b) => b.score - a.score);
            uri = ranked[0]?.uri ?? null;
        }

        if (!uri) {
            throw new TrackLookupError(`No matching Spotify recording found for "${track.title}"`);
        }
        resolvedUriCache.set(cacheKey, uri);
        return uri;
    }

    // Resolve every entry independently, a few at a time. Entries already
    // in Spotify's queue (`known`: entryId -> URI) need no search. Lookup
    // failures are returned per entry; any other error is rethrown.
    async function resolveEntries(entries, known) {
        const results = new Array(entries.length);
        let next = 0;
        async function worker() {
            while (next < entries.length) {
                const index = next;
                next += 1;
                const entry = entries[index];
                const knownUri = known.get(entry.entryId);
                if (knownUri) {
                    results[index] = { entry, uri: knownUri };
                    continue;
                }
                try {
                    results[index] = { entry, uri: await resolveTrack(entry.track) };
                } catch (error) {
                    if (!(error instanceof TrackLookupError)) throw error;
                    results[index] = { entry, error };
                }
            }
        }
        await Promise.all(Array.from({ length: Math.min(SEARCH_CONCURRENCY, entries.length) }, worker));
        return results;
    }

    // ---------------------------------------------------------------------
    // Reading Spotify's queue
    // ---------------------------------------------------------------------
    //
    // Spotify's upcoming list has three kinds of items (`provider`):
    // - "queue": added by the user ("Next in queue"),
    // - "context": the rest of the playlist or album being played,
    // - "autoplay": recommendations after the context ends.
    // Attune's queue is the current track plus the queue and context items.
    // Autoplay items are left out: Spotify regenerates them at will, and
    // autoplay is switched off for the length of a session.

    // Spotify identifies each queue occurrence by a `uid`; Attune entry IDs
    // are mapped to those.
    const entryIdByUid = new Map();
    // When the queue runs out, Spotify returns to the track its context
    // started with (see `detectQueueEnd`). While that lasts, the finished
    // entry is still reported as current.
    let queueEnd = null;

    function internalQueue() {
        return player?._queue?.getInternalQueue?.() ?? null;
    }

    function isSessionItem(uri, provider) {
        return isTrackUri(uri) && provider !== "autoplay";
    }

    /**
     * Spotify's playback as plain data: `current` and `upcoming` are
     * `{ uid, uri, track }`.
     */
    function readPlayer(state = player.getState()) {
        const item = state?.item;
        const current = item && isTrackUri(item.uri)
            ? { uid: item.uid || `current:${item.uri}`, uri: item.uri, track: descriptorFromItem(item) }
            : null;

        // The state's own list matches the rest of the state, but is cut
        // off at 50 items. Take what follows from the full queue, which
        // arrives separately and slightly later.
        const nextItems = state?.nextItems ?? [];
        const upcoming = nextItems
            .filter(next => isSessionItem(next.uri, next.provider))
            .map((next, index) => ({
                uid: next.uid || `next:${next.uri}:${index}`,
                uri: next.uri,
                track: descriptorFromItem(next),
            }));
        if (nextItems.length >= STATE_NEXT_ITEMS_CAP) {
            const tracks = internalQueue()?.nextTracks ?? [];
            const lastUid = nextItems[nextItems.length - 1]?.uid;
            const lastIndex = tracks.findIndex(track => track.contextTrack?.uid === lastUid);
            if (lastUid && lastIndex >= 0) {
                for (const track of tracks.slice(lastIndex + 1)) {
                    const contextTrack = track.contextTrack ?? {};
                    if (!isSessionItem(contextTrack.uri, track.provider)) continue;
                    upcoming.push({
                        uid: contextTrack.uid,
                        uri: contextTrack.uri,
                        track: descriptorFromMetadata(contextTrack.metadata),
                    });
                }
            }
        }

        return {
            current,
            upcoming,
            isPaused: !current || Boolean(state?.isPaused),
            isBuffering: Boolean(state?.isBuffering),
            positionMs: current ? Math.max(0, Math.round(state?.positionAsOfTimestamp ?? 0)) : 0,
            // `positionAsOfTimestamp` is the position at `timestamp` (epoch
            // ms); both are only updated on events (verified live).
            capturedAtMs: Number.isFinite(state?.timestamp) ? state.timestamp : Date.now(),
            durationMs: finiteOrNull(state?.duration),
        };
    }

    function entryIdFor(uid) {
        if (!entryIdByUid.has(uid)) entryIdByUid.set(uid, crypto.randomUUID());
        return entryIdByUid.get(uid);
    }

    function queueEndApplies(view) {
        return Boolean(queueEnd && view.current?.uid === queueEnd.uid &&
            view.isPaused && view.positionMs < SEEK_THRESHOLD_MS && view.upcoming.length === 0);
    }

    function stateFromView(view) {
        if (!view.current) {
            return {
                queue: [],
                currentEntryId: null,
                playback: { isPlaying: false, positionMs: 0, positionCapturedAtMs: Date.now() },
            };
        }
        if (queueEndApplies(view)) {
            return {
                queue: [queueEnd.entry],
                currentEntryId: queueEnd.entry.entryId,
                playback: {
                    isPlaying: false,
                    positionMs: queueEnd.positionMs,
                    positionCapturedAtMs: view.capturedAtMs,
                },
            };
        }

        const queue = [view.current, ...view.upcoming].map(item => ({
            entryId: entryIdFor(item.uid),
            track: item.track,
        }));
        return {
            queue,
            currentEntryId: queue[0].entryId,
            playback: {
                isPlaying: !view.isPaused,
                positionMs: view.positionMs,
                positionCapturedAtMs: view.capturedAtMs,
            },
        };
    }

    function getState() {
        return stateFromView(readPlayer());
    }

    // Session entry ID -> Spotify URI for everything in Spotify's queue.
    function knownUris(view = readPlayer()) {
        const known = new Map();
        if (queueEndApplies(view)) return known;
        for (const item of [view.current, ...view.upcoming]) {
            if (item) known.set(entryIdFor(item.uid), item.uri);
        }
        return known;
    }

    function livePosition(view) {
        return view.positionMs + (view.isPaused ? 0 : Math.max(0, Date.now() - view.capturedAtMs));
    }

    // ---------------------------------------------------------------------
    // Applying changes: suppression and waiting
    // ---------------------------------------------------------------------

    let applying = 0;
    let suppressLocalChangesUntil = 0;
    // Remote changes are applied one after another.
    let applyChain = Promise.resolve();

    /**
     * Run `action` as a remote change: nothing Spotify does meanwhile is
     * reported as the user's doing.
     */
    function asRemoteChange(action) {
        const run = applyChain.then(async () => {
            applying += 1;
            try {
                return await action();
            } finally {
                applying -= 1;
                suppressLocalChangesUntil = Date.now() + REMOTE_CHANGE_SUPPRESSION_MS;
                rebase();
            }
        });
        applyChain = run.catch(() => {});
        return run;
    }

    /**
     * Call `check` (in a microtask, outside Spotify's emitter) whenever
     * Spotify's state may have changed. Returns a function that stops it.
     *
     * Verified live: the state changes only with an `update` event (the
     * full queue with `queue_update`). Polling instead would run at 1 Hz
     * while Spotify's window is hidden: Chromium runs a hidden page's timers
     * once a second when the page itself is silent (verified live in TIDAL),
     * and Spotify's sound comes from its native player, not the page
     * (assumed: not measured in Spotify).
     */
    function watchPlayer(check) {
        let scheduled = false;
        let stopped = false;
        let fallback = null;
        const run = () => {
            scheduled = false;
            if (!stopped) check();
        };
        const schedule = () => {
            if (scheduled || stopped) return;
            scheduled = true;
            Promise.resolve().then(run);
        };
        const events = player?.getEvents?.();
        const names = ["update", "queue_update"];
        const removers = events ? names.map(name => events.addListener(name, schedule)) : [];
        const poll = () => {
            schedule();
            fallback = setTimeout(poll, FALLBACK_CHECK_MS);
        };
        fallback = setTimeout(poll, FALLBACK_CHECK_MS);
        return () => {
            stopped = true;
            clearTimeout(fallback);
            names.forEach((name, index) => {
                if (typeof removers[index] === "function") removers[index]();
                else events?.removeListener?.(name, schedule);
            });
        };
    }

    /** Wait until Spotify's state satisfies `condition`. False on timeout. */
    function waitFor(condition, timeoutMs = CHANGE_TIMEOUT_MS) {
        return new Promise(resolve => {
            let done = false;
            let timer = null;
            let stop = () => {};
            const holds = () => {
                try {
                    return Boolean(player && condition(player.getState()));
                } catch {
                    return false;
                }
            };
            const finish = value => {
                if (done) return;
                done = true;
                clearTimeout(timer);
                stop();
                resolve(value);
            };
            if (holds()) return finish(true);
            stop = watchPlayer(() => {
                if (holds()) finish(true);
            });
            timer = setTimeout(() => finish(holds()), timeoutMs);
        });
    }

    /** Resolve once `key()` hasn't changed for `quietMs()`, or at the timeout. */
    function waitUntilQuiet(key, quietMs, timeoutMs) {
        const deadline = Date.now() + timeoutMs;
        return new Promise(resolve => {
            let last = key();
            let since = Date.now();
            let done = false;
            let timer = null;
            let stop = () => {};
            const finish = () => {
                if (done) return;
                done = true;
                clearTimeout(timer);
                stop();
                resolve();
            };
            const check = () => {
                if (done) return;
                const now = Date.now();
                const value = key();
                if (value !== last) {
                    last = value;
                    since = now;
                }
                const left = Math.min(quietMs() - (now - since), deadline - now);
                if (left <= 0) return finish();
                clearTimeout(timer);
                timer = setTimeout(check, left);
            };
            stop = watchPlayer(check);
            check();
        });
    }

    /**
     * Re-apply a target state until it has held for `stableMs`: `correct()`
     * runs whenever `matches()` is false (at most every HOLD_RETRY_MS) and
     * returns whether it did anything. Checked on Spotify's events, plus a
     * timer for the stable period. Resolves to whether it held.
     */
    function holdUntil({ timeoutMs, stableMs, matches, correct }) {
        const deadline = Date.now() + timeoutMs;
        return new Promise((resolve, reject) => {
            let matchingSince = null;
            let lastCorrected = 0;
            let done = false;
            let timer = null;
            let stop = () => {};
            const finish = (error, value) => {
                if (done) return;
                done = true;
                clearTimeout(timer);
                stop();
                if (error) reject(error);
                else resolve(value);
            };
            const check = () => {
                if (done) return;
                if (!player) return finish(null, false);
                const now = Date.now();
                if (now >= deadline) return finish(null, false);
                try {
                    if (matches()) {
                        matchingSince ??= now;
                        if (now - matchingSince >= stableMs) return finish(null, true);
                    } else {
                        matchingSince = null;
                        if (now - lastCorrected >= HOLD_RETRY_MS && correct()) lastCorrected = now;
                    }
                } catch (error) {
                    return finish(error);
                }
                const wait = matchingSince != null
                    ? stableMs - (now - matchingSince)
                    : HOLD_RETRY_MS - (now - lastCorrected);
                clearTimeout(timer);
                timer = setTimeout(check, Math.max(1, Math.min(wait, deadline - now)));
            };
            stop = watchPlayer(check);
            check();
        });
    }

    // ---------------------------------------------------------------------
    // Playback actions
    // ---------------------------------------------------------------------

    // Spotify refuses a pause while paused and a resume while playing
    // (verified live: "not_paused"), so only ask for a real change.
    async function setPlaying(isPlaying) {
        const state = player.getState();
        if (!state?.item || Boolean(state.isPaused) !== Boolean(isPlaying)) return;
        try {
            await (isPlaying ? player.resume() : player.pause());
        } catch (error) {
            if (Boolean(player.getState()?.isPaused) === Boolean(isPlaying)) throw error;
        }
        await waitFor(now => Boolean(now?.isPaused) !== Boolean(isPlaying), 2_000);
    }

    async function seek(positionMs) {
        if (!Number.isFinite(positionMs) || positionMs < 0) {
            throw new TypeError("positionMs must be a non-negative number");
        }
        const target = Math.round(positionMs);
        await player.seekTo(target);
        await waitFor(now => Math.abs(livePosition(readPlayer(now)) - target) <= SEEK_THRESHOLD_MS, 2_000);
    }

    /** Bring the current track to the session's position and play state. */
    async function alignPlayback(playback, calledAt) {
        if (!playback) return;
        const target = () => Math.max(0, (playback.positionMs ?? 0) +
            (playback.isPlaying ? Date.now() - calledAt : 0));
        if (Math.abs(livePosition(readPlayer()) - target()) > POSITION_TOLERANCE_MS) {
            await seek(target());
        }
        await setPlaying(Boolean(playback.isPlaying));
    }

    /**
     * Start `uri` at a position, playing or paused.
     *
     * Verified live: Spotify's internal player takes the start position and
     * "start paused" with the play request, so the track loads directly
     * into the session's state. (PlayerAPI.play drops "start paused".)
     * Playing a single track makes that track the whole context: nothing
     * follows it except what is queued. Queued items survive.
     */
    async function startTrack(uri, positionMs, isPlaying) {
        const before = player.getState()?.playbackId;
        const options = { initiallyPaused: !isPlaying };
        if (positionMs > POSITION_TOLERANCE_MS) options.seekTo = { value: BigInt(Math.round(positionMs)) };
        checkRpc(await requireContextPlayer().play({
            preparePlayRequest: {
                context: { uri, url: `context://${uri}` },
                options,
                playOrigin: { ...PLAY_ORIGIN, featureVersion: player._defaultFeatureVersion ?? "" },
            },
        }), "play a track");
        const started = await waitFor(state => state?.item?.uri === uri && state.playbackId !== before);
        if (!started) throw new Error("Spotify did not start the requested track");
    }

    /** Unload the current track and empty the queue (verified live). */
    async function unload() {
        if (!player.getState()?.item) return;
        checkRpc(await requireContextPlayer().stop({}), "stop playback");
        await waitFor(state => !state?.item);
    }

    // ---------------------------------------------------------------------
    // Queue writes
    // ---------------------------------------------------------------------
    //
    // Verified live: Spotify's queue is written as a whole. `setQueue`
    // takes the complete upcoming list and the revision it was based on,
    // and never touches the current track. Items are kept by passing them
    // back unchanged; new ones only need a URI and get a uid from Spotify.
    // An outdated revision is refused with error code 3.

    function newQueueItem(uri) {
        return {
            contextTrack: { uri, uid: "", metadata: { is_queued: "true" } },
            removed: [],
            blocked: [],
            provider: "queue",
        };
    }

    /**
     * Make `wanted` (`[{ entryId, uri }]`) Spotify's upcoming list, then
     * map the entry IDs to the uids Spotify gave the items.
     */
    async function writeUpcoming(wanted) {
        const uris = wanted.map(item => item.uri);
        const shown = () => readPlayer().upcoming;
        // When Spotify shows our write: the list starts with our songs (a
        // playlist it is playing may show more behind them), items we sent
        // as new have uids that didn't exist before, items we kept still
        // have theirs, and queued items we left out are gone. Context items
        // aren't checked for that: Spotify shows more of its playlist by
        // itself.
        //
        // The items themselves have to show it, not the revision. Verified
        // live: rewriting songs that were already queued as new items
        // showed the old items, with the old uids, for another 120 ms. The
        // queue's revision changed several times per write, once back to
        // the value it had before (revisions aren't counters), and before
        // the new items showed. Taking the uids early mapped the entries to
        // items that were about to go, and the new uids then came back as
        // the user's queue change.
        const writeShown = (sent, queue) => {
            const before = new Set([
                ...Array.from(queue.nextTracks ?? [], track => track.contextTrack?.uid),
                ...shown().map(item => item.uid),
            ].filter(Boolean));
            const keptUids = sent.map(track => track.contextTrack?.uid || null);
            const kept = new Set(keptUids.filter(Boolean));
            const dropped = new Set(Array.from(queue.nextTracks ?? [])
                .filter(track => track.provider === "queue")
                .map(track => track.contextTrack?.uid)
                .filter(uid => uid && !kept.has(uid)));
            return () => {
                const upcoming = shown();
                if (upcoming.length < uris.length) return false;
                const ours = uris.every((uri, index) => {
                    const item = upcoming[index];
                    if (item.uri !== uri) return false;
                    return keptUids[index] ? item.uid === keptUids[index] : !before.has(item.uid);
                });
                return ours && !upcoming.some(item => dropped.has(item.uid));
            };
        };

        const sameAsNow = () => {
            const upcoming = shown();
            return upcoming.length === wanted.length &&
                upcoming.every((item, index) => entryIdFor(item.uid) === wanted[index].entryId);
        };
        if (sameAsNow()) return;

        let lastError = null;
        for (let attempt = 0; attempt < QUEUE_WRITE_ATTEMPTS; attempt += 1) {
            const queue = internalQueue();
            if (!queue) throw new Error("Spotify's queue is unavailable");

            // Reuse Spotify's own items where the entry is already queued.
            const existing = new Map();
            for (const track of queue.nextTracks ?? []) {
                const uid = track.contextTrack?.uid;
                if (uid && entryIdByUid.has(uid)) existing.set(entryIdByUid.get(uid), track);
            }
            const nextTracks = wanted.map(item => {
                const track = existing.get(item.entryId);
                return track && track.contextTrack?.uri === item.uri ? track : newQueueItem(item.uri);
            });

            const result = await requireContextPlayer().setQueue({
                nextTracks,
                prevTracks: queue.prevTracks ?? [],
                queueRevision: queue.queueRevision,
            });
            if (!result?.error) {
                if (await waitFor(writeShown(nextTracks, queue))) {
                    shown().slice(0, wanted.length)
                        .forEach((item, index) => entryIdByUid.set(item.uid, wanted[index].entryId));
                    return;
                }
                lastError = new Error("Spotify did not show the new queue");
            } else {
                lastError = new Error(`Spotify refused the queue change (code ${result.error} ${result.reasons ?? ""})`.trim());
            }
            // Verified live: a write right after `play` is refused (code 3)
            // because the play changed the queue, and the queue shows its new
            // revision with one of the next updates (40–110 ms). Wait for
            // that, not for a timer (a 100 ms sleep took 330 ms with Spotify
            // hidden).
            await waitFor(() => internalQueue()?.queueRevision !== queue.queueRevision, 100 * (attempt + 1));
        }
        throw lastError;
    }

    /**
     * Load `entries` (current first) into Spotify.
     *
     * Resolves to `{ unavailableEntryIds }`: upcoming entries with no
     * Spotify match are left out and reported, the rest still plays. If
     * the *current* entry has no match, nothing is changed and a
     * `trackUnavailable` error is thrown.
     *
     * If the current entry is already playing it keeps playing: only the
     * upcoming list is rewritten.
     */
    async function loadQueue(entries, playback, calledAt = Date.now()) {
        if (!Array.isArray(entries)) throw new TypeError("Queue entries must be an array");
        if (entries.length === 0) {
            await unload();
            return { unavailableEntryIds: [] };
        }

        // The current entry first: if it can't play, nothing changes and the
        // rest isn't searched for. It starts before the rest is looked up,
        // which takes seconds for a long playlist (four searches at a time;
        // songs from other providers carry no ISRC). Starting a single track
        // keeps only queued items behind it, so nothing else plays from the
        // old list meanwhile.
        const view = readPlayer();
        const known = knownUris(view);
        const [current] = await resolveEntries([entries[0]], known);
        if (current.error) throw trackUnavailableError([current]);

        // Look the rest up while the song starts; the lookups don't touch
        // the queue.
        const lookups = resolveEntries(entries.slice(1), known);
        lookups.catch(() => {}); // awaited below; don't report it twice

        const now = readPlayer();
        const alreadyCurrent = now.current && !queueEndApplies(now) &&
            entryIdFor(now.current.uid) === current.entry.entryId;
        if (!alreadyCurrent) {
            const isPlaying = Boolean(playback?.isPlaying);
            const positionMs = Math.max(0, (playback?.positionMs ?? 0) + (isPlaying ? Date.now() - calledAt : 0));
            await startTrack(current.uri, positionMs, isPlaying);
            queueEnd = null;
            const started = readPlayer().current;
            if (started) entryIdByUid.set(started.uid, current.entry.entryId);
        }
        await alignPlayback(playback, calledAt);

        const upcoming = await lookups;
        const failures = upcoming.filter(result => result.error);
        const wanted = upcoming
            .filter(result => !result.error)
            .map(result => ({ entryId: result.entry.entryId, uri: result.uri }));
        // The song may have ended during the lookups; the session's own
        // advance follows then.
        const playing = readPlayer().current;
        if (playing && entryIdFor(playing.uid) === current.entry.entryId) {
            await writeUpcoming(wanted);
        }

        return { unavailableEntryIds: failures.map(failure => failure.entry.entryId) };
    }

    /** Skip to the next track and wait until it is the current one. */
    async function skipToNext() {
        const before = player.getState()?.playbackId;
        await player.skipToNext();
        await waitFor(state => state?.playbackId !== before && state?.item);
    }

    // ---------------------------------------------------------------------
    // Session operations
    // ---------------------------------------------------------------------

    function cloneEntries(queue) {
        return queue.map(entry => ({
            entryId: entry.entryId,
            track: { ...entry.track, artists: [...(entry.track.artists ?? [])] },
        }));
    }

    function applySnapshot(nextState) {
        if (!nextState || !Array.isArray(nextState.queue) || !nextState.playback) {
            throw new TypeError("Invalid Attune music state");
        }
        const calledAt = Date.now();
        return asRemoteChange(() => {
            const entries = nextState.queue;
            const start = nextState.currentEntryId == null
                ? 0
                : entries.findIndex(entry => entry.entryId === nextState.currentEntryId);
            if (start < 0) throw new Error(`Current queue entry was not found: ${nextState.currentEntryId}`);
            // Entries before the current one are history.
            return loadQueue(entries.slice(start), nextState.playback, calledAt);
        });
    }

    function applyOperation(operation) {
        if (!operation || typeof operation.type !== "string") {
            throw new TypeError("Invalid Attune operation");
        }
        const calledAt = Date.now();
        return asRemoteChange(() => applyOperationNow(operation, calledAt));
    }

    async function applyOperationNow(operation, calledAt) {
        const done = { unavailableEntryIds: [] };
        const state = getState();
        const queue = cloneEntries(state.queue);
        // A new current track starts from the beginning, playing.
        const fromStart = { isPlaying: true, positionMs: 0 };
        const indexOf = entryId => queue.findIndex(entry => entry.entryId === entryId);

        switch (operation.type) {
            case "queue.replace": {
                const entries = operation.entries ?? [];
                const currentEntryId = operation.currentEntryId ?? entries[0]?.entryId ?? null;
                const start = currentEntryId == null
                    ? 0
                    : entries.findIndex(entry => entry.entryId === currentEntryId);
                if (start < 0) throw new Error(`Current queue entry was not found: ${currentEntryId}`);
                const sameCurrent = currentEntryId != null && currentEntryId === state.currentEntryId;
                // Protocol 2: where the new current song is (a list may be
                // resumed part-way); without it, from 0, playing.
                const startAt = operation.playback
                    ? { isPlaying: Boolean(operation.playback.isPlaying), positionMs: operation.playback.positionMs ?? 0 }
                    : fromStart;
                return loadQueue(entries.slice(start), sameCurrent ? null : startAt, calledAt);
            }

            case "queue.insert": {
                const index = Math.max(0, Math.min(operation.index, queue.length));
                queue.splice(index, 0, operation.entry);
                return loadQueue(queue, index === 0 ? fromStart : null, calledAt);
            }

            case "queue.remove": {
                const index = indexOf(operation.entryId);
                if (index < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                queue.splice(index, 1);
                return loadQueue(queue, index === 0 ? { ...fromStart, isPlaying: state.playback.isPlaying } : null, calledAt);
            }

            case "queue.move": {
                const from = indexOf(operation.entryId);
                if (from < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                if (operation.toIndex < 0 || operation.toIndex >= queue.length) {
                    throw new RangeError("Queue index is out of range");
                }
                const [entry] = queue.splice(from, 1);
                queue.splice(operation.toIndex, 0, entry);
                const currentChanged = queue[0].entryId !== state.currentEntryId;
                return loadQueue(queue, currentChanged ? fromStart : null, calledAt);
            }

            case "playback.select":
            case "playback.advance": {
                const targetId = operation.entryId ?? operation.targetEntryId;
                const target = indexOf(targetId);
                if (target < 0) throw new Error(`Playback target is not in the queue: ${targetId}`);
                const wanted = {
                    isPlaying: operation.isPlaying ?? true,
                    positionMs: operation.positionMs ?? 0,
                };
                // Spotify may have moved on by itself already (a song end
                // reaches every client): then only line up the playback.
                if (target === 0) {
                    await alignPlayback(wanted, calledAt);
                    return done;
                }
                // Skipping keeps the context; drop what lies in between.
                if (target > 1) {
                    const known = knownUris();
                    await writeUpcoming(queue.slice(target).map(entry => ({
                        entryId: entry.entryId,
                        uri: known.get(entry.entryId),
                    })));
                }
                await skipToNext();
                queueEnd = null;
                if (readPlayer().current && getState().currentEntryId === targetId) {
                    await alignPlayback(wanted, calledAt);
                    return done;
                }
                // The skip went somewhere else: load the queue outright.
                return loadQueue(queue.slice(target), wanted, calledAt);
            }

            case "playback.seek": {
                if (operation.entryId !== state.currentEntryId) {
                    throw new Error("Cannot seek a track that is not currently playing");
                }
                await seek(operation.positionMs);
                return done;
            }

            case "playback.setPlaying": {
                // After the queue ran out Spotify sits on another track
                // (see `detectQueueEnd`); resuming would play that one.
                if (operation.isPlaying && queueEndApplies(readPlayer())) return done;
                await setPlaying(Boolean(operation.isPlaying));
                return done;
            }

            default:
                throw new Error(`Unsupported Attune operation: ${operation.type}`);
        }
    }

    // ---------------------------------------------------------------------
    // Neutral state
    // ---------------------------------------------------------------------

    /**
     * After attaching (which reloads Spotify), wait until it has finished
     * restoring itself, then pause and keep it paused, so every session has
     * a deliberate starting point.
     */
    function settle({ pause = true } = {}) {
        return asRemoteChange(async () => {
            const key = () => {
                const view = readPlayer();
                return JSON.stringify([view.current?.uid ?? null, view.upcoming.map(item => item.uid), view.isPaused]);
            };
            // Nothing loaded may be all there is, but give a restore longer.
            await waitUntilQuiet(
                key,
                () => (readPlayer().current ? SETTLE_STABLE_MS : SETTLE_EMPTY_MS),
                SETTLE_TIMEOUT_MS,
            );
            if (!pause) return;

            // Pause, and catch a late resume.
            await holdUntil({
                timeoutMs: CHANGE_TIMEOUT_MS,
                stableMs: SETTLE_PAUSED_MS,
                matches: () => readPlayer().isPaused,
                correct: () => {
                    setPlaying(false).catch(() => {});
                    return true;
                },
            });
        });
    }

    /**
     * Turn off what a session can't follow: shuffle and repeat (they change
     * what plays next behind Attune's back) and autoplay (it adds
     * recommendations after the queue). Returns the user's settings, for
     * `restoreAfterSession`.
     */
    function prepareForSession() {
        return asRemoteChange(async () => {
            const state = player.getState();
            const settings = getSettings();
            const previous = {
                shuffle: Boolean(state?.shuffle),
                repeat: Number.isInteger(state?.repeat) ? state.repeat : REPEAT_OFF,
                autoPlay: null,
            };
            try {
                const value = await settings?.autoPlay?.getValue?.();
                if (typeof value === "boolean") previous.autoPlay = value;
            } catch {
                // Unknown: leave autoplay alone.
            }

            // Spotify refuses these in some states (e.g. while it plays
            // recommendations); a refusal must not stop the session.
            const attempt = async action => {
                try {
                    await action();
                } catch (error) {
                    console.warn("[Attune] Spotify refused a setting change", error);
                }
            };
            if (previous.autoPlay === true) {
                await attempt(() => settings.autoPlay.setValue(false));
            }
            if (previous.repeat !== REPEAT_OFF) {
                await attempt(() => player.setRepeat(REPEAT_OFF));
                await waitFor(now => now?.repeat === REPEAT_OFF, 2_000);
            }
            if (previous.shuffle) {
                // Unshuffling puts the rest of the playlist back in order.
                await attempt(() => player.setShuffle(false));
                await waitFor(now => !now?.shuffle, 2_000);
            }
            // Spotify drops its recommendations a moment after autoplay is
            // switched off.
            if (previous.autoPlay === true) {
                await waitFor(now => !(now?.nextItems ?? []).some(item => item.provider === "autoplay"), 2_000);
            }
            return previous;
        });
    }

    /** Put back the settings `prepareForSession` returned. */
    function restoreAfterSession(previous) {
        if (!previous || typeof previous !== "object") return Promise.resolve();
        return asRemoteChange(async () => {
            const attempt = async action => {
                try {
                    await action();
                } catch (error) {
                    console.warn("[Attune] Spotify refused a setting change", error);
                }
            };
            if (previous.autoPlay === true) await attempt(() => getSettings()?.autoPlay?.setValue?.(true));
            if (Number.isInteger(previous.repeat) && previous.repeat !== REPEAT_OFF) {
                await attempt(() => player.setRepeat(previous.repeat));
            }
            if (previous.shuffle === true) await attempt(() => player.setShuffle(true));
        });
    }

    // ---------------------------------------------------------------------
    // Local state observation and operation emission
    // ---------------------------------------------------------------------

    // The last state local changes were compared against, and the Spotify
    // view it came from.
    let baseline = null;
    let baselineView = null;
    let eventTimer = null;
    let bufferingSince = null;
    let removeListeners = [];

    function rebase() {
        try {
            baselineView = readPlayer();
            baseline = stateFromView(baselineView);
        } catch {
            baseline = null;
            baselineView = null;
        }
    }

    const ids = queue => queue.map(entry => entry.entryId);
    const sameIds = (left, right) =>
        left.length === right.length && left.every((id, index) => id === right[index]);

    /**
     * Verified live: when the last track ends (autoplay off), Spotify goes
     * back to the track its context started with and pauses at 0. That is
     * not the user picking a song: the queue is simply over.
     */
    function detectQueueEnd(before, view) {
        if (!before || !baselineView?.current || !view.current) return false;
        if (baseline.queue.length !== 1 || baselineView.isPaused) return false;
        if (view.current.uid === baselineView.current.uid) return false;
        if (!view.isPaused || view.positionMs >= SEEK_THRESHOLD_MS || view.upcoming.length > 0) return false;
        const duration = baselineView.durationMs ?? baseline.queue[0].track.durationMs;
        if (!Number.isFinite(duration)) return false;
        return livePosition(baselineView) >= duration - 5_000;
    }

    /**
     * The queue edit that turns `before` into `after` (same current entry),
     * as operations. Small edits stay small so other clients don't reload
     * their queue.
     */
    function queueEditOperations(before, after) {
        const beforeIds = ids(before);
        const afterIds = ids(after);
        if (sameIds(beforeIds, afterIds)) return [];

        const added = after.filter(entry => !beforeIds.includes(entry.entryId));
        const removed = beforeIds.filter(id => !afterIds.includes(id));

        // Entries added (anywhere), nothing else changed.
        if (removed.length === 0 && added.length > 0 && added.length <= 20 &&
            sameIds(beforeIds, afterIds.filter(id => beforeIds.includes(id)))) {
            return added.map(entry => ({
                type: "queue.insert",
                entry,
                index: afterIds.indexOf(entry.entryId),
            }));
        }
        // Entries removed, nothing else changed.
        if (added.length === 0 && removed.length > 0 && removed.length <= 20 &&
            sameIds(afterIds, beforeIds.filter(id => afterIds.includes(id)))) {
            return removed.map(entryId => ({ type: "queue.remove", entryId }));
        }
        // One entry moved.
        if (added.length === 0 && removed.length === 0) {
            const first = beforeIds.findIndex((id, index) => id !== afterIds[index]);
            for (const moved of [beforeIds[first], afterIds[first]]) {
                if (sameIds(beforeIds.filter(id => id !== moved), afterIds.filter(id => id !== moved))) {
                    return [{ type: "queue.move", entryId: moved, toIndex: afterIds.indexOf(moved) }];
                }
            }
        }
        return null;
    }

    function positionWasSought(before, after) {
        if (before.currentEntryId !== after.currentEntryId) return false;
        if (before.playback.isPlaying !== after.playback.isPlaying) return false;
        const actual = after.playback.positionMs - before.playback.positionMs;
        const elapsed = after.playback.positionCapturedAtMs - before.playback.positionCapturedAtMs;
        const expected = before.playback.isPlaying ? Math.max(0, elapsed) : 0;
        return Math.abs(actual - expected) > SEEK_THRESHOLD_MS;
    }

    function emitLocalChanges() {
        if (applying > 0 || Date.now() < suppressLocalChangesUntil) {
            rebase();
            return;
        }

        let view;
        try {
            view = readPlayer();
        } catch (error) {
            sendEvent("Error", `Could not read Spotify's state: ${error.message}`);
            return;
        }
        // Not a track (an advertisement, a podcast): wait for music.
        if (player.getState()?.item && !view.current) return;

        // A playing track that is still buffering hasn't arrived yet.
        if (!view.isPaused && view.isBuffering) {
            bufferingSince ??= Date.now();
            if (Date.now() - bufferingSince < BUFFERING_WAIT_MS) {
                scheduleEmit();
                return;
            }
        }

        // Freshly queued items get their metadata a moment later.
        const undecorated = [view.current, ...view.upcoming].some(item => item && !item.track.title);
        if (undecorated && Date.now() - (bufferingSince ??= Date.now()) < BUFFERING_WAIT_MS) {
            scheduleEmit();
            return;
        }
        bufferingSince = null;

        const before = baseline;
        if (!before) {
            rebase();
            return;
        }

        if (!queueEnd && detectQueueEnd(before, view)) {
            queueEnd = {
                uid: view.current.uid,
                entry: before.queue[0],
                positionMs: baselineView.durationMs ?? before.queue[0].track.durationMs ?? 0,
            };
        } else if (queueEnd && !queueEndApplies(view)) {
            // Spotify left the track it returned to. If it's now playing
            // that track, it is a new occurrence of it.
            if (view.current?.uid === queueEnd.uid) entryIdByUid.delete(queueEnd.uid);
            queueEnd = null;
        }

        // Spotify reuses uids: playing one track twice gives the same uid
        // (verified live). A current track that didn't come from the queue
        // is a new occurrence.
        if (!queueEnd && view.current && baselineView &&
            view.current.uid !== baselineView.current?.uid &&
            !baselineView.upcoming.some(item => item.uid === view.current.uid)) {
            entryIdByUid.delete(view.current.uid);
        }

        const after = stateFromView(view);
        baseline = after;
        baselineView = view;

        const currentChanged = before.currentEntryId !== after.currentEntryId;
        let reported = false;

        if (currentChanged && after.currentEntryId && before.queue[1]?.entryId === after.currentEntryId) {
            // Moved on to the next entry. What changed besides is an edit of
            // the remaining queue (e.g. Spotify showing more of a playlist).
            const edits = queueEditOperations(before.queue.slice(1), after.queue);
            if (edits) {
                sendOperation({
                    type: "playback.advance",
                    targetEntryId: after.currentEntryId,
                    positionMs: after.playback.positionMs,
                    isPlaying: after.playback.isPlaying,
                });
                edits.forEach(sendOperation);
                reported = true;
            }
        } else if (!currentChanged) {
            const edits = queueEditOperations(before.queue, after.queue);
            if (edits) {
                edits.forEach(sendOperation);
                reported = true;
            }
        }
        if (!reported) {
            sendOperation({
                type: "queue.replace",
                entries: after.queue,
                currentEntryId: after.currentEntryId,
            });
        }

        if (before.playback.isPlaying !== after.playback.isPlaying) {
            sendOperation({ type: "playback.setPlaying", isPlaying: after.playback.isPlaying });
        }
        const sought = positionWasSought(before, after);
        if (sought) {
            sendOperation({
                type: "playback.seek",
                entryId: after.currentEntryId,
                positionMs: after.playback.positionMs,
            });
        }

        // Preserve the original event for the in-app diagnostics/UI.
        if (currentChanged || sought || before.playback.isPlaying !== after.playback.isPlaying) {
            sendEvent("PlaystateChange", getPlayState());
        }
    }

    function scheduleEmit() {
        clearTimeout(eventTimer);
        eventTimer = setTimeout(() => {
            eventTimer = null;
            try {
                emitLocalChanges();
            } catch (error) {
                sendEvent("Error", `Could not follow Spotify's state: ${error.message}`);
            }
        }, EVENT_DEBOUNCE_MS);
    }

    function subscribe() {
        const events = player.getEvents();
        rebase();
        removeListeners = ["update", "queue_update"].map(name => events.addListener(name, scheduleEmit));
    }

    // ---------------------------------------------------------------------
    // Legacy methods retained for the current Rust/UI call sites
    // ---------------------------------------------------------------------

    function getPlayState() {
        const state = player.getState();
        const view = readPlayer(state);
        const item = view.current ? state.item : null;
        return {
            track: item ? {
                ...view.current.track,
                provider: "Spotify",
                provider_track_id: item.uri,
                cover_url: imageUrl(item.metadata?.image_url ?? item.images?.[0]?.url),
            } : null,
            position_ms: view.positionMs,
            timestamp_ms: view.capturedAtMs,
            playing: !view.isPaused,
        };
    }

    async function playTrack(track) {
        const uri = await resolveTrack(track);
        await startTrack(uri, 0, true);
    }

    async function addToQueue(tracks) {
        const uris = await Promise.all(tracks.map(resolveTrack));
        await player.addToQueue(uris.map(uri => ({ uri, uid: null })));
    }

    // ---------------------------------------------------------------------
    // Lifecycle and public bridge API
    // ---------------------------------------------------------------------

    // The last events sent, for diagnosing live behaviour from the console.
    const recentEvents = [];

    function sendEvent(type, data) {
        const event = { type };
        if (data !== undefined) event.data = data;
        recentEvents.push({ at: Date.now(), ...event });
        if (recentEvents.length > RECENT_EVENT_LIMIT) recentEvents.shift();

        if (typeof window[EVENT_BINDING] !== "function") {
            console.warn("[Attune] CDP event binding is unavailable", type);
            return;
        }
        window[EVENT_BINDING](JSON.stringify(event));
    }

    function sendOperation(operation) {
        sendEvent("Operation", operation);
    }

    async function initialise() {
        player = await waitForPlayer();
        contextPlayer = player._contextPlayer ?? null;
        requireContextPlayer();
        subscribe();
        bridge.ready = true;
        sendEvent("Ready");
        console.info("[Attune] Spotify adapter ready");
    }

    async function dispose() {
        clearTimeout(eventTimer);
        eventTimer = null;
        removeListeners.forEach(remove => remove?.());
        removeListeners = [];
        player = null;
        contextPlayer = null;
        platform = null;
        baseline = null;
        baselineView = null;
        queueEnd = null;
        entryIdByUid.clear();
        resolvedUriCache.clear();
        bridge.ready = false;

        if (window[BRIDGE_KEY] === bridge) delete window[BRIDGE_KEY];
        if (window.bridge === bridge) delete window.bridge;
    }

    const legacy = action => asRemoteChange(action);

    const bridge = {
        version: BRIDGE_VERSION,
        ready: false,
        initialise,
        dispose,
        sendEvent,
        recentEvents,
        getState,
        getSnapshot: getState,
        applySnapshot,
        applyOperation,
        settle,
        prepareForSession,
        restoreAfterSession,

        // Existing controller commands.
        getPlayState,
        playTrack: track => legacy(() => playTrack(track)),
        pause: () => legacy(() => setPlaying(false)),
        resume: () => legacy(() => setPlaying(true)),
        seek: positionMs => legacy(() => seek(positionMs)),
        next: () => legacy(() => skipToNext()),
        previous: () => legacy(() => player.skipToPrevious()),
        addToQueue: tracks => legacy(() => addToQueue(tracks)),
        moveQueueItem: (fromIndex, toIndex) => {
            const entry = getState().queue[fromIndex];
            if (!entry) throw new RangeError("Queue index is out of range");
            return applyOperation({ type: "queue.move", entryId: entry.entryId, toIndex });
        },
        removeFromQueue: index => {
            const entry = getState().queue[index];
            if (!entry) throw new RangeError("Queue index is out of range");
            return applyOperation({ type: "queue.remove", entryId: entry.entryId });
        },
    };

    window[BRIDGE_KEY] = bridge;
    window.bridge = bridge;

    try {
        await initialise();
    } catch (error) {
        console.error("[Attune] Spotify adapter initialization failed", error);
        sendEvent("Error", error?.message ?? String(error));
    }

    return bridge;
})();
