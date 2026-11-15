import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

const matcherPath = new URL("../src/adapters/matcher.js", import.meta.url);

async function loadMatcher() {
    const context = vm.createContext({});
    const source = await readFile(matcherPath, "utf8");
    new vm.Script(source, { filename: fileURLToPath(matcherPath) }).runInContext(context);
    return context.__ATTUNE_MATCHER__;
}

const matcher = await loadMatcher();
const song = (title, artists, extra = {}) => ({ title, artists, ...extra });

// Each pair was seen live (see PROVIDER_ADAPTERS.md): the same recording
// named differently on two services.
const SAME = [
    ["remaster tag after a dash", song("Hotel California - 2013 Remaster", ["Eagles"]), song("Hotel California", ["Eagles"])],
    ["remaster year after the tag", song("Enter Sandman - Remastered 2021", ["Metallica"]), song("Enter Sandman", ["Metallica"])],
    ["remaster in brackets", song("Whole Lotta Love - 1990 Remaster", ["Led Zeppelin"]), song("Whole Lotta Love (Remaster)", ["Led Zeppelin"])],
    ["year and remaster", song("Rock You Like a Hurricane - 2015 - Remaster", ["Scorpions"]), song("Rock You Like a Hurricane", ["Scorpions"])],
    ["band credit", song("Purple Rain", ["Prince"]), song("Purple Rain", ["Prince & The Revolution"])],
    ["featured artist in the title", song("Shoong! (feat. LISA of BLACKPINK)", ["TAEYANG"]), song("Shoong!", ["TAEYANG", "LISA"])],
    ["featured artists twice", song("Get Lucky (feat. Pharrell Williams and Nile Rodgers)", ["Daft Punk", "Pharrell Williams", "Nile Rodgers"]), song("Get Lucky", ["Daft Punk"])],
    ["music video", song("Blinding Lights (Official Video)", ["The Weeknd"]), song("Blinding Lights", ["The Weeknd"])],
    ["music video with ft.", song("Starboy ft. Daft Punk (Official Video) (feat. Daft Punk)", ["The Weeknd"]), song("Starboy", ["The Weeknd", "Daft Punk"])],
    ["official music video", song("Take My Breath (Official Music Video)", ["The Weeknd"]), song("Take My Breath", ["The Weeknd"])],
    ["subtitle only the song has", song("Sunflower", ["Post Malone", "Swae Lee"]), song("Sunflower (Spider-Man: Into the Spider-Verse)", ["Post Malone & Swae Lee"])],
    ["soundtrack subtitle", song("I Ain’t Worried (From “Top Gun: Maverick”)", ["OneRepublic"]), song("I Ain't Worried", ["OneRepublic"])],
    ["apostrophes and quotes", song("Don’t Start Now", ["Dua Lipa"]), song("Don't Start Now", ["Dua Lipa"])],
    ["accents", song("Déjà Vu", ["Beyoncé"]), song("Deja Vu", ["Beyonce"])],
    ["& and and", song("Simon & Garfunkel Medley", ["Simon & Garfunkel"]), song("Simon and Garfunkel Medley", ["Simon and Garfunkel"])],
    ["part spelled out", song("Love Me Pt. II", ["Artist"]), song("Love Me (Part 2)", ["Artist"])],
    ["artist before the title of a video", song("Arctic Monkeys - Do I Wanna Know? (Official Video)", ["Arctic Monkeys"]), song("Do I Wanna Know?", ["Arctic Monkeys"])],
    ["radio edit", song("Levels (Radio Edit)", ["Avicii"]), song("Levels", ["Avicii"])],
    ["same remix", song("Dancin - Krono Remix (Official Video) (feat. Luvli)", ["Aaron Smith"]), song("Dancin (feat. Luvli) - Krono Remix", ["Aaron Smith", "Luvli"])],
    ["names in two scripts, same length", song("Lemon", ["米津玄師"], { durationMs: 255_000 }), song("Lemon", ["Kenshi Yonezu"], { durationMs: 255_500 })],
];

