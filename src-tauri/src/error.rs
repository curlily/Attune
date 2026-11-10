use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] jam_core::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Join an error and all of its sources: "outer: middle: root cause".
///
/// `#[error(transparent)]` forwards both Display and source() to the wrapped
/// error, so starting from `self` does not repeat the top-level message.
fn message_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// Commands reject with the full cause chain as a plain string.
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&message_chain(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_full_cause_chain() {
        let error: AppError = jam_core::Error::KillFailed {
            exe: "TIDAL.exe".to_owned(),
            source: std::io::Error::other("access denied"),
        }
        .into();

        let value = serde_json::to_value(&error).expect("serialize error");

        assert_eq!(
            value,
            "failed to run the process termination command for TIDAL.exe: access denied"
        );
    }

    #[test]
    fn sourceless_error_has_single_message() {
        let error: AppError = jam_core::Error::NotConnected.into();
        let value = serde_json::to_value(&error).expect("serialize error");

        assert_eq!(value, "not connected to a music provider");
    }
}
