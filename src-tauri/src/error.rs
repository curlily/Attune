use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] jam_core::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl AppError {
    fn code(&self) -> &'static str {
        match self {
            Self::Core(error) => error.code(),
            Self::Io(_) => "io",
        }
    }
}

/// Shape the frontend receives when a command rejects: `{ code, message }`.
#[derive(Serialize)]
struct WireError {
    code: &'static str,
    message: String,
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

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        WireError {
            code: self.code(),
            message: message_chain(self),
        }
        .serialize(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_code_and_full_cause_chain() {
        let error: AppError = jam_core::Error::KillFailed {
            exe: "TIDAL.exe".to_owned(),
            source: std::io::Error::other("access denied"),
        }
        .into();

        let value = serde_json::to_value(&error).expect("serialize error");

        assert_eq!(value["code"], "killFailed");
        assert_eq!(
            value["message"],
            "failed to run the process termination command for TIDAL.exe: access denied"
        );
    }

    #[test]
    fn sourceless_error_has_single_message() {
        let error: AppError = jam_core::Error::NotConnected.into();
        let value = serde_json::to_value(&error).expect("serialize error");

        assert_eq!(value["code"], "notConnected");
        assert_eq!(value["message"], "not connected to a music provider");
    }
}
