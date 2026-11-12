//! Pure state validation and operation transitions.
//!
//! The server uses these functions to decide what is accepted, and clients use
//! the same functions to mirror accepted operations, so both sides compute
//! identical results. Semantics follow the provider adapters' `applyOperation`
//! (`core/src/adapters/*.js`).
//!
//! # Invariants of a valid [`SessionMusicState`]
//!
//! - At most [`MAX_QUEUE_ENTRIES`] entries with unique, non-empty entry IDs.
//!   Duplicate *tracks* are allowed; they have different entry IDs.
//! - Non-empty queue: `currentEntryId == queue[0].entryId`.
//! - Empty queue: `currentEntryId == null`, not playing, position 0.
//!
//! # Time
//!
//! Every transition takes `now_ms` (the server clock on the server, the
//! operation's `acceptedAtMs` on clients). Playback is first extrapolated to
//! `now_ms`, so the resulting `positionCapturedAtMs` is always `now_ms`.

use std::collections::HashSet;

use crate::model::{
    QueueEntry, SessionMusicState, SessionOperation, SessionPlayback, TrackDescriptor,
};

pub const MAX_QUEUE_ENTRIES: usize = 1000;
pub const MAX_ENTRY_ID_LEN: usize = 64;
/// Maximum byte length of titles, artist names and album names.
pub const MAX_TEXT_LEN: usize = 512;
/// Maximum byte length of a cover art URL.
pub const MAX_COVER_URL_LEN: usize = 2048;
pub const MAX_ARTISTS: usize = 32;
pub const MAX_ISRC_LEN: usize = 32;
pub const MAX_DURATION_MS: u64 = 24 * 60 * 60 * 1000;
/// Positions this close to the expected position count as "the same" when
/// deciding whether an operation changes anything.
pub const POSITION_TOLERANCE_MS: u64 = 1_500;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    #[error("queue has {len} entries; the maximum is {max}", max = MAX_QUEUE_ENTRIES)]
    QueueTooLong { len: usize },

    #[error("queue entry id is empty or longer than {max} bytes", max = MAX_ENTRY_ID_LEN)]
    InvalidEntryId,

    #[error("queue entry {entry_id} appears more than once")]
    DuplicateEntry { entry_id: String },

    #[error("track field {field} is empty, too long or contains control characters")]
    InvalidTrackField { field: &'static str },

    #[error("the current entry must be the first queue entry")]
    CurrentNotFirst,

    #[error("an empty queue must be stopped at position 0 with no current entry")]
    InvalidEmptyState,

    #[error("queue entry {entry_id} was not found")]
    EntryNotFound { entry_id: String },

    #[error("index {index} is out of range for a queue of {len} entries")]
    IndexOutOfRange { index: usize, len: usize },

    #[error("the current entry can only be moved to index 0")]
    CannotMoveCurrent,

    #[error("only the current entry can be seeked")]
    SeekNotCurrent,

    #[error("cannot start playback of an empty queue")]
    NothingToPlay,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn check_text(value: &str, field: &'static str, required: bool) -> Result<(), StateError> {
    let invalid = value.len() > MAX_TEXT_LEN
        || value.chars().any(char::is_control)
        || (required && value.trim().is_empty());
    if invalid {
        Err(StateError::InvalidTrackField { field })
    } else {
        Ok(())
    }
}

pub fn validate_track(track: &TrackDescriptor) -> Result<(), StateError> {
    check_text(&track.title, "title", true)?;
    if track.artists.len() > MAX_ARTISTS {
        return Err(StateError::InvalidTrackField { field: "artists" });
    }
    for artist in &track.artists {
        check_text(artist, "artists", true)?;
    }
    if let Some(album) = &track.album {
        check_text(album, "album", false)?;
    }
    if let Some(isrc) = &track.isrc {
        if isrc.len() > MAX_ISRC_LEN || isrc.chars().any(char::is_control) {
            return Err(StateError::InvalidTrackField { field: "isrc" });
        }
    }
    if track.duration_ms.is_some_and(|ms| ms > MAX_DURATION_MS) {
        return Err(StateError::InvalidTrackField {
            field: "durationMs",
        });
    }
    if let Some(url) = &track.cover_url {
        if !url.starts_with("https://")
            || url.len() > MAX_COVER_URL_LEN
            || url.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(StateError::InvalidTrackField { field: "coverUrl" });
        }
    }
    Ok(())
}

