#![forbid(unsafe_code)]

pub mod calendar;
pub mod config;
pub mod gateway;
mod missing;
pub mod model;
pub mod relay;
mod subforge;
mod subforge_events;

/// Errors intentionally contain no request URLs, credentials, or provider data.
#[derive(Debug)]
pub enum Error {
    Configuration(&'static str),
    Upstream,
    Limit,
    InvalidData,
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(name) => write!(f, "invalid configuration: {name}"),
            Self::Upstream => f.write_str("upstream request failed"),
            Self::Limit => f.write_str("resource limit exceeded"),
            Self::InvalidData => f.write_str("invalid provider data"),
            Self::Io(_) => f.write_str("local I/O failed"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
