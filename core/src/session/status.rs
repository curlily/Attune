//! Sync state machine and the status published to the UI.

use serde::Serialize;

use crate::protocol::{Participant, ParticipantStatus, TrackDescriptor};

/// How this client relates to the session right now.
///
/// ```text
/// Resyncing ─▶ Synced ─▶ WaitingForNextSong ─▶ Resyncing   (song unavailable here)
/// Synced/Waiting/Error ─▶ SteppedOut ─▶ Resyncing           (user steps out / back)
/// Synced/Waiting ─▶ Error ─▶ Resyncing                       (bridge failure, manual resync)
/// any ─▶ Reconnecting ─▶ Resyncing | SteppedOut              (connection drop)
/// any ─▶ Ended
/// ```
///
/// Only `Synced` applies remote operations to the provider *and* publishes
/// local ones. Every other state keeps the session mirror up to date only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    /// Applying the session's state to the provider.
    Resyncing,
    /// Following the session.
    Synced,
    /// The provider can't play the current song; rejoins automatically when
    /// the session moves on.
    WaitingForNextSong,
    /// The user chose to stop following; returns only on resync.
    SteppedOut,
    /// Applying something failed unexpectedly; needs a manual resync.
    Error,
    /// The connection dropped; rejoining in the background.
    Reconnecting,
    /// Left, or the session ended. Final.
    Ended,
}

impl SyncState {
    /// Whether `self -> next` is a legal transition. Staying in the same
    /// state is not a transition and is handled by the caller.
    pub fn can_become(self, next: SyncState) -> bool {
        use SyncState::*;
        match (self, next) {
            (Ended, _) => false,
            (_, Ended) => true,
            (Resyncing, Synced | WaitingForNextSong | SteppedOut | Error | Reconnecting) => true,
            (Synced, Resyncing | WaitingForNextSong | SteppedOut | Error | Reconnecting) => true,
            (WaitingForNextSong, Resyncing | SteppedOut | Error | Reconnecting) => true,
            (SteppedOut, Resyncing | Reconnecting) => true,
            (Error, Resyncing | SteppedOut | Reconnecting) => true,
            (Reconnecting, Resyncing | SteppedOut) => true,
            _ => false,
        }
    }

    /// What to report to the other participants; `None` while there is no
    /// connection to report over.
    pub fn participant_status(self) -> Option<ParticipantStatus> {
        match self {
            Self::Resyncing => Some(ParticipantStatus::Resyncing),
            Self::Synced => Some(ParticipantStatus::Synced),
            Self::WaitingForNextSong => Some(ParticipantStatus::WaitingForNextSong),
            Self::SteppedOut => Some(ParticipantStatus::Detached),
            Self::Error => Some(ParticipantStatus::Error),
            Self::Reconnecting | Self::Ended => None,
        }
    }
}

/// An upcoming (or current) song someone in the session can't play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnavailableWarning {
    pub entry_id: String,
    pub title: String,
    pub artists: Vec<String>,
    /// 0 = playing now.
    pub songs_ahead: usize,
    /// Display names of the participants who can't play it.
    pub missing_for: Vec<String>,
}

/// Everything the UI shows about the session. Contains no secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub session_id: String,
    pub sync: SyncState,
    /// Last session sequence applied here.
    pub sequence: u64,
    pub current: Option<TrackDescriptor>,
    pub queue_length: usize,
    pub participants: Vec<Participant>,
    pub warnings: Vec<UnavailableWarning>,
}

/// One-off messages for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Notice {
    /// A queue edit made here conflicted with someone else's and was undone.
    QueueEditUndone,
    /// This provider can't play the current song; waiting for the next one.
    SongUnavailable {
        title: String,
    },
    /// Back in sync after a resync or reconnect.
    Resynced,
    /// Following stopped because applying a change failed.
    Desynchronized {
        message: String,
    },
    ServerError {
        message: String,
    },
    /// The session is over for this client.
    Ended {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use SyncState::*;

    const ALL: [SyncState; 7] = [
        Resyncing,
        Synced,
        WaitingForNextSong,
        SteppedOut,
        Error,
        Reconnecting,
        Ended,
    ];

    #[test]
    fn transition_table() {
        let allowed = [
            (
                Resyncing,
                &[
                    Synced,
                    WaitingForNextSong,
                    SteppedOut,
                    Error,
                    Reconnecting,
                    Ended,
                ][..],
            ),
            (
                Synced,
                &[
                    Resyncing,
                    WaitingForNextSong,
                    SteppedOut,
                    Error,
                    Reconnecting,
                    Ended,
                ],
            ),
            (
                WaitingForNextSong,
                &[Resyncing, SteppedOut, Error, Reconnecting, Ended],
            ),
            (SteppedOut, &[Resyncing, Reconnecting, Ended]),
            (Error, &[Resyncing, SteppedOut, Reconnecting, Ended]),
            (Reconnecting, &[Resyncing, SteppedOut, Ended]),
            (Ended, &[]),
        ];
        for (from, targets) in allowed {
            for to in ALL {
                assert_eq!(
                    from.can_become(to),
                    targets.contains(&to),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn returning_to_synced_always_goes_through_resyncing() {
        for from in [WaitingForNextSong, SteppedOut, Error, Reconnecting] {
            assert!(
                !from.can_become(Synced),
                "{from:?} -> Synced must resync first"
            );
        }
    }

    #[test]
    fn participant_status_mapping() {
        assert_eq!(
            SteppedOut.participant_status(),
            Some(ParticipantStatus::Detached)
        );
        assert_eq!(Reconnecting.participant_status(), None);
        assert_eq!(Ended.participant_status(), None);
    }
}
