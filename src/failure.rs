//! The failure categories both front ends branch on. The product contract for
//! the categories is docs/browse-collab-cli.md; the exit code each one carries
//! is docs/configuration.md.

use std::fmt;

/// The stable category a failure belongs to. The CLI prints it as `error`;
/// the TUI shows the detail on its status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    InvalidInput,
    NotFound,
    Forbidden,
    Network,
    TimeoutUnknown,
    RelayRejected,
}

impl Category {
    /// The string the CLI prints for `error`.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::InvalidInput => "invalid_input",
            Category::NotFound => "not_found",
            Category::Forbidden => "forbidden",
            Category::Network => "network",
            Category::TimeoutUnknown => "timeout_unknown",
            Category::RelayRejected => "relay_rejected",
        }
    }
}

/// One failure: the category a caller branches on, and the reason a person
/// reads. It never carries a key or an authorization header.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub category: Category,
    pub detail: String,
}

impl Failure {
    pub fn new(category: Category, detail: impl Into<String>) -> Self {
        Self {
            category,
            detail: detail.into(),
        }
    }

    pub fn invalid_input(detail: impl Into<String>) -> Self {
        Self::new(Category::InvalidInput, detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(Category::NotFound, detail)
    }

    pub fn network(detail: impl Into<String>) -> Self {
        Self::new(Category::Network, detail)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}