pub fn validate_entry(entry: &QueueEntry) -> Result<(), StateError> {
    if entry.entry_id.is_empty() || entry.entry_id.len() > MAX_ENTRY_ID_LEN {
        return Err(StateError::InvalidEntryId);
    }
    if let Some(name) = &entry.added_by {
        check_text(name, "addedBy", true)?;
    }
    validate_track(&entry.track)
}

/// Record who queued the entries an operation adds: entries already in the
/// queue keep what they have, new ones get `added_by`. The server applies it
/// with the sender's name, so nobody can queue in someone else's name;
/// clients apply it with their own so their edits match what the server
/// accepts.
pub fn stamp_added_by(
    state: &SessionMusicState,
    operation: SessionOperation,
    added_by: Option<&str>,
) -> SessionOperation {
    let stamp = |mut entry: QueueEntry| {
        entry.added_by = match state.queue.iter().find(|e| e.entry_id == entry.entry_id) {
            Some(existing) => existing.added_by.clone(),
            None => added_by.map(str::to_owned),
        };
        entry
    };
    match operation {
        SessionOperation::QueueInsert { entry, index } => SessionOperation::QueueInsert {
            entry: stamp(entry),
            index,
        },
        SessionOperation::QueueReplace {
            entries,
            current_entry_id,
            playback,
        } => SessionOperation::QueueReplace {
            entries: entries.into_iter().map(stamp).collect(),
            current_entry_id,
            playback,
        },
        other => other,
    }
}

