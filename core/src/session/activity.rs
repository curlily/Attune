//! What other people did in the session, for notifications ("Paused by
//! Lily", "Sam joined"). Built from others' accepted operations and from
//! changes to the participant list; this client's own changes never make
//! one (the server doesn't echo them).

use serde::Serialize;

use crate::protocol::{Participant, RepeatMode, SessionMusicState, SessionOperation};

/// Something someone else did. `by` is their display name; `None` for a
/// change the server made itself (e.g. skipping a song nobody can play).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Activity {
    Paused {
        by: Option<String>,
    },
    Resumed {
        by: Option<String>,
    },
    Skipped {
        by: Option<String>,
    },
    /// Picked a queued song to play now.
    Played {
        by: Option<String>,
        title: String,
    },
    Seeked {
        by: Option<String>,
    },
    Added {
        by: Option<String>,
        title: String,
    },
    Removed {
        by: Option<String>,
        title: String,
    },
    /// Moved songs around or replaced the queue.
    QueueChanged {
        by: Option<String>,
    },
    Repeat {
        by: Option<String>,
        on: bool,
    },
    Joined {
        name: String,
    },
    Left {
        name: String,
    },
    /// The session has a new host; `you`: it's this client.
    NewHost {
        name: String,
        you: bool,
    },
}

/// What `operation`, accepted from someone else, looks like to the user.
/// `mirror` is the session as it was before it; `song_ending`: the current
/// song is about to end, so an advance is the song finishing, not a skip.
pub fn from_operation(
    operation: &SessionOperation,
    by: Option<String>,
    mirror: &SessionMusicState,
    song_ending: bool,
) -> Option<Activity> {
    let title = |entry_id: &str| {
        mirror
            .queue
            .iter()
            .find(|entry| entry.entry_id == entry_id)
            .map(|entry| entry.track.title.clone())
    };
    Some(match operation {
        SessionOperation::PlaybackSetPlaying { is_playing } => {
            if *is_playing == mirror.playback.is_playing {
                return None;
            }
            if *is_playing {
                Activity::Resumed { by }
            } else {
                Activity::Paused { by }
            }
        }
        // Every app moves on when a song ends: nobody did anything.
        SessionOperation::PlaybackAdvance { .. } if song_ending => return None,
        SessionOperation::PlaybackAdvance { .. } => Activity::Skipped { by },
        SessionOperation::PlaybackSelect { entry_id, .. } => {
            if mirror.current_entry_id.as_deref() == Some(entry_id.as_str()) {
                Activity::Seeked { by }
            } else {
                Activity::Played {
                    by,
                    title: title(entry_id)?,
                }
            }
        }
        SessionOperation::PlaybackSeek { .. } => Activity::Seeked { by },
        SessionOperation::QueueInsert { entry, .. } => Activity::Added {
            by,
            title: entry.track.title.clone(),
        },
        SessionOperation::QueueRemove { entry_id } => Activity::Removed {
            by,
            title: title(entry_id)?,
        },
        SessionOperation::QueueMove { .. } | SessionOperation::QueueReplace { .. } => {
            Activity::QueueChanged { by }
        }
        SessionOperation::PlaybackSetRepeat { mode } => {
            if *mode == mirror.repeat {
                return None;
            }
            Activity::Repeat {
                by,
                on: *mode == RepeatMode::One,
            }
        }
    })
}

