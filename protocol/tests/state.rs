mod common;

use std::collections::HashSet;

use attune_protocol::{
    SessionOperation as Op,
    state::{
        MAX_QUEUE_ENTRIES, StateError, apply, is_noop, position_at, reinsert_unavailable,
        validate_state,
    },
};
use common::{DURATION_MS, empty, entry, ids, state};

const NOW: u64 = 10_000;

fn id(value: &str) -> String {
    value.to_owned()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[test]
fn valid_states_pass() {
    assert_eq!(validate_state(&state(&["a", "b"], true, 5, 0)), Ok(()));
    assert_eq!(validate_state(&empty(0)), Ok(()));
}

#[test]
fn empty_queue_must_be_stopped_without_current() {
    let mut with_current = empty(0);
    with_current.current_entry_id = Some(id("a"));
    assert_eq!(
        validate_state(&with_current),
        Err(StateError::InvalidEmptyState)
    );

    let mut playing = empty(0);
    playing.playback.is_playing = true;
    assert_eq!(validate_state(&playing), Err(StateError::InvalidEmptyState));

    let mut positioned = empty(0);
    positioned.playback.position_ms = 1;
    assert_eq!(
        validate_state(&positioned),
        Err(StateError::InvalidEmptyState)
    );
}

#[test]
fn current_entry_must_be_first() {
    let mut s = state(&["a", "b"], false, 0, 0);
    s.current_entry_id = Some(id("b"));
    assert_eq!(validate_state(&s), Err(StateError::CurrentNotFirst));

    s.current_entry_id = None;
    assert_eq!(validate_state(&s), Err(StateError::CurrentNotFirst));
}

#[test]
fn duplicate_entry_ids_are_rejected_but_duplicate_tracks_are_allowed() {
    let duplicated_id = state(&["a", "a"], false, 0, 0);
    assert_eq!(
        validate_state(&duplicated_id),
        Err(StateError::DuplicateEntry { entry_id: id("a") })
    );

    let mut same_track_twice = state(&["a", "b"], false, 0, 0);
    same_track_twice.queue[1].track = same_track_twice.queue[0].track.clone();
    assert_eq!(validate_state(&same_track_twice), Ok(()));
}

type TrackMutation = fn(&mut attune_protocol::TrackDescriptor);

#[test]
fn track_fields_are_bounded() {
    let cases: Vec<(TrackMutation, &str)> = vec![
        (|t| t.title = "   ".to_owned(), "title"),
        (|t| t.title = "x".repeat(513), "title"),
        (|t| t.title = "bad\nname".to_owned(), "title"),
        (|t| t.artists = vec![String::new()], "artists"),
        (|t| t.artists = vec!["a".to_owned(); 33], "artists"),
        (|t| t.isrc = Some("x".repeat(33)), "isrc"),
        (|t| t.duration_ms = Some(25 * 60 * 60 * 1000), "durationMs"),
    ];
    for (mutate, field) in cases {
        let mut s = state(&["a"], false, 0, 0);
        mutate(&mut s.queue[0].track);
        assert_eq!(
            validate_state(&s),
            Err(StateError::InvalidTrackField { field })
        );
    }

    // Optional fields may be empty; adapters do not always know the album.
    let mut s = state(&["a"], false, 0, 0);
    s.queue[0].track.album = Some(String::new());
    s.queue[0].track.artists.clear();
    assert_eq!(validate_state(&s), Ok(()));
}

#[test]
fn entry_ids_and_queue_length_are_bounded() {
    let mut s = state(&["a"], false, 0, 0);
    s.queue[0].entry_id = "x".repeat(65);
    s.current_entry_id = Some("x".repeat(65));
    assert_eq!(validate_state(&s), Err(StateError::InvalidEntryId));

    let many: Vec<String> = (0..=MAX_QUEUE_ENTRIES).map(|i| i.to_string()).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(
        validate_state(&state(&refs, false, 0, 0)),
        Err(StateError::QueueTooLong {
            len: MAX_QUEUE_ENTRIES + 1
        })
    );
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

#[test]
fn position_extrapolates_only_while_playing_and_clamps_to_duration() {
    assert_eq!(position_at(&state(&["a"], true, 1_000, 0), 500), 1_500);
    assert_eq!(position_at(&state(&["a"], false, 1_000, 0), 500), 1_000);
    assert_eq!(
        position_at(&state(&["a"], true, 0, 0), DURATION_MS * 2),
        DURATION_MS
    );
    // A capture time in the future never moves the position backwards.
    assert_eq!(position_at(&state(&["a"], true, 1_000, 900), 500), 1_000);
}

#[test]
fn every_transition_captures_playback_at_now() {
    let before = state(&["a", "b"], true, 1_000, 0);
    for operation in [
        Op::PlaybackSetPlaying { is_playing: false },
        Op::PlaybackSeek {
            entry_id: id("a"),
            position_ms: 7,
        },
        Op::QueueRemove { entry_id: id("b") },
    ] {
        let after = apply(&before, &operation, NOW).expect("apply");
        assert_eq!(after.playback.position_captured_at_ms, NOW, "{operation:?}");
    }
}

// ---------------------------------------------------------------------------
// queue.replace
// ---------------------------------------------------------------------------

#[test]
fn replace_with_same_current_keeps_playback() {
    let before = state(&["a", "b"], true, 1_000, 0);
    let after = apply(
        &before,
        &Op::QueueReplace {
            entries: vec![entry("a"), entry("c"), entry("b")],
            current_entry_id: Some(id("a")),
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(ids(&after), ["a", "c", "b"]);
    assert_eq!(after.playback.position_ms, 1_000 + NOW);
    assert!(after.playback.is_playing);
}

#[test]
fn replace_with_new_current_drops_history_and_restarts_playing() {
    let before = state(&["a"], false, 1_000, 0);
    let after = apply(
        &before,
        &Op::QueueReplace {
            entries: vec![entry("x"), entry("y"), entry("z")],
            current_entry_id: Some(id("y")),
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(ids(&after), ["y", "z"]);
    assert_eq!(after.current_entry_id.as_deref(), Some("y"));
    assert_eq!(after.playback.position_ms, 0);
    assert!(after.playback.is_playing);
}

#[test]
fn replace_without_current_uses_first_entry() {
    let after = apply(
        &empty(0),
        &Op::QueueReplace {
            entries: vec![entry("x"), entry("y")],
            current_entry_id: None,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(after.current_entry_id.as_deref(), Some("x"));
}

#[test]
fn replace_with_empty_entries_yields_empty_state() {
    let after = apply(
        &state(&["a"], true, 5, 0),
        &Op::QueueReplace {
            entries: vec![],
            current_entry_id: None,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(after, empty(NOW));
}

#[test]
fn replace_rejects_unknown_current_and_duplicates() {
    let before = state(&["a"], false, 0, 0);
    assert_eq!(
        apply(
            &before,
            &Op::QueueReplace {
                entries: vec![entry("x")],
                current_entry_id: Some(id("nope")),
            },
            NOW
        ),
        Err(StateError::EntryNotFound {
            entry_id: id("nope")
        })
    );
    assert_eq!(
        apply(
            &before,
            &Op::QueueReplace {
                entries: vec![entry("x"), entry("x")],
                current_entry_id: None,
            },
            NOW
        ),
        Err(StateError::DuplicateEntry { entry_id: id("x") })
    );
}

// ---------------------------------------------------------------------------
// queue.insert
// ---------------------------------------------------------------------------

#[test]
fn insert_upcoming_keeps_current() {
    let before = state(&["a", "b"], true, 100, NOW);
    let after = apply(
        &before,
        &Op::QueueInsert {
            entry: entry("c"),
            index: 1,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(ids(&after), ["a", "c", "b"]);
    assert_eq!(after.playback.position_ms, 100);
}

#[test]
fn insert_at_end_is_allowed_and_past_end_is_rejected() {
    let before = state(&["a", "b"], false, 0, 0);
    let after = apply(
        &before,
        &Op::QueueInsert {
            entry: entry("c"),
            index: 2,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(ids(&after), ["a", "b", "c"]);

    assert_eq!(
        apply(
            &before,
            &Op::QueueInsert {
                entry: entry("c"),
                index: 3
            },
            NOW
        ),
        Err(StateError::IndexOutOfRange { index: 3, len: 2 })
    );
}

#[test]
fn insert_at_front_becomes_current_at_zero() {
    let after = apply(
        &state(&["a"], true, 5_000, NOW),
        &Op::QueueInsert {
            entry: entry("c"),
            index: 0,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(ids(&after), ["c", "a"]);
    assert_eq!(after.current_entry_id.as_deref(), Some("c"));
    assert_eq!(after.playback.position_ms, 0);
    assert!(after.playback.is_playing);
}

#[test]
fn insert_into_empty_queue_becomes_current_but_stays_paused() {
    let after = apply(
        &empty(0),
        &Op::QueueInsert {
            entry: entry("c"),
            index: 0,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(after.current_entry_id.as_deref(), Some("c"));
    assert!(!after.playback.is_playing);
}

#[test]
fn insert_rejects_existing_entry_id() {
    assert_eq!(
        apply(
            &state(&["a", "b"], false, 0, 0),
            &Op::QueueInsert {
                entry: entry("b"),
                index: 0
            },
            NOW
        ),
        Err(StateError::DuplicateEntry { entry_id: id("b") })
    );
}

// ---------------------------------------------------------------------------
// queue.remove
// ---------------------------------------------------------------------------

#[test]
fn remove_upcoming_keeps_current() {
    let after = apply(
        &state(&["a", "b", "c"], true, 100, NOW),
        &Op::QueueRemove { entry_id: id("b") },
        NOW,
    )
    .expect("apply");
    assert_eq!(ids(&after), ["a", "c"]);
    assert_eq!(after.playback.position_ms, 100);
}

#[test]
fn remove_current_makes_next_current_at_zero() {
    let after = apply(
        &state(&["a", "b"], true, 100, NOW),
        &Op::QueueRemove { entry_id: id("a") },
        NOW,
    )
    .expect("apply");
    assert_eq!(after.current_entry_id.as_deref(), Some("b"));
    assert_eq!(after.playback.position_ms, 0);
}

#[test]
fn remove_last_entry_yields_empty_state() {
    let after = apply(
        &state(&["a"], true, 100, 0),
        &Op::QueueRemove { entry_id: id("a") },
        NOW,
    )
    .expect("apply");
    assert_eq!(after, empty(NOW));
}

#[test]
fn remove_unknown_entry_is_rejected() {
    assert_eq!(
        apply(
            &state(&["a"], false, 0, 0),
            &Op::QueueRemove { entry_id: id("z") },
            NOW
        ),
        Err(StateError::EntryNotFound { entry_id: id("z") })
    );
}

// ---------------------------------------------------------------------------
// queue.move
// ---------------------------------------------------------------------------

#[test]
fn move_upcoming_entries() {
    let before = state(&["a", "b", "c", "d"], false, 0, 0);
    let after = apply(
        &before,
        &Op::QueueMove {
            entry_id: id("b"),
            to_index: 3,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(ids(&after), ["a", "c", "d", "b"]);
    assert_eq!(after.current_entry_id.as_deref(), Some("a"));
}

#[test]
fn move_to_front_makes_entry_current() {
    let after = apply(
        &state(&["a", "b", "c"], true, 900, NOW),
        &Op::QueueMove {
            entry_id: id("c"),
            to_index: 0,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(ids(&after), ["c", "a", "b"]);
    assert_eq!(after.current_entry_id.as_deref(), Some("c"));
    assert_eq!(after.playback.position_ms, 0);
}

#[test]
fn current_entry_can_only_move_to_front() {
    let before = state(&["a", "b"], true, 900, NOW);
    assert_eq!(
        apply(
            &before,
            &Op::QueueMove {
                entry_id: id("a"),
                to_index: 1
            },
            NOW
        ),
        Err(StateError::CannotMoveCurrent)
    );
    let unchanged = apply(
        &before,
        &Op::QueueMove {
            entry_id: id("a"),
            to_index: 0,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(unchanged, before);
}

#[test]
fn move_bounds_are_checked() {
    assert_eq!(
        apply(
            &state(&["a", "b"], false, 0, 0),
            &Op::QueueMove {
                entry_id: id("b"),
                to_index: 2
            },
            NOW
        ),
        Err(StateError::IndexOutOfRange { index: 2, len: 2 })
    );
}

// ---------------------------------------------------------------------------
// playback.*
// ---------------------------------------------------------------------------

#[test]
fn select_and_advance_drop_history_and_set_playback() {
    for operation in [
        Op::PlaybackSelect {
            entry_id: id("c"),
            position_ms: 1_234,
            is_playing: false,
        },
        Op::PlaybackAdvance {
            target_entry_id: id("c"),
            position_ms: 1_234,
            is_playing: false,
        },
    ] {
        let after =
            apply(&state(&["a", "b", "c", "d"], true, 0, 0), &operation, NOW).expect("apply");
        assert_eq!(ids(&after), ["c", "d"], "{operation:?}");
        assert_eq!(after.current_entry_id.as_deref(), Some("c"));
        assert_eq!(after.playback.position_ms, 1_234);
        assert!(!after.playback.is_playing);
    }
}

#[test]
fn select_position_is_clamped_to_duration() {
    let after = apply(
        &state(&["a"], false, 0, 0),
        &Op::PlaybackSelect {
            entry_id: id("a"),
            position_ms: DURATION_MS + 1,
            is_playing: true,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(after.playback.position_ms, DURATION_MS);
}

#[test]
fn select_unknown_target_is_rejected() {
    assert_eq!(
        apply(
            &state(&["a"], false, 0, 0),
            &Op::PlaybackAdvance {
                target_entry_id: id("z"),
                position_ms: 0,
                is_playing: true
            },
            NOW
        ),
        Err(StateError::EntryNotFound { entry_id: id("z") })
    );
}

#[test]
fn seek_only_applies_to_current_entry() {
    let before = state(&["a", "b"], true, 0, NOW);
    let after = apply(
        &before,
        &Op::PlaybackSeek {
            entry_id: id("a"),
            position_ms: 42_000,
        },
        NOW,
    )
    .expect("apply");
    assert_eq!(after.playback.position_ms, 42_000);

    assert_eq!(
        apply(
            &before,
            &Op::PlaybackSeek {
                entry_id: id("b"),
                position_ms: 1
            },
            NOW
        ),
        Err(StateError::SeekNotCurrent)
    );
}

#[test]
fn pausing_freezes_the_extrapolated_position() {
    let after = apply(
        &state(&["a"], true, 1_000, 0),
        &Op::PlaybackSetPlaying { is_playing: false },
        NOW,
    )
    .expect("apply");
    assert!(!after.playback.is_playing);
    assert_eq!(after.playback.position_ms, 1_000 + NOW);
}

#[test]
fn playing_an_empty_queue_is_rejected() {
    assert_eq!(
        apply(&empty(0), &Op::PlaybackSetPlaying { is_playing: true }, NOW),
        Err(StateError::NothingToPlay)
    );
}

#[test]
fn failed_transition_leaves_input_unchanged() {
    let before = state(&["a", "b"], true, 1_000, 0);
    let copy = before.clone();
    assert!(apply(&before, &Op::QueueRemove { entry_id: id("z") }, NOW).is_err());
    assert_eq!(before, copy);
}

#[test]
fn changes_queue_classification() {
    assert!(!Op::PlaybackSetPlaying { is_playing: true }.changes_queue());
    assert!(
        !Op::PlaybackSeek {
            entry_id: id("a"),
            position_ms: 0
        }
        .changes_queue()
    );
    assert!(
        Op::PlaybackAdvance {
            target_entry_id: id("a"),
            position_ms: 0,
            is_playing: true
        }
        .changes_queue()
    );
    assert!(Op::QueueRemove { entry_id: id("a") }.changes_queue());
}

// ---------------------------------------------------------------------------
// is_noop
// ---------------------------------------------------------------------------

#[test]
fn noop_detection() {
    // Playing "a" at 1_000ms as of t=0, so at NOW the position is 11_000ms.
    let s = state(&["a", "b"], true, 1_000, 0);

    assert!(is_noop(
        &s,
        &Op::PlaybackSetPlaying { is_playing: true },
        NOW
    ));
    assert!(!is_noop(
        &s,
        &Op::PlaybackSetPlaying { is_playing: false },
        NOW
    ));

    let advance = |target: &str| Op::PlaybackAdvance {
        target_entry_id: id(target),
        position_ms: 0,
        is_playing: true,
    };
    assert!(
        is_noop(&s, &advance("a"), NOW),
        "already on a, position ignored"
    );
    assert!(!is_noop(&s, &advance("b"), NOW));

    let seek = |position_ms| Op::PlaybackSeek {
        entry_id: id("a"),
        position_ms,
    };
    assert!(is_noop(&s, &seek(11_000 + 1_500), NOW));
    assert!(!is_noop(&s, &seek(11_000 + 1_501), NOW));

    let select = |position_ms| Op::PlaybackSelect {
        entry_id: id("a"),
        position_ms,
        is_playing: true,
    };
    assert!(is_noop(&s, &select(10_000), NOW));
    assert!(
        !is_noop(&s, &select(0), NOW),
        "restarting the song is a real change"
    );

    let replace_same = Op::QueueReplace {
        entries: vec![entry("a"), entry("b")],
        current_entry_id: Some(id("a")),
    };
    assert!(is_noop(&s, &replace_same, NOW));
    let replace_other = Op::QueueReplace {
        entries: vec![entry("a")],
        current_entry_id: Some(id("a")),
    };
    assert!(!is_noop(&s, &replace_other, NOW));

    assert!(!is_noop(
        &s,
        &Op::QueueMove {
            entry_id: id("a"),
            to_index: 0
        },
        NOW
    ));
}

// ---------------------------------------------------------------------------
// reinsert_unavailable
// ---------------------------------------------------------------------------

fn unavailable(list: &[&str]) -> HashSet<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn queue(list: &[&str]) -> Vec<attune_protocol::QueueEntry> {
    list.iter().map(|id| entry(id)).collect()
}

fn entry_ids(entries: &[attune_protocol::QueueEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.entry_id.as_str()).collect()
}

#[test]
fn reinsert_puts_unavailable_entries_after_their_predecessor() {
    let session = queue(&["a", "x", "b", "y", "z", "c"]);
    let local = queue(&["a", "b", "c", "new"]);
    let merged = reinsert_unavailable(&local, &session, &unavailable(&["x", "y", "z"]));
    assert_eq!(entry_ids(&merged), ["a", "x", "b", "y", "z", "c", "new"]);
}

#[test]
fn reinsert_follows_predecessor_when_user_reordered() {
    let session = queue(&["a", "b", "x", "c"]);
    let local = queue(&["a", "c", "b"]);
    let merged = reinsert_unavailable(&local, &session, &unavailable(&["x"]));
    assert_eq!(entry_ids(&merged), ["a", "c", "b", "x"]);
}

#[test]
fn reinsert_walks_back_past_removed_predecessors() {
    let session = queue(&["a", "b", "x", "c"]);
    let local = queue(&["a", "c"]); // user removed b
    let merged = reinsert_unavailable(&local, &session, &unavailable(&["x"]));
    assert_eq!(entry_ids(&merged), ["a", "x", "c"]);
}

#[test]
fn reinsert_without_anchor_goes_after_current() {
    let session = queue(&["gone", "x"]);
    let local = queue(&["new", "other"]);
    let merged = reinsert_unavailable(&local, &session, &unavailable(&["x"]));
    assert_eq!(entry_ids(&merged), ["new", "x", "other"]);
}

#[test]
fn reinsert_is_identity_without_unavailable_entries() {
    let session = queue(&["a", "b"]);
    let local = queue(&["b", "a", "c"]);
    assert_eq!(
        reinsert_unavailable(&local, &session, &HashSet::new()),
        local
    );
}
