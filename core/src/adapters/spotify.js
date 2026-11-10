(async () => {
    window.__jamBootstrap = true;
    window.bridge = {};

    const bridge = {
        // ---------------------------------------------------------------------
        // Cached Spotify objects
        // ---------------------------------------------------------------------

        platform: null,
        services: new Map(),
        searchRequest: null,
        searchOperations: new Map(),

        playerAPI: null,

        // ---------------------------------------------------------------------
        // Lifecycle state
        // ---------------------------------------------------------------------

        removeActionListener: null,
        removeUpdateListener: null,
        initialized: false,
        lastState: null,
        pendingAction: null,
        searchFunction: null,
        sessionEntryIds: new Map(),
        suppressSessionEventsUntil: 0,
        lastSessionState: null,
        searchOperation: null,

        // ---------------------------------------------------------------------
        // Initialisation
        // ---------------------------------------------------------------------

        async initialise() {
            if (this.initialized) {
                this.sendEvent("Ready");
                return true;
            }

            try {
                console.log("[Jam] Waiting for Spotify PlayerAPI...");

                this.playerAPI = await this.waitForPlayerAPI();

                this.attachEventListeners();

                this.initialized = true;

                this.sendEvent("Ready");

                console.log("[Jam] Initialised");

                return true;
            } catch (error) {
                this.sendEvent("Error", error?.message ?? String(error));
                console.error("[Jam] Initialisation failed:", error);
                throw error;
            }
        },

        // ---------------------------------------------------------------------
        // Spotify platform discovery
        // ---------------------------------------------------------------------

        getPlatform() {
            if (this.platform) {
                return this.platform;
            }

            for (const element of document.querySelectorAll("*")) {
                const fiberKey = Object.keys(element).find(
                    key => key.startsWith("__reactFiber")
                );

                if (!fiberKey) {
                    continue;
                }

                let fiber = element[fiberKey];

                while (fiber) {
                    const props = fiber.memoizedProps;

                    if (
                        props &&
                        typeof props === "object" &&
                        props.platform &&
                        typeof props.platform.getRegistry === "function"
                    ) {
                        this.platform = props.platform;
                        console.log("[Jam] Spotify platform found");
                        return this.platform;
                    }

                    fiber = fiber.return;
                }
            }

            throw new Error("[Jam] Spotify platform not found");
        },

        // ---------------------------------------------------------------------
        // Spotify registry
        // ---------------------------------------------------------------------

        getService(name) {
            if (this.services.has(name)) {
                return this.services.get(name);
            }

            const registry = this.getPlatform().getRegistry();

            // _map is Spotify's current registry implementation.
            // Keep this detail isolated to this function.
            if (!registry?._map) {
                throw new Error("[Jam] Spotify registry unavailable");
            }

            for (const [key, entry] of registry._map) {
                if (key.description !== name) {
                    continue;
                }

                const service =
                    entry?.instance ??
                    (typeof entry?.factory === "function"
                        ? entry.factory()
                        : null);

                if (!service) {
                    throw new Error(
                        `[Jam] Spotify service "${name}" unavailable`
                    );
                }

                this.services.set(name, service);
                return service;
            }

            throw new Error(
                `[Jam] Spotify service "${name}" not found`
            );
        },

        getPlayerAPI() {
            if (this.playerAPI) {
                return this.playerAPI;
            }

            // Preferred path.
            try {
                this.playerAPI = this.getService("PlayerAPI");
                console.log("[Jam] PlayerAPI found via registry");
                return this.playerAPI;
            } catch (error) {
                console.warn(
                    "[Jam] Registry PlayerAPI unavailable, using Fiber fallback",
                    error
                );
            }

            // Fallback path.
            for (const element of document.querySelectorAll("*")) {
                const fiberKey = Object.keys(element).find(
                    key => key.startsWith("__reactFiber")
                );

                if (!fiberKey) {
                    continue;
                }

                let fiber = element[fiberKey];

                while (fiber) {
                    const props = fiber.memoizedProps;

                    if (
                        props &&
                        typeof props === "object" &&
                        props.playerAPI &&
                        typeof props.playerAPI.getState === "function" &&
                        typeof props.playerAPI.play === "function"
                    ) {
                        this.playerAPI = props.playerAPI;
                        console.log("[Jam] PlayerAPI found via Fiber");
                        return this.playerAPI;
                    }

                    fiber = fiber.return;
                }
            }

            throw new Error("[Jam] Spotify PlayerAPI not found");
        },

        // ---------------------------------------------------------------------
        // Search request discovery
        // ---------------------------------------------------------------------

        getSearchRequest() {
            if (this.searchRequest) {
                return this.searchRequest;
            }

            for (const element of document.querySelectorAll("*")) {
                const fiberKey = Object.keys(element).find(
                    key => key.startsWith("__reactFiber")
                );

                if (!fiberKey) continue;

                let fiber = element[fiberKey];

                while (fiber) {
                    if (fiber.tag === 10) {
                        const value = fiber.memoizedProps?.value;
                        const request = value?.request;

                        if (
                            value &&
                            typeof value === "object" &&
                            typeof request === "function" &&
                            request.length >= 2 &&
                            !request.toString().includes(
                                "GraphQL Context not initialized"
                            )
                        ) {
                            this.searchRequest = request;
                            this.searchContext = fiber.type;

                            console.log("[Jam] Live SearchContext found");

                            return request;
                        }
                    }

                    fiber = fiber.return;
                }
            }

            throw new Error("[Jam] Live Spotify SearchContext not found");
        },

        async getSearchFunction(timeoutMs = 10000) {
            // -------------------------------------------------------------
            // Already initialized
            // -------------------------------------------------------------

            if (this.searchFunction) {
                return this.searchFunction;
            }

            const operationName = "searchTopResultsList";

            // -------------------------------------------------------------
            // Helper: search the loaded webpack modules for the operation
            // -------------------------------------------------------------

            const findOperation = () => {
                const modules = window.__webpack_modules__;

                if (!modules) {
                    return null;
                }

                const pattern = new RegExp(
                    `"${operationName}"\\s*,\\s*"query"\\s*,\\s*"([a-f0-9]{64})"`
                );

                for (const module of Object.values(modules)) {
                    let source;

                    try {
                        source = module.toString();
                    } catch {
                        continue;
                    }

                    const match = source.match(pattern);

                    if (!match) {
                        continue;
                    }

                    return {
                        name: operationName,
                        operation: "query",
                        sha256Hash: match[1],
                        value: null,
                    };
                }

                return null;
            };

            // -------------------------------------------------------------
            // Helper: turn an operation + variables into the actual
            // Spotify SearchContext request
            // -------------------------------------------------------------

            const buildSearchFunction = operation => {
                const request = this.getSearchRequest();

                return async (query, limit = 20) => {
                    const variables = {
                        query: query.trim(),

                        limit,
                        offset: 0,
                        numberOfTopResults: limit,

                        includeArtistHasConcertsField: false,
                        includeLocalConcertsField: false,

                        includeAudiobooks: true,
                        includeAuthors: true,
                        includePreReleases: true,
                        includeAlbumPreReleases: false,
                        includeEpisodeContentRatingsV2: true,

                        isPrefix: null,

                        sectionFilters: [
                            "GENERIC",
                            "VIDEO_CONTENT",
                        ],
                    };

                    return request(
                        operation,
                        variables
                    );
                };
            };

            // -------------------------------------------------------------
            // Operation already loaded?
            // -------------------------------------------------------------

            let operation = findOperation();

            if (operation) {
                this.searchOperation = operation;
                this.searchFunction = buildSearchFunction(operation);

                console.log("[Jam] Search function initialized");

                return this.searchFunction;
            }

            // -------------------------------------------------------------
            // Warm up Spotify's search implementation
            // -------------------------------------------------------------

            const input = [...document.querySelectorAll("input")]
                .find(
                    el =>
                        el.getAttribute("aria-label") ===
                        "What do you want to play?"
                );

            if (!input) {
                throw new Error(
                    "[Jam] Spotify global search input not found"
                );
            }

            const setter = Object.getOwnPropertyDescriptor(
                HTMLInputElement.prototype,
                "value"
            )?.set;

            if (!setter) {
                throw new Error(
                    "[Jam] Could not access HTMLInputElement.value setter"
                );
            }

            const warmupQuery = " ";

            // Populate Spotify's controlled input using the native setter.
            setter.call(input, warmupQuery);

            // Let React receive a genuine browser event.
            input.dispatchEvent(
                new Event("input", {
                    bubbles: true,
                    composed: true,
                })
            );

            input.dispatchEvent(
                new Event("change", {
                    bubbles: true,
                    composed: true,
                })
            );

            input.focus();

            // Trigger Spotify's normal search submission path.
            input.dispatchEvent(
                new KeyboardEvent("keydown", {
                    key: "Enter",
                    code: "Enter",
                    keyCode: 13,
                    which: 13,
                    bubbles: true,
                    cancelable: true,
                    composed: true,
                })
            );

            // -------------------------------------------------------------
            // Wait for Spotify to load the search operation
            // -------------------------------------------------------------

            const deadline = performance.now() + timeoutMs;

            while (performance.now() < deadline) {
                operation = findOperation();

                if (operation) {
                    break;
                }

                await new Promise(resolve => setTimeout(resolve, 100));
            }

            if (!operation) {
                throw new Error(
                    `[Jam] Spotify GraphQL operation "${operationName}" failed to load`
                );
            }

            this.searchOperation = operation;

            console.log(
                `[Jam] Search operation found: ${operation.sha256Hash}`
            );

            // -------------------------------------------------------------
            // Return Spotify to Home
            // -------------------------------------------------------------

            const homeButton = [...document.querySelectorAll("button")]
                .find(
                    el =>
                        el.getAttribute("aria-label") ===
                        "Home"
                );

            homeButton?.click();

            // -------------------------------------------------------------
            // Build/cache the actual search function
            // -------------------------------------------------------------

            this.searchFunction = buildSearchFunction(operation);

            console.log("[Jam] Search function initialized");

            return this.searchFunction;
        },

        // ---------------------------------------------------------------------
        // Player readiness
        // ---------------------------------------------------------------------

        async waitForPlayerAPI(timeout_ms = 30000) {
            const start = performance.now();

            while (performance.now() - start < timeout_ms) {
                try {
                    const player = this.getPlayerAPI();

                    if (player) {
                        return player;
                    }
                } catch {
                    // Spotify has not created the PlayerAPI yet.
                }

                await new Promise(resolve => setTimeout(resolve, 50));
            }

            throw new Error("[Jam] Timed out waiting for Spotify PlayerAPI");
        },

        // ---------------------------------------------------------------------
        // Spotify player events
        // ---------------------------------------------------------------------

        attachEventListeners() {
            if (
                this.removeActionListener ||
                this.removeUpdateListener
            ) {
                return;
            }

            const events = this.getPlayerAPI().getEvents();

            this.removeActionListener = events.addListener(
                "action",
                event => this.spotifyActionEventHandler(event)
            );

            this.removeUpdateListener = events.addListener(
                "update",
                event => this.spotifyUpdateEventHandler(event)
            );

            console.log("[Jam] Event listeners added");
        },

        spotifyActionEventHandler(event) {
            // console.log(
            //     "[Jam] Spotify action:",
            //     event.data
            // );

            this.pendingAction = event.data.action;
        },

        spotifyUpdateEventHandler(event) {
            // console.log(
            //     "[Jam] Spotify update:",
            //     event.data
            // );

            const currentState = event.data;

            if (currentState.isBuffering) {
                return;
            }

            const previousState = this.lastState;
            this.lastState = currentState;

            this.emitSessionOperations(previousState, currentState);

            const currentPlaystate = this.getPlayState(currentState);
            const previousPlaystate = this.getPlayState(previousState);

            const currentQueue = this.getQueueState(currentState);
            const previousQueue = this.getQueueState(previousState);

            if (!previousState) {
                this.sendEvent("PlaystateChange", currentPlaystate);
                this.sendEvent("QueueChange", currentQueue);
                return;
            }

            const trackChanged = previousPlaystate.track?.provider_track_id !== currentPlaystate.track?.provider_track_id;
            const playbackChanged = previousPlaystate.playing !== currentPlaystate.playing;

            const timeDelta = currentState.timestamp - previousState.timestamp;
            const seeked = Math.abs(previousPlaystate.position_ms + timeDelta - currentPlaystate.position_ms) > 500;

            if (trackChanged || playbackChanged && this.pendingAction) {
                this.sendEvent("PlaystateChange", currentPlaystate);
                this.pendingAction = null;
            }

            if (!trackChanged && seeked) {
                this.sendEvent("PlaystateChange", currentPlaystate);
            }

            const previousQueueTrackIds = previousQueue?.queue.map(t => t.provider_track_id) ?? [];
            const currentQueueTrackIds = currentQueue?.queue.map(t => t.provider_track_id) ?? [];

            if (JSON.stringify(previousQueueTrackIds) !== JSON.stringify((currentQueueTrackIds))) {
                this.sendEvent("QueueChange", currentQueue);
            }
        },

        removeEventListeners() {
            this.removeActionListener?.();
            this.removeUpdateListener?.();

            this.removeActionListener = null;
            this.removeUpdateListener = null;

            console.log("[Jam] Event listeners removed");
        },

        // ---------------------------------------------------------------------
        // Playback state
        // ---------------------------------------------------------------------

        getPlayState(state = null) {
            state ??= this.getPlayerAPI().getState();

            const item = state?.item;

            return {
                track: item ? {
                    title: item.name ?? item.title ?? "",
                    artists: item.artists?.map(
                        artist => typeof artist === "string" ? artist : artist?.name ?? artist?.profile?.name
                    ).filter(Boolean) ?? [],
                    isrc: item.isrc ?? null,
                    provider: "Spotify",
                    provider_track_id: item.uri ?? item.id ?? null,
                    cover_url: item.metadata?.image_url
                        ? `https://i.scdn.co/image/${item.metadata.image_url.split(":")[2]}`
                        : null,
                } : null,
                position_ms: state?.positionAsOfTimestamp ?? 0,
                timestamp_ms: state?.timestamp ?? Date.now(),
                playing: Boolean(state && !state.isPaused),
            };
        },

        // ---------------------------------------------------------------------
        // Queue state
        // ---------------------------------------------------------------------

        getQueueState(state = null) {
            state ??= this.getPlayerAPI().getState();

            const items = state?.nextItems ?? [];

            return {
                queue: (items ?? [])
                    .filter(item => item.provider === "queue")
                    .map(item => ({
                        title: item.name,
                        artists: item.artists?.map(
                            artist => artist.name
                        ) ?? [],
                        isrc: null, // todo?
                        provider: "Spotify",
                        provider_track_id: item.uri,
                        cover_url: item.metadata?.image_url
                            ? `https://i.scdn.co/image/${item.metadata.image_url.split(":")[2]}`
                            : null,
                    }))
            };
        },

        async addToQueue(tracks) {
            const items = await Promise.all(tracks.map(async track => ({
                uri: await this.getTrackUri(track),
                uid: null,
            })));
            return this.getPlayerAPI().addToQueue(items);
        },

        async removeFromQueue(tracks) {
            const items = await Promise.all(tracks.map(async track => ({
                uri: await this.getTrackUri(track),
                uid: track.uid ?? null,
            })));
            return this.getPlayerAPI().removeFromQueue(items, true);
        },

        async reorderQueue(tracks, afterTrack) {
            const items = await Promise.all(tracks.map(async track => ({
                uri: await this.getTrackUri(track),
                uid: null,
            })));

            const position = {};
            if (afterTrack?.length) {
                position.after = await Promise.all(afterTrack.map(async track => ({
                    uri: await this.getTrackUri(track),
                    uid: null,
                })));
            } else {
                const queue = await this.getQueue();
                const firstInQueue = queue?.queued?.[0];
                if (!firstInQueue) {
                    throw new Error("reorderQueue failed: queue is empty");
                }
                position.before = {
                    uri: firstInQueue.uri,
                    uid: firstInQueue.uid,
                };
            }

            return this.getPlayerAPI().reorderQueue(items, position);
        },

        async getQueue() {
            return this.getPlayerAPI().getQueue();
        },

        async insertIntoQueue(tracks, afterTrack) {
            const items = await Promise.all(tracks.map(async track => ({
                uri: await this.getTrackUri(track),
                uid: null,
            })));

            const position = {};
            if (afterTrack?.length) {
                position.after = await Promise.all(afterTrack.map(async track => ({
                    uri: await this.getTrackUri(track),
                    uid: null,
                })));
            } else {
                const queue = await this.getQueue();
                const firstInQueue = queue?.queued?.[0];
                if (!firstInQueue) {
                    throw new Error("insertIntoQueue failed: queue is empty");
                }
                position.before = {
                    uri: firstInQueue.uri,
                    uid: firstInQueue.uid,
                };
            }

            return this.getPlayerAPI().insertIntoQueue(items, position, {
                interactionId: null,
            });
        },

        async clearQueue() {
            return this.getPlayerAPI().clearQueue();
        },

        // ---------------------------------------------------------------------
        // Search
        // ---------------------------------------------------------------------

        async searchTrack(trackRef) {

            const query = `${trackRef.title} ${trackRef.artists.join(" ")}`;
            const searchFn = await this.getSearchFunction();
            const result = await searchFn(query, 10);

            for (const candidate of result.data.searchV2.topResultsV2.itemsV2) {

                const data = candidate.item.data;

                if (data.__typename !== "Track") {
                    continue;
                }

                const trackRes = {
                    title: data.name,
                    artists: data.artists.items.map((artist) => artist.profile?.name).filter(Boolean),
                    isrc: null,
                    provider_track_id: data.id,
                    provider: "Spotify",
                    cover_url: null,
                };

                if (trackRes.title && trackRef.title &&
                    this.normalize(trackRes.title) == this.normalize(trackRef.title) &&
                    trackRes.artists
                        .map(this.normalize)
                        .some(candidateArtist =>
                            trackRef.artists
                                .map(this.normalize)
                                .some(sourceArtist =>
                                    candidateArtist &&
                                    sourceArtist &&
                                    candidateArtist === sourceArtist
                                )
                        )
                ) {
                    return trackRes;
                }
            }

            throw new Error(
                "Failed to find track"
            )
        },

        normalize(value) {
            return String(value ?? "")
                .normalize("NFKC")
                .toLowerCase()
                .replace(/[’‘]/g, "'")
                .replace(/["""']/g, "")
                .replace(/[^\p{L}\p{N}]+/gu, " ")
                .trim()
                .replace(/\s+/g, " ");
        },

        async getTrackUri(trackRef) {
            if (trackRef.provider === "Spotify" && trackRef.provider_track_id) {
                const id = trackRef.provider_track_id;
                return id.startsWith("spotify:track:") ? id : `spotify:track:${id}`;
            }

            const trackRes = await this.searchTrack(trackRef);
            const uri = trackRes.provider_track_id;
            return uri.startsWith("spotify:track:") ? uri : `spotify:track:${uri}`;
        },

        // ---------------------------------------------------------------------
        // Playback control
        // ---------------------------------------------------------------------

        async playTrack(trackRef) {

            let track_uri = null;

            track_uri = await this.getTrackUri(trackRef);

            if (
                typeof track_uri !== "string" ||
                !track_uri.startsWith("spotify:track:")
            ) {
                throw new TypeError(
                    "[Jam] Invalid Spotify track URI"
                );
            }

            return this.getPlayerAPI().play(
                { track_uri },
                {
                    featureIdentifier: "jam",
                    referrerIdentifier: "jam",
                },
                {}
            );
        },

        async pause() {
            return this.getPlayerAPI().pause();
        },

        async resume() {
            return this.getPlayerAPI().resume();
        },

        async seek(position_ms) {
            if (!Number.isFinite(position_ms)) {
                throw new TypeError(
                    "[Jam] position_ms must be a finite number"
                );
            }

            return this.getPlayerAPI().seekTo(position_ms);
        },

        // ---------------------------------------------------------------------
        // Provider-independent MyJam session protocol
        // ---------------------------------------------------------------------

        getSessionTrack(item) {
            if (!item) return null;
            return {
                title: item.name ?? item.title ?? "",
                artists: item.artists?.map(artist =>
                    typeof artist === "string" ? artist : artist?.name ?? artist?.profile?.name
                ).filter(Boolean) ?? [],
                isrc: item.isrc ?? null,
                album: item.album?.name ?? item.album?.title ?? null,
                durationMs: item.duration_ms ?? item.durationMs ?? null,
            };
        },

        getSessionEntryId(item, index, isCurrent = false) {
            const uri = item?.uri ?? item?.id ?? `unknown-${index}`;
            const key = item?.uid != null
                ? `uid:${item.uid}`
                : `${isCurrent ? "current" : "queue"}:${uri}:${index}`;

            if (!this.sessionEntryIds.has(key)) {
                this.sessionEntryIds.set(key, crypto.randomUUID());
            }
            return this.sessionEntryIds.get(key);
        },

        getState(state = null) {
            state ??= this.getPlayerAPI().getState();

            const upcomingItems = (state?.nextItems ?? [])
                .filter(item => item.provider === "queue");
            const queue = [];

            if (state?.item) {
                queue.push({
                    entryId: this.getSessionEntryId(state.item, 0, true),
                    track: this.getSessionTrack(state.item),
                });
            }

            upcomingItems.forEach((item, index) => {
                queue.push({
                    entryId: this.getSessionEntryId(item, index + 1),
                    track: this.getSessionTrack(item),
                });
            });

            return {
                queue,
                currentEntryId: queue[0]?.entryId ?? null,
                playback: {
                    isPlaying: Boolean(state && !state.isPaused),
                    positionMs: Math.max(0, Math.round(state?.positionAsOfTimestamp ?? 0)),
                    positionCapturedAtMs: state?.timestamp ?? Date.now(),
                },
            };
        },

        setRemoteMutationWindow() {
            this.suppressSessionEventsUntil = Date.now() + 1200;
        },

        async replaceSessionQueue(nextState) {
            if (!Array.isArray(nextState.queue)) {
                throw new TypeError("Queue entries must be an array");
            }

            const entries = [...nextState.queue];
            const currentIndex = nextState.currentEntryId == null
                ? 0
                : entries.findIndex(entry => entry.entryId === nextState.currentEntryId);
            if (currentIndex < 0) {
                throw new Error(`Current queue entry was not found: ${nextState.currentEntryId}`);
            }
            const startIndex = currentIndex;
            // Entries before the selected item are history, not upcoming queue items.
            const ordered = entries.slice(startIndex);

            // Resolve the complete queue first. A search failure must not clear
            // the user's currently playing queue as a side effect.
            const uris = await Promise.all(ordered.map(entry => this.getTrackUri(entry.track)));

            if (!uris.length && this.getPlayerAPI().getState()?.item) {
                // PlayerAPI.clearQueue() removes upcoming entries, but we have
                // not verified a safe action for unloading the active track.
                throw new Error("Spotify cannot safely unload the active track with the verified PlayerAPI actions");
            }

            this.setRemoteMutationWindow();
            await this.clearQueue();

            if (uris.length) {
                await this.getPlayerAPI().play(
                    { track_uri: uris[0] },
                    { featureIdentifier: "jam", referrerIdentifier: "jam" },
                    {},
                );
                if (uris.length > 1) {
                    await this.getPlayerAPI().addToQueue(
                        uris.slice(1).map(uri => ({ uri, uid: null }))
                    );
                }
            } else {
                await this.pause();
            }

            if (nextState.playback?.positionMs > 0) {
                await this.seek(nextState.playback.positionMs);
            }
            if (nextState.playback && !nextState.playback.isPlaying) {
                await this.pause();
            }

            // Associate newly generated Spotify queue UIDs with MyJam entry IDs.
            try {
                const liveState = this.getPlayerAPI().getState();
                const currentItem = liveState?.item;
                if (currentItem) {
                    const currentKey = currentItem.uid != null
                        ? `uid:${currentItem.uid}`
                        : `current:${currentItem.uri ?? currentItem.id}:0`;
                    this.sessionEntryIds.set(currentKey, ordered[0]?.entryId);
                }

                const liveQueue = await this.getQueue();
                (liveQueue?.queued ?? []).forEach((item, index) => {
                    const entry = ordered[index + 1];
                    if (entry && item.uid != null) {
                        this.sessionEntryIds.set(`uid:${item.uid}`, entry.entryId);
                    }
                });
            } catch (error) {
                console.warn("[MyJam] Could not refresh Spotify queue entry IDs", error);
            }
        },

        async applySnapshot(nextState) {
            if (!nextState || !Array.isArray(nextState.queue) || !nextState.playback) {
                throw new TypeError("Invalid MyJam music state");
            }
            return this.replaceSessionQueue(nextState);
        },

        async applyOperation(operation) {
            if (!operation || typeof operation.type !== "string") {
                throw new TypeError("Invalid MyJam operation");
            }

            const state = this.getState();
            const nextState = {
                queue: state.queue.map(entry => ({
                    entryId: entry.entryId,
                    track: { ...entry.track, artists: [...entry.track.artists] },
                })),
                currentEntryId: state.currentEntryId,
                playback: { ...state.playback },
            };

            switch (operation.type) {
                case "queue.replace": {
                    nextState.queue = operation.entries;
                    nextState.currentEntryId = operation.currentEntryId ?? operation.entries[0]?.entryId ?? null;
                    if (nextState.currentEntryId !== state.currentEntryId) {
                        nextState.playback.positionMs = 0;
                        nextState.playback.isPlaying = true;
                    }
                    break;
                }

                case "queue.insert": {
                    const index = Math.max(0, Math.min(operation.index, nextState.queue.length));
                    nextState.queue.splice(index, 0, operation.entry);
                    if (!nextState.currentEntryId || index === 0) {
                        nextState.currentEntryId = operation.entry.entryId;
                        nextState.playback.positionMs = 0;
                    }
                    break;
                }

                case "queue.remove": {
                    const index = nextState.queue.findIndex(entry => entry.entryId === operation.entryId);
                    if (index < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                    nextState.queue.splice(index, 1);
                    if (nextState.currentEntryId === operation.entryId) {
                        nextState.currentEntryId = nextState.queue[0]?.entryId ?? null;
                        nextState.playback.positionMs = 0;
                    }
                    break;
                }

                case "queue.move": {
                    const fromIndex = nextState.queue.findIndex(entry => entry.entryId === operation.entryId);
                    if (fromIndex < 0) throw new Error(`Queue entry not found: ${operation.entryId}`);
                    if (operation.toIndex < 0 || operation.toIndex >= nextState.queue.length) {
                        throw new RangeError("Queue index is out of range");
                    }
                    if (nextState.currentEntryId === operation.entryId && operation.toIndex !== 0) {
                        throw new Error("Cannot move the currently playing entry behind upcoming tracks");
                    }
                    const [entry] = nextState.queue.splice(fromIndex, 1);
                    nextState.queue.splice(operation.toIndex, 0, entry);
                    if (operation.toIndex === 0) {
                        nextState.currentEntryId = operation.entryId;
                        nextState.playback.positionMs = 0;
                    }
                    break;
                }

                case "playback.select":
                case "playback.advance": {
                    const entryId = operation.entryId ?? operation.targetEntryId;
                    if (!nextState.queue.some(entry => entry.entryId === entryId)) {
                        throw new Error(`Playback target not found: ${entryId}`);
                    }
                    const targetIndex = nextState.queue.findIndex(entry => entry.entryId === entryId);
                    nextState.queue = nextState.queue.slice(targetIndex);
                    nextState.currentEntryId = entryId;
                    nextState.playback.positionMs = operation.positionMs ?? 0;
                    nextState.playback.isPlaying = operation.isPlaying ?? true;
                    break;
                }

                case "playback.seek":
                    if (operation.entryId !== nextState.currentEntryId) {
                        throw new Error("Cannot seek a track that is not currently playing");
                    }
                    this.setRemoteMutationWindow();
                    return this.seek(operation.positionMs);

                case "playback.setPlaying":
                    this.setRemoteMutationWindow();
                    return operation.isPlaying ? this.resume() : this.pause();

                default:
                    throw new Error(`Unsupported MyJam operation: ${operation.type}`);
            }

            return this.replaceSessionQueue(nextState);
        },

        sameSessionTrack(left, right) {
            if (!left || !right) return false;
            if (this.normalize(left.title) !== this.normalize(right.title)) return false;

            const leftArtists = new Set((left.artists ?? []).map(this.normalize));
            const sharesArtist = (right.artists ?? []).some(artist =>
                leftArtists.has(this.normalize(artist))
            );
            if (!sharesArtist) return false;

            if (left.isrc && right.isrc) {
                return this.normalize(left.isrc) === this.normalize(right.isrc);
            }
            return true;
        },

        emitSessionOperations(previousState, currentState) {
            if (!previousState) {
                this.lastSessionState = this.getState(currentState);
                return;
            }
            if (Date.now() < this.suppressSessionEventsUntil) {
                this.lastSessionState = this.getState(currentState);
                return;
            }

            // Keep the previous normalized state rather than rebuilding it:
            // Spotify can omit the queue UID from the now-playing item when
            // an upcoming item becomes current. Match the descriptor to carry
            // its queue-entry identity across that transition.
            const before = this.lastSessionState ?? this.getState(previousState);
            const after = this.getState(currentState);
            if (before.currentEntryId !== after.currentEntryId &&
                before.queue[1] && after.queue[0] &&
                this.sameSessionTrack(before.queue[1].track, after.queue[0].track)) {
                after.queue[0].entryId = before.queue[1].entryId;
                after.currentEntryId = after.queue[0].entryId;

                // Persist the remapped ID under the now-playing item's key so
                // the next PlayerAPI update does not generate a fresh ID.
                const currentItem = currentState?.item;
                const currentKey = currentItem?.uid != null
                    ? `uid:${currentItem.uid}`
                    : `current:${currentItem?.uri ?? currentItem?.id ?? "unknown-0"}:0`;
                this.sessionEntryIds.set(currentKey, after.currentEntryId);
            }
            const currentChanged = before.currentEntryId !== after.currentEntryId;
            const advancedToNextEntry = currentChanged &&
                before.queue[1]?.entryId === after.currentEntryId &&
                before.queue.slice(2).every((entry, index) => entry.entryId === after.queue[index + 1]?.entryId) &&
                before.queue.length - 2 === after.queue.length - 1;
            const queueChanged = !advancedToNextEntry && (
                before.queue.length !== after.queue.length ||
                before.queue.some((entry, index) => entry.entryId !== after.queue[index]?.entryId)
            );

            if (queueChanged) {
                this.sendEvent("Operation", {
                    type: "queue.replace",
                    entries: after.queue,
                    currentEntryId: after.currentEntryId,
                });
            } else if (before.currentEntryId !== after.currentEntryId && after.currentEntryId) {
                this.sendEvent("Operation", {
                    type: "playback.advance",
                    targetEntryId: after.currentEntryId,
                    positionMs: after.playback.positionMs,
                    isPlaying: after.playback.isPlaying,
                });
            }

            if (before.playback.isPlaying !== after.playback.isPlaying) {
                this.sendEvent("Operation", {
                    type: "playback.setPlaying",
                    isPlaying: after.playback.isPlaying,
                });
            }

            const elapsed = after.playback.positionCapturedAtMs - before.playback.positionCapturedAtMs;
            const expected = before.playback.isPlaying ? Math.max(0, elapsed) : 0;
            const actual = after.playback.positionMs - before.playback.positionMs;
            if (before.currentEntryId === after.currentEntryId &&
                Math.abs(actual - expected) > 1200) {
                this.sendEvent("Operation", {
                    type: "playback.seek",
                    entryId: after.currentEntryId,
                    positionMs: after.playback.positionMs,
                });
            }

            this.lastSessionState = after;
        },

        sendEvent(type, data = undefined) {
            const event = {
                type,
                ...(data === undefined ? {} : { data }),
            };

            if (typeof window.__jamEvent !== "function") {
                console.warn("[Jam] Rust event binding unavailable", event);
                return;
            }

            window.__jamEvent(JSON.stringify(event));
        },

        // ---------------------------------------------------------------------
        // Shutdown
        // ---------------------------------------------------------------------

        exit() {
            this.removeEventListeners();

            this.playerAPI = null;
            this.searchRequest = null;

            this.platform = null;
            this.services.clear();
            this.searchOperations.clear();

            this.initialized = false;
            this.lastSessionState = null;

            if (window.__MYJAM_BRIDGE_V1__ === this) {
                delete window.__MYJAM_BRIDGE_V1__;
            }
            if (window.bridge === this) {
                delete window.bridge;
            }

            console.log("[Jam] Bridge exited");
        },
    };

    bridge.version = 1;
    bridge.getSessionState = bridge.getState;
    bridge.dispose = bridge.exit;
    window.__MYJAM_BRIDGE_V1__ = bridge;
    window.bridge = bridge;

    try {
        console.log("[Jam] Initialisation started");
        await bridge.initialise();
    } catch (error) {
        console.error("[Jam] Initialisation threw:", error);
        bridge._initializationError = error;
    }

    return bridge;
})();