// Different recordings or songs that look alike.
const DIFFERENT = [
    ["live version", song("Hotel California", ["Eagles"]), song("Hotel California - Live", ["Eagles"])],
    ["live in brackets", song("Sunflower", ["Post Malone"]), song("Sunflower (Live)", ["Post Malone"])],
    ["remix", song("Levels", ["Avicii"]), song("Levels (Skrillex Remix)", ["Avicii"])],
    ["another remixer", song("Dancin (Krono Remix)", ["Aaron Smith"]), song("Dancin (Kygo Remix)", ["Aaron Smith"])],
    ["acoustic", song("Wonderwall", ["Oasis"]), song("Wonderwall (Acoustic)", ["Oasis"])],
    ["sped up", song("Cupid", ["FIFTY FIFTY"]), song("Cupid (Sped Up Version)", ["FIFTY FIFTY"])],
    ["karaoke", song("Don't Start Now", ["Dua Lipa"]), song("Don't Start Now (Karaoke Version)", ["Sing2Piano"])],
    ["cover by someone else", song("Don't Start Now", ["Dua Lipa"]), song("Don't Start Now", ["Stephen Scaccia"])],
    ["a credit naming the artist", song("Don't Start Now", ["Dua Lipa"]), song("Don't Start Now (Dua Lipa)", ["Josephine Alexandra"])],
    ["a longer name", song("Love Me", ["The Beatles"]), song("Love Me Do", ["The Beatles"])],
    ["another part", song("Love Me", ["Artist"]), song("Love Me (Part 2)", ["Artist"])],
    ["names in two scripts, other length", song("Lemon", ["米津玄師"], { durationMs: 255_000 }), song("Lemon", ["Lemon Demon"], { durationMs: 180_000 })],
    ["Taylor's Version", song("Love Story", ["Taylor Swift"]), song("Love Story (Taylor's Version)", ["Taylor Swift"])],
];

for (const [name, source, candidate] of SAME) {
    test(`Same song: ${name}`, () => {
        const value = matcher.score(source, candidate);
        assert.ok(value != null && value >= matcher.ACCEPT_SCORE, `scored ${value}`);
        // And the other way round.
        const back = matcher.score(candidate, source);
        assert.ok(back != null && back >= matcher.ACCEPT_SCORE, `the other way scored ${back}`);
    });
}

for (const [name, source, candidate] of DIFFERENT) {
    test(`Different song: ${name}`, () => {
        const value = matcher.score(source, candidate);
        assert.ok(value == null || value < matcher.ACCEPT_SCORE, `scored ${value}`);
    });
}

test("The original beats covers, karaoke and a different length", () => {
    const source = song("Blinding Lights", ["The Weeknd"], { durationMs: 200_000, album: "After Hours" });
    const candidates = [
        { id: "cover", track: song("Blinding Lights", ["The Naked and Famous"], { durationMs: 188_000 }) },
        { id: "karaoke", track: song("Blinding Lights (Karaoke Version)", ["Sing2Piano"], { durationMs: 200_000 }) },
        { id: "single", track: song("Blinding Lights", ["The Weeknd"], { durationMs: 201_000, album: "Blinding Lights" }) },
        { id: "album", track: song("Blinding Lights", ["The Weeknd"], { durationMs: 200_000, album: "After Hours (Deluxe)" }) },
    ];
    assert.equal(matcher.pick(source, candidates).candidate.id, "album");
});

test("Equal candidates keep the service's order", () => {
    const source = song("Shoong!", ["TAEYANG"]);
    const candidates = [
        { id: "single", track: song("Shoong! (feat. LISA of BLACKPINK)", ["TAEYANG", "LISA"]) },
        { id: "album", track: song("Shoong! (feat. LISA of BLACKPINK)", ["TAEYANG", "LISA"]) },
    ];
    assert.equal(matcher.pick(source, candidates).candidate.id, "single");
});

test("A matching ISRC wins; a different one only counts against", () => {
    const source = song("Get Lucky", ["Daft Punk"], { isrc: "USQX91300108", durationMs: 369_000 });
    const sameCode = song("Get Lucky", ["Daft Punk"], { isrc: "USQX91300108", durationMs: 368_000 });
    const otherCode = song("Get Lucky", ["Daft Punk"], { isrc: "USQX91300999", durationMs: 369_000 });
    assert.ok(matcher.score(source, sameCode) > matcher.score(source, otherCode));
    assert.ok(matcher.score(source, otherCode) >= matcher.ACCEPT_SCORE, "a re-release is still the song");
});

