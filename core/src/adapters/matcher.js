/*
 * Attune's song matcher, shared by every adapter.
 *
 * Injected ahead of each adapter (see `providers.rs`), so all services find
 * songs the same way. It knows nothing about any service: an adapter hands
 * it the song to find and a function that searches its own catalog, and
 * gets back the best candidate or null.
 *
 * How a song is found:
 * 1. An ID for this service that came with the song (the queuer's own, or
 *    one another listener found): played as it is.
 * 2. Its ISRC, where the service can look one up: the same recording.
 * 3. Text searches, best query first. A clear match stops there; otherwise
 *    the candidates of all queries are ranked together.
 *
 * Candidates are scored, not filtered on exact text. A title is parsed into
 * its parts (the song's name, featured artists, version, subtitle, video
 * tags), so "Hotel California - 2013 Remaster", "Hotel California" and
 * "Hotel California (Official Video)" are one song, while "(Live)" or
 * "(Remix)" is another recording and never matches the original. What was
 * observed live is collected in PROVIDER_ADAPTERS.md.
 */
globalThis.__ATTUNE_MATCHER__ = (() => {
    "use strict";

    // A candidate needs this score to be taken at all …
    const ACCEPT_SCORE = 60;
    // … and with this one no further queries are tried.
    const CONFIDENT_SCORE = 85;

    // ---------------------------------------------------------------------
    // Text
    // ---------------------------------------------------------------------

    /**
     * Lowercase words without punctuation, accents or apostrophes, "&" as
     * "and". Non-Latin scripts are kept as they are.
     */
    function normalize(value) {
        return String(value ?? "")
            .normalize("NFKD")
            .replace(/\p{M}+/gu, "")
            .normalize("NFKC")
            .toLowerCase()
            .replace(/['’‘`´]/g, "")
            .replace(/&/g, " and ")
            .replace(/[^\p{L}\p{N}]+/gu, " ")
            .trim()
            .replace(/\s+/g, " ");
    }

    const PART_WORDS = new Map([["pt", "part"], ["vol", "volume"], ["no", "number"]]);
    const ROMAN = new Map([["i", "1"], ["ii", "2"], ["iii", "3"], ["iv", "4"], ["v", "5"], ["vi", "6"]]);

    /** Words of a title, with "Pt. II" and "Part 2" the same. */
    function titleWords(value) {
        const words = normalize(value).split(" ").filter(Boolean);
        return words.map((word, index) => {
            const previous = PART_WORDS.get(words[index - 1]) ?? words[index - 1];
            if (previous === "part" || previous === "volume" || previous === "chapter") {
                return ROMAN.get(word) ?? word;
            }
            return PART_WORDS.get(word) ?? word;
        });
    }

    /** How alike two word lists are, 0 to 1 (Sørensen–Dice on words). */
    function similarity(a, b) {
        if (!a.length || !b.length) return 0;
        const rest = [...b];
        let common = 0;
        for (const word of a) {
            const at = rest.indexOf(word);
            if (at >= 0) {
                common += 1;
                rest.splice(at, 1);
            }
        }
        return (2 * common) / (a.length + b.length);
    }

    const hasNonLatinLetters = value => /[^\p{Script=Latin}\P{L}]/u.test(String(value ?? ""));

    // ---------------------------------------------------------------------
    // Titles
    // ---------------------------------------------------------------------

    // Says only that it is a video: "(Official Video)", "[Lyric Video]", "(HD)" …
    const VIDEO_TAG = /^(?:official\s+)?(?:hd\s+|4k\s+)?(?:music\s+video|video\s+clip|lyric\s+video|lyrics?|visuali[sz]er|audio|video|mv|hd|4k|official)(?:\s+(?:hd|4k|version))?$/;
    // Another recording or take: never the same song as the original.
    const VERSION_WORDS = /\b(remix|mix|live|acoustic|instrumental|karaoke|cover|sped|slowed|reverb|nightcore|8d|demo|piano|orchestral|rework|vip|bootleg|mashup|tribute|originally|extended|remake|rerecorded|re recorded|taylors|unplugged|session|sessions|edit|version|stripped|a cappella|acapella|dub|flip)\b/;
    // The same recording under another name.
    const NEUTRAL = /^(?:(?:\d{4}\s+)?(?:digital(?:ly)?\s+)?remaster(?:ed)?(?:\s+\d{4})?(?:\s+version)?|\d{4}|radio\s+edit|single\s+(?:version|edit)|album\s+version|original\s+(?:mix|version)|mono|stereo|explicit|clean|deluxe(?:\s+edition)?|bonus\s+track)$/;
    const FEATURING = /^(?:feat|ft|featuring|with)\b\s*(.*)$/;
    // "(Part 2)", "(No. 5)", "(2)": part of the song's name.
    const NAME_PART = /^(?:(?:part|pt|vol|volume|chapter|no|number)\s+)?[\divx]+$/;

    function splitNames(value) {
        return String(value ?? "")
            .split(/\s*(?:,|&|\band\b|\bx\b|\bwith\b|\bfeat\b|\bft\b)\s*/i)
            .map(name => name.trim())
            .filter(Boolean);
    }

    /**
     * A title in parts: `{ name, featured, versions, subtitles, words }`.
     * `artists` lets "Artist - Song", as videos are often named, drop the
     * artist.
     */
    function parseTitle(title, artists = []) {
        let text = String(title ?? "");
        const groups = [];
        // Bracketed parts, innermost first.
        let previous;
        do {
            previous = text;
            text = text.replace(/\s*[([{]([^()[\]{}]*)[)\]}]/g, (_, inner) => {
                groups.push(inner);
                return " ";
            });
        } while (text !== previous);

        // " - " parts: "Song - 2013 Remaster", "Song - Live at Wembley".
        const dashed = text.split(/\s+[-–—]\s+/);
        const artistNames = new Set(artists.map(normalize).filter(Boolean));
        if (dashed.length > 1 && artistNames.has(normalize(dashed[0]))) dashed.shift();
        let name = dashed.shift() ?? "";
        groups.push(...dashed);

        // "Starboy ft. Daft Punk": featured artists outside brackets.
        const featured = [];
        name = name.replace(/\s+(?:feat\.?|ft\.|featuring)\s+(.+)$/i, (_, names) => {
            featured.push(...splitNames(names));
            return "";
        });
        // "| Official Video" and the like.
        name = name.replace(/\s*\|\s*official\b.*$/i, "");

        const versions = [];
        const subtitles = [];
        const nameParts = [];
        for (const group of groups) {
            const words = normalize(group);
            if (!words) continue;
            const feat = words.match(FEATURING);
            if (feat) featured.push(...splitNames(group.replace(/^\s*(?:feat\.?|ft\.?|featuring|with)\s*/i, "")));
            else if (VIDEO_TAG.test(words) || NEUTRAL.test(words)) continue;
            else if (NAME_PART.test(words)) nameParts.push(group);
            else if (VERSION_WORDS.test(words) && !/^(?:from|taken from)\b/.test(words)) versions.push(words);
            else subtitles.push(words);
        }
        const nameWords = titleWords([name, ...nameParts].join(" "));
        return { name: nameWords.join(" "), words: nameWords, featured, versions: versions.sort(), subtitles };
    }

    /** A music video's title as its song's: without "(Official Video)" and the like. */
    function videoTitleAsSong(title) {
        const cleaned = String(title ?? "")
            .replace(/\s*[([]([^()[\]]*)[)\]]/g, (whole, inner) => (VIDEO_TAG.test(normalize(inner)) ? " " : whole))
            .replace(/\s*\|\s*official\b.*$/i, " ")
            .replace(/\s+/g, " ")
            .trim();
        return cleaned || String(title ?? "");
    }

    const versionKeywords = version => new Set(version.match(new RegExp(VERSION_WORDS.source, "g")) ?? []);

    /**
     * Whether two titles' versions are the same recording. "Remix" and
     * "Krono Remix" are not; "Live" and "Live at Wembley" may be.
     * Null: different recordings. Otherwise a score adjustment.
     */
    function compareVersions(a, b) {
        if (a.length !== b.length) return null;
        let adjustment = 0;
        for (let index = 0; index < a.length; index += 1) {
            if (a[index] === b[index]) continue;
            const keywordsA = versionKeywords(a[index]);
            const keywordsB = versionKeywords(b[index]);
            const sameKeywords = keywordsA.size === keywordsB.size && [...keywordsA].every(word => keywordsB.has(word));
            if (!sameKeywords || similarity(a[index].split(" "), b[index].split(" ")) < 0.6) return null;
            adjustment -= 5;
        }
        return adjustment;
    }

    // ---------------------------------------------------------------------
    // Artists
    // ---------------------------------------------------------------------

    /**
     * Whether two normalized names are the same act. "Prince" is "Prince &
     * The Revolution", as YouTube Music credits "Purple Rain" (verified
     * live): a name followed by "and …" or "with …". "Shawn Mendes Covers"
     * is not Shawn Mendes (seen live among the search results). Very short
     * names must match exactly. "The" is optional.
     */
    function sameArtist(a, b) {
        const bare = name => name.replace(/^the /, "");
        a = bare(a);
        b = bare(b);
        if (a === b || a.replace(/ /g, "") === b.replace(/ /g, "")) return true;
        const [short, long] = a.length < b.length ? [a, b] : [b, a];
        return short.length >= 4 && (long.startsWith(`${short} and `) || long.startsWith(`${short} with `));
    }

    /** Everyone credited: the artists and any featured in the title. */
    function creditedArtists(track, parsed) {
        const names = [...(track.artists ?? []), ...parsed.featured];
        // "Post Malone & Swae Lee" as one name on one service, two on another.
        return [...new Set(names.flatMap(name => [name, ...splitNames(name)]).map(normalize).filter(Boolean))];
    }

    function artistOverlap(namesA, namesB) {
        return namesA.some(a => namesB.some(b => sameArtist(a, b)));
    }

    // ---------------------------------------------------------------------
    // Scoring
    // ---------------------------------------------------------------------

    const normalizeIsrc = value => String(value ?? "").replace(/[^a-z0-9]/gi, "").toUpperCase();

    // "Album (Deluxe Edition)" and "Album - Single" are the album.
    const albumName = value => normalize(String(value ?? "")
        .replace(/\s*[([][^()[\]]*(?:deluxe|edition|remaster|expanded|anniversary|version)[^()[\]]*[)\]]/gi, " ")
        .replace(/\s+-\s+(?:single|ep)$/i, " "));

    /**
     * How well `candidate` matches `source` (both track descriptors), or
     * null if it is a different song. Roughly: name 40 (25 if only close),
     * an artist 30 (+5 the same main artist), length up to 15, album 6,
     * ISRC 100.
     */
    function score(source, candidate) {
        const parsedSource = parseTitle(source.title, source.artists);
        const parsedCandidate = parseTitle(candidate.title, candidate.artists);

        // Another version is another recording.
        const versionAdjustment = compareVersions(parsedSource.versions, parsedCandidate.versions);
        if (versionAdjustment == null) return null;

        const lengthDifference = Number.isFinite(source.durationMs) && Number.isFinite(candidate.durationMs)
            ? Math.abs(source.durationMs - candidate.durationMs)
            : null;

        let value = versionAdjustment;
        if (parsedSource.name && parsedSource.name === parsedCandidate.name) {
            value += 40;
        } else if (
            // Nearly the same name ("Pt. 2" spelled out, a word more or
            // less in a long title) at nearly the same length. Short names
            // are left out: "Love Me" is not "Love Me Do".
            Math.min(parsedSource.words.length, parsedCandidate.words.length) >= 3 &&
            similarity(parsedSource.words, parsedCandidate.words) >= 0.8 &&
            (lengthDifference == null || lengthDifference <= 5_000)
        ) {
            value += 25;
        } else {
            return null;
        }
        if (normalize(videoTitleAsSong(source.title)) === normalize(candidate.title)) value += 5;
        // A subtitle only one of them has ("Sunflower (Spider-Man: …)") is fine.
        const subtitlesA = parsedSource.subtitles.join(" ");
        const subtitlesB = parsedCandidate.subtitles.join(" ");
        if (subtitlesA !== subtitlesB) value -= subtitlesA && subtitlesB ? 2 : 3;

        const artistsA = creditedArtists(source, parsedSource);
        const artistsB = creditedArtists(candidate, parsedCandidate);
        if (artistOverlap(artistsA, artistsB)) {
            value += 30;
            const mainA = normalize(source.artists?.[0]);
            if (mainA && sameArtist(mainA, normalize(candidate.artists?.[0]))) value += 5;
        } else if (
            // "米津玄師" and "Kenshi Yonezu": names in two scripts can't be
            // compared. The same name and length to the second still are.
            lengthDifference != null && lengthDifference <= 2_000 &&
            hasNonLatinLetters(artistsA.join(" ")) !== hasNonLatinLetters(artistsB.join(" "))
        ) {
            value += 20;
        } else {
            return null;
        }

        if (lengthDifference != null) {
            // A music video runs longer than its song (intro, outro): 263 s
            // for the 202 s "Blinding Lights" (verified live). So a long
            // difference ranks a candidate lower but never rules out the
            // same name by the same artist.
            const fromVideo = source.provider === "YoutubeMusic" || candidate.provider === "YoutubeMusic";
            if (lengthDifference <= 2_500) value += 15;
            else if (lengthDifference <= 5_000) value += 8;
            else if (lengthDifference > 10_000 && !fromVideo) value -= 15;
        }
        if (source.album && candidate.album && albumName(source.album) === albumName(candidate.album)) value += 6;

        const isrcA = normalizeIsrc(source.isrc);
        const isrcB = normalizeIsrc(candidate.isrc);
        if (isrcA && isrcB) {
            // Re-releases can get a code of their own, so a different one
            // only counts against.
            value += isrcA === isrcB ? 100 : -20;
        }
        return value;
    }

    /** The best of `candidates` (`{ track, … }`, in the service's order), or null. */
    function pick(source, candidates) {
        let best = null;
        for (const candidate of candidates) {
            const value = score(source, candidate.track);
            if (value == null || value < ACCEPT_SCORE) continue;
            // Ties keep the service's own order.
            if (!best || value > best.score) best = { candidate, score: value };
        }
        return best;
    }

    /**
     * Among candidates with the song's ISRC (one recording, but on the
     * album, the single and every compilation: five for "Get Lucky" on
     * Apple Music, verified live), the one on the same album, else the
     * closest length, else the service's first. A hit with neither the
     * title nor an artist in common is more likely a data error.
     */
    function pickByIsrc(source, candidates) {
        const isrc = normalizeIsrc(source.isrc);
        const parsedSource = parseTitle(source.title, source.artists);
        const artistsA = creditedArtists(source, parsedSource);
        let best = null;
        for (const candidate of candidates) {
            const track = candidate.track;
            if (isrc && normalizeIsrc(track.isrc) && normalizeIsrc(track.isrc) !== isrc) continue;
            const parsed = parseTitle(track.title, track.artists);
            const sameName = similarity(parsedSource.words, parsed.words) >= 0.8;
            if (!sameName && !artistOverlap(artistsA, creditedArtists(track, parsed))) continue;
            let value = sameName ? 10 : 0;
            if (source.album && track.album && albumName(source.album) === albumName(track.album)) value += 6;
            if (Number.isFinite(source.durationMs) && Number.isFinite(track.durationMs)) {
                value -= Math.min(5, Math.abs(source.durationMs - track.durationMs) / 1000);
            }
            if (!best || value > best.score) best = { candidate, score: value };
        }
        return best?.candidate ?? null;
    }

    // ---------------------------------------------------------------------
    // Searching
    // ---------------------------------------------------------------------

    /**
     * Queries to try in turn. The song's name with its version and the main
     * artist comes first: featured artists named twice threw YouTube Music's
     * search off ("Get Lucky (feat. Pharrell Williams and Nile Rodgers) Daft
     * Punk Pharrell Williams" found only covers, verified live). Then the
     * whole title with the main artist, then with two.
     * `withoutApostrophes`: YouTube Music's search found only covers for
     * "Don't Start Now Dua Lipa", the original for "Dont …" (verified live).
     */
    function searchQueries(track, { withoutApostrophes = false } = {}) {
        const artists = (track.artists ?? []).filter(Boolean);
        const parsed = parseTitle(track.title, artists);
        const raw = String(track.title ?? "");
        // The name as written, without brackets, plus any version tag.
        const written = raw.replace(/\s*[([{][^()[\]{}]*[)\]}]/g, " ").split(/\s+[-–—]\s+/)[0]
            .replace(/\s+(?:feat\.?|ft\.|featuring)\s+.+$/i, "").trim() || raw;
        const first = [written, ...parsed.versions, artists[0] ?? ""].join(" ");
        const queries = [
            withoutApostrophes ? first.replace(/['’‘`]/g, "") : first,
            `${videoTitleAsSong(raw)} ${artists[0] ?? ""}`,
            `${videoTitleAsSong(raw)} ${artists.slice(0, 2).join(" ")}`,
        ].map(query => query.replace(/\s+/g, " ").trim());
        return [...new Set(queries)].filter(Boolean);
    }

    /**
     * The best candidate for `track` from text searches, or null.
     * `search(query)` resolves to candidates `{ track, … }` in the service's
     * order. A confident match stops the queries; otherwise all are tried
     * and ranked together. `idOf(candidate)` tells the same candidate apart
     * when several queries find it.
     */
    async function searchText(track, search, { idOf, withoutApostrophes = false } = {}) {
        const seen = new Set();
        const all = [];
        let best = null;
        for (const query of searchQueries(track, { withoutApostrophes })) {
            for (const candidate of await search(query)) {
                const id = idOf ? idOf(candidate) : candidate;
                if (seen.has(id)) continue;
                seen.add(id);
                all.push(candidate);
            }
            best = pick(track, all);
            if (best && best.score >= CONFIDENT_SCORE) break;
        }
        return best?.candidate ?? null;
    }

    /**
     * An ID for `service` that came with the song: the queuer's own, or one
     * another listener found (`ids`).
     */
    function knownId(track, service) {
        const own = track.providerTrackId ?? track.provider_track_id;
        if (track.provider === service && own) return String(own);
        const shared = track.ids?.[service];
        return shared ? String(shared) : null;
    }

    /** One lookup per song, however it is spelled out. */
    function cacheKey(track) {
        return [
            normalizeIsrc(track.isrc),
            normalize(track.title),
            (track.artists ?? []).map(normalize).join("|"),
            normalize(track.album),
            track.durationMs ?? "",
        ].join("::");
    }

    return Object.freeze({
        ACCEPT_SCORE,
        CONFIDENT_SCORE,
        normalize,
        normalizeIsrc,
        parseTitle,
        videoTitleAsSong,
        sameArtist,
        score,
        pick,
        pickByIsrc,
        searchQueries,
        searchText,
        knownId,
        cacheKey,
    });
})();