/// Who joined and who left between two participant lists, leaving out
/// this client. Someone who only lost their connection is still listed
/// (`connected: false`) during the server's grace period, so a short drop
/// makes nothing.
pub fn participant_changes(
    before: &[Participant],
    after: &[Participant],
    own_client_id: &str,
) -> Vec<Activity> {
    let listed = |list: &[Participant], id: &str| list.iter().any(|p| p.client_id == id);
    let joined = after
        .iter()
        .filter(|p| p.client_id != own_client_id && !listed(before, &p.client_id))
        .map(|p| Activity::Joined {
            name: p.display_name.clone(),
        });
    let left = before
        .iter()
        .filter(|p| p.client_id != own_client_id && !listed(after, &p.client_id))
        .map(|p| Activity::Left {
            name: p.display_name.clone(),
        });
    joined.chain(left).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ParticipantStatus, QueueEntry, SessionPlayback, TrackDescriptor};

    fn entry(id: &str, title: &str) -> QueueEntry {
        QueueEntry {
            entry_id: id.to_owned(),
            track: TrackDescriptor {
                title: title.to_owned(),
                artists: Vec::new(),
                isrc: None,
                album: None,
                duration_ms: None,
                cover_url: None,
                provider: None,
                provider_track_id: None,
            },
            added_by: None,
        }
    }

    fn mirror(playing: bool) -> SessionMusicState {
        SessionMusicState {
            queue: vec![entry("a", "First"), entry("b", "Second")],
            current_entry_id: Some("a".to_owned()),
            playback: SessionPlayback {
                is_playing: playing,
                position_ms: 0,
                position_captured_at_ms: 0,
            },
            repeat: RepeatMode::Off,
        }
    }

    fn lily() -> Option<String> {
        Some("Lily".to_owned())
    }

    #[test]
    fn pausing_and_resuming_say_who() {
        let pause = SessionOperation::PlaybackSetPlaying { is_playing: false };
        let play = SessionOperation::PlaybackSetPlaying { is_playing: true };
        assert_eq!(
            from_operation(&pause, lily(), &mirror(true), false),
            Some(Activity::Paused { by: lily() })
        );
        assert_eq!(
            from_operation(&play, lily(), &mirror(false), false),
            Some(Activity::Resumed { by: lily() })
        );
        // Already paused: nothing changed.
        assert_eq!(from_operation(&pause, lily(), &mirror(false), false), None);
    }

    #[test]
    fn an_advance_is_a_skip_unless_the_song_is_ending() {
        let advance = SessionOperation::PlaybackAdvance {
            target_entry_id: "b".to_owned(),
            position_ms: 0,
            is_playing: true,
        };
        assert_eq!(
            from_operation(&advance, lily(), &mirror(true), false),
            Some(Activity::Skipped { by: lily() })
        );
        assert_eq!(from_operation(&advance, lily(), &mirror(true), true), None);
        // The server skipping a song nobody can play.
        assert_eq!(
            from_operation(&advance, None, &mirror(true), false),
            Some(Activity::Skipped { by: None })
        );
    }

    #[test]
    fn selecting_names_the_song_and_queue_edits_name_theirs() {
        let select = |id: &str| SessionOperation::PlaybackSelect {
            entry_id: id.to_owned(),
            position_ms: 0,
            is_playing: true,
        };
        assert_eq!(
            from_operation(&select("b"), lily(), &mirror(true), false),
            Some(Activity::Played {
                by: lily(),
                title: "Second".to_owned()
            })
        );
        // The current song again, elsewhere in it.
        assert_eq!(
            from_operation(&select("a"), lily(), &mirror(true), false),
            Some(Activity::Seeked { by: lily() })
        );
        let insert = SessionOperation::QueueInsert {
            entry: entry("c", "Third"),
            index: 2,
        };
        assert_eq!(
            from_operation(&insert, lily(), &mirror(true), false),
            Some(Activity::Added {
                by: lily(),
                title: "Third".to_owned()
            })
        );
        let remove = SessionOperation::QueueRemove {
            entry_id: "b".to_owned(),
        };
        assert_eq!(
            from_operation(&remove, lily(), &mirror(true), false),
            Some(Activity::Removed {
                by: lily(),
                title: "Second".to_owned()
            })
        );
    }

    #[test]
    fn repeat_reports_only_a_change() {
        let one = SessionOperation::PlaybackSetRepeat {
            mode: RepeatMode::One,
        };
        assert_eq!(
            from_operation(&one, lily(), &mirror(true), false),
            Some(Activity::Repeat {
                by: lily(),
                on: true
            })
        );
        let mut repeating = mirror(true);
        repeating.repeat = RepeatMode::One;
        assert_eq!(from_operation(&one, lily(), &repeating, false), None);
    }

    fn person(id: &str, name: &str, connected: bool) -> Participant {
        Participant {
            client_id: id.to_owned(),
            display_name: name.to_owned(),
            provider: None,
            status: ParticipantStatus::Synced,
            connected,
            unavailable_entry_ids: Vec::new(),
        }
    }

    #[test]
    fn joins_and_leaves_leave_out_this_client_and_short_drops() {
        let me = person("me", "Me", true);
        let sam = person("sam", "Sam", true);
        let alex = person("alex", "Alex", true);

        let changes = participant_changes(
            &[me.clone(), alex.clone()],
            &[me.clone(), sam.clone()],
            "me",
        );
        assert_eq!(
            changes,
            vec![
                Activity::Joined {
                    name: "Sam".to_owned()
                },
                Activity::Left {
                    name: "Alex".to_owned()
                },
            ]
        );
        // Lost the connection but still listed: nothing.
        let dropped = person("sam", "Sam", false);
        assert!(participant_changes(&[me.clone(), sam], &[me.clone(), dropped], "me").is_empty());
        // Our own seat appearing (joining) isn't news.
        assert!(participant_changes(&[], &[me], "me").is_empty());
    }
}