// Everyone plays the session's song at the same position: another length
// would leave its listeners that far apart all song long.
test("Another length is another cut, even with the same name, artist or ISRC", () => {
    const album = song("Get Lucky", ["Daft Punk"], { isrc: "USQX91300108", durationMs: 369_000 });
    const radioEdit = song("Get Lucky (Radio Edit)", ["Daft Punk"], { isrc: "USQX91300108", durationMs: 248_000 });
    assert.equal(matcher.score(album, radioEdit), null);
    assert.equal(matcher.pickByIsrc(album, [{ id: "edit", track: radioEdit }]), null);
    // Verified live: the video runs 263 s, the song 202 s.
    const video = song("Blinding Lights", ["The Weeknd"], { durationMs: 263_000, provider: "YoutubeMusic" });
    assert.equal(matcher.score(video, song("Blinding Lights", ["The Weeknd"], { durationMs: 202_000 })), null);
    // Services round differently: 2 s is still the same recording, and so is 4 s.
    for (const offset of [2_000, 4_000]) {
        const value = matcher.score(album, { ...album, isrc: null, durationMs: 369_000 + offset });
        assert.ok(value >= matcher.ACCEPT_SCORE, `${offset} ms: ${value}`);
    }
    assert.equal(matcher.score(album, { ...album, isrc: null, durationMs: 374_000 }), null);
});

test("Among releases of one ISRC, the same album, else the closest length", () => {
    const source = song("Get Lucky", ["Daft Punk"], { isrc: "USQX91300108", album: "Random Access Memories", durationMs: 369_000 });
    const releases = [
        { id: "compilation", track: song("Get Lucky", ["Daft Punk"], { isrc: "USQX91300108", album: "Hits 2013", durationMs: 369_000 }) },
        { id: "album", track: song("Get Lucky", ["Daft Punk"], { isrc: "USQX91300108", album: "Random Access Memories", durationMs: 369_000 }) },
    ];
    assert.equal(matcher.pickByIsrc(source, releases).id, "album");
    // A wrong code (neither title nor artist alike) is a data error.
    const wrong = [{ id: "wrong", track: song("Something Else", ["Nobody"], { isrc: "USQX91300108" }) }];
    assert.equal(matcher.pickByIsrc(source, wrong), null);
});

test("Searching stops at a confident match and ranks all queries otherwise", async () => {
    const asked = [];
    const results = {
        "Get Lucky Daft Punk": [{ id: "original", track: song("Get Lucky", ["Daft Punk", "Pharrell Williams"], { durationMs: 369_000 }) }],
    };
    const search = async query => {
        asked.push(query);
        return results[query] ?? [];
    };
    const track = song("Get Lucky (feat. Pharrell Williams and Nile Rodgers)", ["Daft Punk", "Pharrell Williams", "Nile Rodgers"], { durationMs: 369_000 });
    const found = await matcher.searchText(track, search, { idOf: c => c.id });
    assert.equal(found.id, "original");
    assert.deepEqual(asked, ["Get Lucky Daft Punk"], "the first query was enough");

    // No confident match: every query runs and the best of all wins.
    asked.length = 0;
    const weak = song("Sunflower", ["Post Malone"]);
    const later = {
        "Sunflower Post Malone": [{ id: "soundtrack", track: song("Sunflower (Spider-Man: Into the Spider-Verse)", ["Post Malone", "Swae Lee"]) }],
    };
    const found2 = await matcher.searchText(weak, async query => {
        asked.push(query);
        return later[query] ?? [];
    }, { idOf: c => c.id });
    assert.equal(found2.id, "soundtrack");
});

test("Queries: the name with its version and main artist first, apostrophes optional", () => {
    assert.deepEqual(
        matcher.searchQueries(song("Don't Start Now", ["Dua Lipa"]), { withoutApostrophes: true })[0],
        "Dont Start Now Dua Lipa",
    );
    assert.equal(matcher.searchQueries(song("Levels (Skrillex Remix)", ["Avicii"]))[0], "Levels skrillex remix Avicii");
    assert.equal(matcher.searchQueries(song("Blinding Lights (Official Video)", ["The Weeknd"]))[0], "Blinding Lights The Weeknd");
});

test("An ID that came with the song is used for its own service only", () => {
    const track = song("x", ["y"], { provider: "Spotify", providerTrackId: "spotify:track:1", ids: { Tidal: "42" } });
    assert.equal(matcher.knownId(track, "Spotify"), "spotify:track:1");
    assert.equal(matcher.knownId(track, "Tidal"), "42");
    assert.equal(matcher.knownId(track, "AppleMusic"), null);
});

// ---------------------------------------------------------------------------
// Recorded searches
// ---------------------------------------------------------------------------
//
// Each file in corpus/ holds songs to find on one service, the candidates
// its search returned for every query (recorded live), and the result
// checked by hand. A change to the matcher has to keep finding them.
// A query the matcher now asks that was never recorded fails the case:
// record the service again rather than read it as "no results".

