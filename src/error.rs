//! Error types with exit-code mapping.
//!
//! Exit code convention:
//!   0 — success
//!   1 — user error (bad args, unknown iface, unsupported feature)
//!   2 — system error (netlink failure, no permission, parse failure)

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    UserError(String),

    #[error("system error: {0}")]
    SystemError(String),

    #[error("config parse error: {0}")]
    ParseError(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("`ip link` failed: {0}")]
    IpLinkFailed(String),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::UserError(_) => 1,
            _ => 2,
        }
    }
}
