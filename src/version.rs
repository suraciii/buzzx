//! The build identity: the version this binary claims, where its source came
//! from, and how it was installed. build.rs freezes the inputs; the
//! derivation rules are the version vocabulary table in
//! docs/versioning-and-updates.md. Nothing here runs Git or reads a clock.

include!(concat!(env!("OUT_DIR"), "/version_metadata.rs"));

use std::sync::LazyLock;

/// How the binary reached the machine. A source build is provenance, not a
/// compatibility failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    Source,
    Prebuilt,
}

impl InstallKind {
    /// The word the CLI prints and the update check reports.
    pub fn as_str(self) -> &'static str {
        match self {
            InstallKind::Source => "source",
            InstallKind::Prebuilt => "prebuilt",
        }
    }
}

/// The release channel a build belongs to. A dirty or untagged build is
/// always `Dev`: it must never be compared against a stable release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Prerelease,
    Dev,
}

impl Channel {
    /// The word the CLI prints and the update check reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Prerelease => "prerelease",
            Channel::Dev => "dev",
        }
    }
}

/// The version this build reports: the exact tag's package version, or the
/// `0.0.0` development identity, plus `.dirty` when the checkout was not
/// clean.
pub fn version() -> &'static str {
    static VERSION: LazyLock<String> =
        LazyLock::new(|| display_version(PKG_VERSION, GIT_TAG, GIT_BRANCH, GIT_COMMIT, GIT_DIRTY));
    VERSION.as_str()
}

/// The channel the reported version belongs to.
pub fn channel() -> Channel {
    channel_for(PKG_VERSION, GIT_TAG, GIT_DIRTY)
}

/// The tag that is exactly `v{package version}` and points at this commit,
/// when there is one.
pub fn tag() -> Option<&'static str> {
    GIT_TAG
}

/// The checked-out branch; `None` when HEAD is detached or Git is absent.
pub fn branch() -> Option<&'static str> {
    GIT_BRANCH
}

/// The 12-character short object id of the built commit; `None` without Git
/// metadata.
pub fn commit() -> Option<&'static str> {
    GIT_COMMIT
}

/// Whether tracked files differed from the built commit.
pub fn dirty() -> bool {
    GIT_DIRTY
}

/// The Rust target triple the binary was built for.
pub fn target() -> &'static str {
    TARGET
}

/// How this binary was installed: `source` unless the release build set
/// `BUZZX_INSTALL_KIND`, whose value build.rs already validated.
pub fn install_kind() -> InstallKind {
    match INSTALL_KIND {
        "prebuilt" => InstallKind::Prebuilt,
        _ => InstallKind::Source,
    }
}

/// The identity before the dirty suffix.
fn base_version(
    pkg_version: &str,
    tag: Option<&str>,
    branch: Option<&str>,
    commit: Option<&str>,
) -> String {
    if tag.is_some() {
        pkg_version.to_owned()
    } else if let Some(commit) = commit {
        if branch == Some("main") {
            format!("0.0.0-main.g{commit}")
        } else {
            format!("0.0.0-dev.g{commit}")
        }
    } else {
        "0.0.0-dev.unknown".to_owned()
    }
}

fn display_version(
    pkg_version: &str,
    tag: Option<&str>,
    branch: Option<&str>,
    commit: Option<&str>,
    dirty: bool,
) -> String {
    let mut version = base_version(pkg_version, tag, branch, commit);
    if dirty {
        version.push_str(".dirty");
    }
    version
}

/// A prerelease is a package version with a `-` segment before any `+`
/// build metadata.
fn channel_for(pkg_version: &str, tag: Option<&str>, dirty: bool) -> Channel {
    if dirty || tag.is_none() {
        Channel::Dev
    } else if pkg_version
        .split('+')
        .next()
        .is_some_and(|version| version.contains('-'))
    {
        Channel::Prerelease
    } else {
        Channel::Stable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "abcdef123456";

    #[test]
    fn an_exact_stable_tag_reports_the_package_version() {
        assert_eq!(
            display_version("0.1.0", Some("v0.1.0"), Some("main"), Some(COMMIT), false),
            "0.1.0"
        );
        assert_eq!(channel_for("0.1.0", Some("v0.1.0"), false), Channel::Stable);
        assert_eq!(Channel::Stable.as_str(), "stable");
    }

    #[test]
    fn an_exact_prerelease_tag_reports_the_prerelease_channel() {
        assert_eq!(
            display_version(
                "0.1.0-rc.1",
                Some("v0.1.0-rc.1"),
                Some("main"),
                Some(COMMIT),
                false
            ),
            "0.1.0-rc.1"
        );
        assert_eq!(
            channel_for("0.1.0-rc.1", Some("v0.1.0-rc.1"), false),
            Channel::Prerelease
        );
        assert_eq!(Channel::Prerelease.as_str(), "prerelease");
    }

    #[test]
    fn a_clean_main_commit_reports_the_main_identity() {
        assert_eq!(
            display_version("0.1.0", None, Some("main"), Some(COMMIT), false),
            "0.0.0-main.gabcdef123456"
        );
        assert_eq!(channel_for("0.1.0", None, false), Channel::Dev);
    }

    #[test]
    fn another_branch_reports_the_dev_identity() {
        assert_eq!(
            display_version("0.1.0", None, Some("product/tui-nav"), Some(COMMIT), false),
            "0.0.0-dev.gabcdef123456"
        );
    }

    #[test]
    fn a_detached_head_reports_the_dev_identity() {
        assert_eq!(
            display_version("0.1.0", None, None, Some(COMMIT), false),
            "0.0.0-dev.gabcdef123456"
        );
    }

    #[test]
    fn a_dirty_checkout_appends_the_suffix_and_stays_dev() {
        assert_eq!(
            display_version("0.1.0", None, Some("main"), Some(COMMIT), true),
            "0.0.0-main.gabcdef123456.dirty"
        );
        assert_eq!(channel_for("0.1.0", None, true), Channel::Dev);
    }

    #[test]
    fn a_dirty_tagged_checkout_never_claims_a_release_channel() {
        assert_eq!(
            display_version("0.1.0", Some("v0.1.0"), Some("main"), Some(COMMIT), true),
            "0.1.0.dirty"
        );
        assert_eq!(channel_for("0.1.0", Some("v0.1.0"), true), Channel::Dev);
    }

    #[test]
    fn a_checkout_without_git_reports_the_unknown_identity() {
        assert_eq!(
            display_version("0.1.0", None, None, None, false),
            "0.0.0-dev.unknown"
        );
        assert_eq!(channel_for("0.1.0", None, false), Channel::Dev);
    }

    #[test]
    fn install_kind_words_match_the_reported_vocabulary() {
        assert_eq!(InstallKind::Source.as_str(), "source");
        assert_eq!(InstallKind::Prebuilt.as_str(), "prebuilt");
    }
}