pub fn validate_state(state: &SessionMusicState) -> Result<(), StateError> {
    if state.queue.len() > MAX_QUEUE_ENTRIES {
        return Err(StateError::QueueTooLong {
            len: state.queue.len(),
        });
    }

    let mut seen = HashSet::with_capacity(state.queue.len());
    for entry in &state.queue {
        validate_entry(entry)?;
        if !seen.insert(entry.entry_id.as_str()) {
            return Err(StateError::DuplicateEntry {
                entry_id: entry.entry_id.clone(),
            });
        }
    }

    match state.queue.first() {
        None => {
            let playback = &state.playback;
            if state.current_entry_id.is_some() || playback.is_playing || playback.position_ms != 0
            {
                return Err(StateError::InvalidEmptyState);
            }
        }
        Some(first) => {
            if state.current_entry_id.as_deref() != Some(first.entry_id.as_str()) {
                return Err(StateError::CurrentNotFirst);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

fn current_duration(state: &SessionMusicState) -> Option<u64> {
    state
        .queue
        .first()
        .and_then(|entry| entry.track.duration_ms)
}

/// Estimated playback position at `now_ms`, clamped to the current track's
/// duration when it is known.
pub fn position_at(state: &SessionMusicState, now_ms: u64) -> u64 {
    let playback = &state.playback;
    let mut position = playback.position_ms;
    if playback.is_playing {
        position = position.saturating_add(now_ms.saturating_sub(playback.position_captured_at_ms));
    }
    match current_duration(state) {
        Some(duration) => position.min(duration),
        None => position,
    }
}

/// The same state with playback re-captured at `now_ms`.
pub fn at_time(state: &SessionMusicState, now_ms: u64) -> SessionMusicState {
    let mut next = state.clone();
    next.playback.position_ms = position_at(state, now_ms);
    next.playback.position_captured_at_ms = now_ms;
    next
}

fn empty_state(now_ms: u64) -> SessionMusicState {
    SessionMusicState {
        queue: Vec::new(),
        current_entry_id: None,
        playback: SessionPlayback {
            is_playing: false,
            position_ms: 0,
            position_captured_at_ms: now_ms,
        },
    }
}

// ---------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------

fn index_of(queue: &[QueueEntry], entry_id: &str) -> Result<usize, StateError> {
    queue
        .iter()
        .position(|entry| entry.entry_id == entry_id)
        .ok_or_else(|| StateError::EntryNotFound {
            entry_id: entry_id.to_owned(),
        })
}

/// Make `queue[0]` current, restarting it at position 0.
fn restart_at_front(state: &mut SessionMusicState) {
    state.current_entry_id = state.queue.first().map(|entry| entry.entry_id.clone());
    state.playback.position_ms = 0;
}

/// Apply one operation, returning the new state.
///
/// Pure and transactional: on error the input is untouched and nothing is
/// returned. The result is validated, so an `Ok` state always satisfies the
/// module invariants.
///
/// Boundary semantics:
/// - `queue.replace`: entries before `currentEntryId` are history and are
///   dropped. A missing `currentEntryId` means `entries[0]`. If the current
///   entry changes, playback restarts at `playback` (position and play
///   state), or at 0 and playing without it.
/// - `queue.insert`: `index` must be `0..=len`. Inserting at 0 makes the new
///   entry current (position 0, play state unchanged).
/// - `queue.remove`: removing the current entry makes the next one current at
///   position 0; removing the last entry yields the empty state.
/// - `queue.move`: `toIndex` must be `0..len`. The current entry can only be
///   moved to 0 (a no-op). Moving another entry to 0 makes it current.
/// - `playback.select` / `playback.advance`: entries before the target are
///   dropped as history; the target becomes current with the given position
///   and play state.
/// - `playback.seek`: only valid for the current entry.
/// - `playback.setPlaying`: absolute; starting an empty queue is an error.
pub fn apply(
    state: &SessionMusicState,
    operation: &SessionOperation,
    now_ms: u64,
) -> Result<SessionMusicState, StateError> {
    let mut next = at_time(state, now_ms);

    match operation {
        SessionOperation::QueueReplace {
            entries,
            current_entry_id,
            playback,
        } => {
            let start = match current_entry_id {
                None => 0,
                Some(id) => index_of(entries, id)?,
            };
            if entries.is_empty() {
                next = empty_state(now_ms);
            } else {
                next.queue = entries[start..].to_vec();
                let new_current = Some(next.queue[0].entry_id.clone());
                if new_current != next.current_entry_id {
                    restart_at_front(&mut next);
                    next.playback.is_playing = true;
                    if let Some(playback) = playback {
                        next.playback.position_ms = playback.position_ms;
                        next.playback.is_playing = playback.is_playing;
                    }
                }
            }
        }

        SessionOperation::QueueInsert { entry, index } => {
            let len = next.queue.len();
            if *index > len {
                return Err(StateError::IndexOutOfRange { index: *index, len });
            }
            next.queue.insert(*index, entry.clone());
            if *index == 0 {
                restart_at_front(&mut next);
            }
        }

        SessionOperation::QueueRemove { entry_id } => {
            let index = index_of(&next.queue, entry_id)?;
            next.queue.remove(index);
            if next.queue.is_empty() {
                next = empty_state(now_ms);
            } else if index == 0 {
                restart_at_front(&mut next);
            }
        }

        SessionOperation::QueueMove { entry_id, to_index } => {
            let from = index_of(&next.queue, entry_id)?;
            let len = next.queue.len();
            if *to_index >= len {
                return Err(StateError::IndexOutOfRange {
                    index: *to_index,
                    len,
                });
            }
            if from == 0 && *to_index != 0 {
                return Err(StateError::CannotMoveCurrent);
            }
            let entry = next.queue.remove(from);
            next.queue.insert(*to_index, entry);
            if *to_index == 0 && from != 0 {
                restart_at_front(&mut next);
            }
        }

        SessionOperation::PlaybackSelect {
            entry_id,
            position_ms,
            is_playing,
        }
        | SessionOperation::PlaybackAdvance {
            target_entry_id: entry_id,
            position_ms,
            is_playing,
        } => {
            let index = index_of(&next.queue, entry_id)?;
            next.queue.drain(..index);
            next.current_entry_id = Some(entry_id.clone());
            next.playback.position_ms = *position_ms;
            next.playback.is_playing = *is_playing;
        }

        SessionOperation::PlaybackSeek {
            entry_id,
            position_ms,
        } => {
            if next.current_entry_id.as_deref() != Some(entry_id.as_str()) {
                return Err(StateError::SeekNotCurrent);
            }
            next.playback.position_ms = *position_ms;
        }

        SessionOperation::PlaybackSetPlaying { is_playing } => {
            if *is_playing && next.queue.is_empty() {
                return Err(StateError::NothingToPlay);
            }
            next.playback.is_playing = *is_playing;
        }
    }

    if let Some(duration) = current_duration(&next) {
        next.playback.position_ms = next.playback.position_ms.min(duration);
    }
    validate_state(&next)?;
    Ok(next)
}

/// Whether applying `operation` would leave the state effectively unchanged.
///
/// Used by the server to ack duplicates without assigning a sequence (for
/// example every client reporting the same song-end advance), and by clients
/// to drop local echoes of remote changes.
pub fn is_noop(state: &SessionMusicState, operation: &SessionOperation, now_ms: u64) -> bool {
    let current = state.current_entry_id.as_deref();
    let playing = state.playback.is_playing;
    let near_position =
        |position: u64| position.abs_diff(position_at(state, now_ms)) <= POSITION_TOLERANCE_MS;

    match operation {
        SessionOperation::PlaybackSetPlaying { is_playing } => playing == *is_playing,
        // Advancing is about *which* song plays; the reported position is
        // whatever each client had reached when it noticed the change.
        SessionOperation::PlaybackAdvance {
            target_entry_id,
            is_playing,
            ..
        } => current == Some(target_entry_id.as_str()) && playing == *is_playing,
        SessionOperation::PlaybackSelect {
            entry_id,
            position_ms,
            is_playing,
        } => {
            current == Some(entry_id.as_str())
                && playing == *is_playing
                && near_position(*position_ms)
        }
        SessionOperation::PlaybackSeek {
            entry_id,
            position_ms,
        } => current == Some(entry_id.as_str()) && near_position(*position_ms),
        SessionOperation::QueueReplace { .. } => {
            apply(state, operation, now_ms).is_ok_and(|next| {
                next.queue == state.queue && next.current_entry_id == state.current_entry_id
            })
        }
        SessionOperation::QueueInsert { .. }
        | SessionOperation::QueueRemove { .. }
        | SessionOperation::QueueMove { .. } => false,
    }
}

/// Re-insert entries this client could not load into its provider, so that
/// publishing its local queue does not delete them for everyone.
///
/// `local` is the queue read from the provider, `session` the last known
/// session queue and `unavailable` the entry IDs the provider could not
/// resolve. Each missing unavailable entry is placed after the nearest entry
/// that preceded it in `session` and still exists; with no such anchor it goes
/// right after the current entry. Relative order of re-inserted entries is
/// preserved.
pub fn reinsert_unavailable(
    local: &[QueueEntry],
    session: &[QueueEntry],
    unavailable: &HashSet<String>,
) -> Vec<QueueEntry> {
    let mut merged = local.to_vec();
    let mut insert_after: Option<usize> = None;

    for entry in session {
        if let Some(position) = merged.iter().position(|e| e.entry_id == entry.entry_id) {
            insert_after = Some(position);
        } else if unavailable.contains(&entry.entry_id) {
            let at = match insert_after {
                Some(position) => position + 1,
                None => merged.len().min(1),
            };
            merged.insert(at, entry.clone());
            insert_after = Some(at);
        }
    }
    merged
}