const corpusDir = new URL("./corpus/", import.meta.url);
const QUERY_OPTIONS = { YoutubeMusic: { withoutApostrophes: true } };

for (const file of (await readdir(corpusDir)).filter(name => name.endsWith(".json"))) {
    const corpus = JSON.parse(await readFile(new URL(file, corpusDir), "utf8"));
    for (const [index, entry] of corpus.cases.entries()) {
        const name = `${entry.track.title} | ${entry.track.artists.join(", ")}`;
        test(`${corpus.service} corpus #${index} (${entry.from}): ${name}`, async () => {
            const unrecorded = [];
            const search = async query => {
                if (!(query in corpus.searches)) unrecorded.push(query);
                return corpus.searches[query] ?? [];
            };
            const found = await matcher.searchText(entry.track, search, {
                idOf: candidate => candidate.id,
                ...QUERY_OPTIONS[corpus.service],
            });
            const expected = entry.expected?.id ?? null;
            if ((found?.id ?? null) !== expected && unrecorded.length) {
                assert.fail(`not recorded in ${file}: ${JSON.stringify(unrecorded)}`);
            }
            assert.equal(
                found?.id ?? null,
                expected,
                `found ${found ? `"${found.track.title}" by ${found.track.artists.join(", ")}` : "nothing"}, ` +
                `expected ${entry.expected ? `"${entry.expected.title}"` : "nothing"}`,
            );
        });
    }
}

test("Artists in another script are compared by the service's names for them in that language", async () => {
    // Verified live: YouTube Music and Apple Music name the same song's
    // artist "米津玄師" in Japanese and "Kenshi Yonezu" in English.
    const source = song("Lemon", ["米津玄師"], { durationMs: 255_000 });
    const results = [
        { id: "cover", track: song("Lemon", ["Lemon Covers Band"], { durationMs: 255_000 }) },
        { id: "original", track: song("Lemon", ["Kenshi Yonezu"], { durationMs: 256_000 }) },
    ];
    const asked = [];
    const alternateNames = async ({ candidates, language }) => {
        asked.push([language, Array.from(candidates, c => c.id)]);
        const japanese = { original: ["米津玄師"], cover: ["レモン・カバーズ"] };
        return new Map(candidates.map(c => [c.id, japanese[c.id] ?? []]));
    };
    const found = await matcher.searchText(source, async () => results, { idOf: c => c.id, alternateNames });
    assert.equal(found.id, "original");
    // Asked in Japanese first (kanji alone could be Chinese too), for the
    // candidates the names could decide.
    assert.deepEqual(asked.map(([language]) => language), ["ja"], "Chinese not asked once Japanese settled it");
    assert.deepEqual(asked[0][1], ["cover", "original"]);
});

test("Names in another language are only asked when the title and length agree", async () => {
    const source = song("Lemon", ["米津玄師"], { durationMs: 255_000 });
    let asked = 0;
    await matcher.searchText(source, async () => [
        { id: "other length", track: song("Lemon", ["Kenshi Yonezu"], { durationMs: 300_000 }) },
        { id: "other title", track: song("Flamingo", ["Kenshi Yonezu"], { durationMs: 255_000 }) },
    ], { idOf: c => c.id, alternateNames: async () => { asked += 1; return new Map(); } });
    assert.equal(asked, 0);
});

test("Latin names against a catalog in another script are asked in English", async () => {
    const source = song("Dynamite", ["BTS"], { durationMs: 199_000 });
    const languages = [];
    const found = await matcher.searchText(source, async () => [
        { id: "kr", track: song("Dynamite", ["방탄소년단"], { durationMs: 199_000 }) },
    ], {
        idOf: c => c.id,
        alternateNames: async ({ language }) => {
            languages.push(language);
            return new Map([["kr", ["BTS"]]]);
        },
    });
    assert.equal(found.id, "kr");
    assert.deepEqual(languages, ["en"]);
});

test("A service that can't give other names leaves the search as it was", async () => {
    const source = song("Lemon", ["米津玄師"], { durationMs: 255_000 });
    const found = await matcher.searchText(source, async () => [
        { id: "original", track: song("Lemon", ["Kenshi Yonezu"], { durationMs: 256_000 }) },
    ], { idOf: c => c.id, alternateNames: async () => { throw new Error("offline"); } });
    // The fallback: same name, same length to the second.
    assert.equal(found.id, "original");
});
