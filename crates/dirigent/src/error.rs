//! Errors and their exit codes.

use std::path::PathBuf;
use std::process::ExitCode;

/// Something stopped a command. Every variant maps to a documented exit code.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Invalid(dirigera::ChangeError),
    #[error("not paired with a hub yet; run `dirigent pair`")]
    NotPaired,
    #[error("the hub no longer accepts dirigent; run `dirigent pair` to pair again")]
    TokenRevoked,
    #[error(
        "the hub presented another certificate than at pairing; after a factory reset of the \
         hub, run `dirigent pair` to pair again"
    )]
    CertificateChanged,
    /// The change was sent, but did not show in time; the text says what was seen.
    #[error("{0}")]
    NotConfirmed(String),
    #[error("{0}")]
    Unreachable(String),
    #[error(transparent)]
    Api(dirigera::Error),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is unreadable ({message}); fix or delete it")]
    Corrupt { path: PathBuf, message: String },
    /// An error the output already explained; it sets the exit code without being repeated.
    #[error(transparent)]
    Shown(Box<AppError>),
}

impl AppError {
    /// 2 usage, 4 pairing needed, 5 change not confirmed, 1 anything else.
    pub fn exit_code(&self) -> ExitCode {
        ExitCode::from(match self {
            Self::Usage(_) | Self::Invalid(_) => 2,
            Self::NotPaired | Self::TokenRevoked | Self::CertificateChanged => 4,
            Self::NotConfirmed(_) => 5,
            Self::Unreachable(_) | Self::Api(_) | Self::Io { .. } | Self::Corrupt { .. } => 1,
            Self::Shown(error) => return error.exit_code(),
        })
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<dirigera::Error> for AppError {
    fn from(error: dirigera::Error) -> Self {
        match error {
            dirigera::Error::Unauthorized => Self::TokenRevoked,
            dirigera::Error::CertificateChanged { .. } => Self::CertificateChanged,
            dirigera::Error::Change(change) => Self::Invalid(change),
            other => Self::Api(other),
        }
    }
}

impl From<dirigera::ChangeError> for AppError {
    fn from(error: dirigera::ChangeError) -> Self {
        Self::Invalid(error)
    }
}
