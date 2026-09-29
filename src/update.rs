//! The explicit update check. The product contract is
//! docs/versioning-and-updates.md: `buzzx update check` asks one read-only
//! question of the canonical GitHub Releases metadata and reports what it
//! found. It never downloads a binary, runs a shell command, replaces the
//! running executable, or restarts the session.
//!
//! The check is split in two. `decide` is pure: given the build identity and
//! a release list, it answers. `check` adds the network: one GET, no
//! credentials, and every failure — offline, rate limited, unexpected status,
//! malformed body — becomes `unknown` with a reason that says retrying may
//! help. Nothing here retries anything, because nothing here writes anything.

use std::cmp::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The repository whose releases are the canonical update source.
const REPO: &str = "suraciii/buzzx";
const RELEASES_URL: &str = "https://api.github.com/repos/suraciii/buzzx/releases";
const RELEASES_PAGE_SIZE: usize = 100;
const GITHUB_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_API_VERSION: &str = "2022-11-28";

/// The answer to "is there a newer release". The strings are the stable JSON
/// values the CLI prints and the TUI branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    UpToDate,
    UpdateAvailable,
    SourceBuild,
    Unknown,
}

/// Which release line this build belongs to. A development build never
/// compares as a stable release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Stable,
    Prerelease,
    Dev,
}

/// How this binary was installed. The update procedure follows the path the
/// user chose; the client never turns a source installation into a prebuilt
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
    Source,
    Prebuilt,
}

/// What the running binary is, as the build metadata resolved it. The check
/// consumes it; it does not re-derive it.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildIdentity {
    pub version: String,
    pub channel: Channel,
    pub install_kind: InstallKind,
    pub source_commit: Option<String>,
}

/// The machine-readable result of a check. Field names are the contract in
/// docs/versioning-and-updates.md; `reason` appears only when the status is
/// `unknown`, so a successful check prints exactly the specified shape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateReport {
    pub current_version: String,
    pub latest_stable_version: Option<String>,
    pub status: Status,
    pub channel: Channel,
    pub install_kind: InstallKind,
    pub source_commit: Option<String>,
    pub release_url: Option<String>,
    pub update_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One entry of the GitHub release list, reduced to the fields the check
/// needs. Unknown fields are ignored; `draft` and `prerelease` default to
/// false so an older metadata shape cannot hide a release.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub html_url: Option<String>,
}

/// A SemVer value: the numeric triple plus the dot-separated prerelease
/// identifiers, compared by precedence. Build metadata (`+...`) is accepted
/// and ignored, because it does not take part in precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pre: Vec<PreIdentifier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreIdentifier {
    Numeric(u64),
    Text(String),
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| compare_prerelease(&self.pre, &other.pre))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// SemVer precedence for prerelease identifiers: a version without a
/// prerelease outranks one with it; identifiers compare pairwise, numeric
/// below alphanumeric; the shorter list is smaller once all shared positions
/// tie.
fn compare_prerelease(left: &[PreIdentifier], right: &[PreIdentifier]) -> Ordering {
    if left.is_empty() || right.is_empty() {
        return right.len().cmp(&left.len());
    }
    for (left, right) in left.iter().zip(right) {
        let order = match (left, right) {
            (PreIdentifier::Numeric(left), PreIdentifier::Numeric(right)) => left.cmp(right),
            (PreIdentifier::Numeric(_), PreIdentifier::Text(_)) => Ordering::Less,
            (PreIdentifier::Text(_), PreIdentifier::Numeric(_)) => Ordering::Greater,
            (PreIdentifier::Text(left), PreIdentifier::Text(right)) => left.cmp(right),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

/// Parse a version as displayed or as tagged: an optional `v`, the numeric
/// triple, an optional prerelease, optional ignored build metadata.
/// Development identities such as `0.0.0-main.gabcdef123456`,
/// `0.0.0-dev.unknown`, and a `.dirty` suffix parse as ordinary prereleases.
///
/// Release tags are also used in displayed shell commands, so accepting a tag
/// here requires the complete SemVer character set rather than merely a
/// numeric-looking prefix.
pub fn parse_version(raw: &str) -> Option<Version> {
    let trimmed = raw.strip_prefix('v').unwrap_or(raw);
    let (without_build, build) = match trimmed.split_once('+') {
        Some((version, build)) => (version, Some(build)),
        None => (trimmed, None),
    };
    if build.is_some_and(|value| !valid_identifiers(value)) {
        return None;
    }
    let (core, pre) = match without_build.split_once('-') {
        Some((core, pre)) => (core, parse_prerelease(pre)?),
        None => (without_build, Vec::new()),
    };
    let mut numbers = core.split('.');
    let major = parse_core_number(numbers.next()?)?;
    let minor = parse_core_number(numbers.next()?)?;
    let patch = parse_core_number(numbers.next()?)?;
    if numbers.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
        pre,
    })
}

fn parse_core_number(raw: &str) -> Option<u64> {
    if raw.is_empty() || (raw.len() > 1 && raw.starts_with('0')) {
        return None;
    }
    raw.parse().ok()
}

fn valid_identifiers(raw: &str) -> bool {
    !raw.is_empty()
        && raw.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn parse_prerelease(raw: &str) -> Option<Vec<PreIdentifier>> {
    if !valid_identifiers(raw) {
        return None;
    }
    raw.split('.')
        .map(|identifier| {
            if identifier.bytes().all(|byte| byte.is_ascii_digit()) {
                if identifier.len() > 1 && identifier.starts_with('0') {
                    return None;
                }
                Some(PreIdentifier::Numeric(identifier.parse().ok()?))
            } else {
                Some(PreIdentifier::Text(identifier.to_string()))
            }
        })
        .collect()
}

/// Parse the release-list body. Any structural problem — not JSON, not a
/// list, a release without a tag — is malformed metadata, reported as
/// `unknown` by the caller.
fn parse_releases(body: &str) -> Result<Vec<Release>, String> {
    serde_json::from_str(body).map_err(|error| error.to_string())
}

/// The highest release the channel admits. Drafts never qualify; prereleases
/// qualify only when the caller asked for them; a release whose tag is not a
/// usable version cannot be an update target and is skipped.
fn latest_release(releases: &[Release], include_prereleases: bool) -> Option<&Release> {
    releases
        .iter()
        .filter(|release| {
            !release.draft
                && (include_prereleases || !release.prerelease)
                && parse_version(&release.tag_name).is_some()
        })
        .max_by_key(|release| parse_version(&release.tag_name))
}

/// Answer the check from the release list, with no I/O. This is the whole
/// decision table: a source build reports what it runs and how to move the
/// checkout; a prebuilt build compares its version with the selected
/// release; anything unparseable is `unknown`.
pub fn decide(
    build: &BuildIdentity,
    releases: &[Release],
    include_prereleases: bool,
) -> UpdateReport {
    let stable = latest_release(releases, false);
    let target = latest_release(releases, include_prereleases);

    let latest_stable_version = stable.map(|release| display_version(&release.tag_name));
    let selected = target.or(stable);
    let release_url = selected
        .and_then(|release| release.html_url.clone())
        .filter(|url| !url.is_empty());

    match build.install_kind {
        InstallKind::Source => UpdateReport {
            current_version: build.version.clone(),
            latest_stable_version,
            status: Status::SourceBuild,
            channel: build.channel,
            install_kind: build.install_kind,
            source_commit: build.source_commit.clone(),
            release_url,
            update_command: target.map(|release| {
                format!(
                    "git checkout {} && cargo install --locked --path . --force",
                    release.tag_name
                )
            }),
            reason: None,
        },
        InstallKind::Prebuilt => {
            if build.channel == Channel::Prerelease && !include_prereleases {
                return unknown(
                    build,
                    "prerelease builds require --prerelease to compare releases".to_string(),
                );
            }
            let current = match parse_version(&build.version) {
                Some(version) => version,
                None => {
                    return unknown(
                        build,
                        format!(
                            "current version {} is not a valid SemVer value",
                            build.version
                        ),
                    );
                }
            };
            let release = match target.or(stable) {
                Some(release) => release,
                None => {
                    return unknown(
                        build,
                        "no stable release is published for this repository yet".to_string(),
                    );
                }
            };
            match parse_version(&release.tag_name) {
                None => unknown(
                    build,
                    format!(
                        "latest release tag {} is not a valid SemVer value",
                        release.tag_name
                    ),
                ),
                Some(latest) if current >= latest => UpdateReport {
                    current_version: build.version.clone(),
                    latest_stable_version,
                    status: Status::UpToDate,
                    channel: build.channel,
                    install_kind: build.install_kind,
                    source_commit: build.source_commit.clone(),
                    release_url,
                    update_command: None,
                    reason: None,
                },
                Some(_) => UpdateReport {
                    current_version: build.version.clone(),
                    latest_stable_version,
                    status: Status::UpdateAvailable,
                    channel: build.channel,
                    install_kind: build.install_kind,
                    source_commit: build.source_commit.clone(),
                    release_url,
                    update_command: Some(installer_command(&release.tag_name)),
                    reason: None,
                },
            }
        }
    }
}

/// The prebuilt update procedure for one tag, matching the installer
/// commands in docs/installation.md. The tag is pinned, not `latest`, so the
/// command installs exactly the release the check named.
fn installer_command(tag: &str) -> String {
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    if cfg!(windows) {
        format!("powershell -ExecutionPolicy Bypass -c \"irm {base}/buzzx-installer.ps1 | iex\"")
    } else {
        format!("curl --proto '=https' --tlsv1.2 -LsSf {base}/buzzx-installer.sh | sh")
    }
}

/// A tag such as `v0.1.0` reports the version it carries, without the `v`.
fn display_version(tag: &str) -> String {
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

fn unknown(build: &BuildIdentity, reason: String) -> UpdateReport {
    UpdateReport {
        current_version: build.version.clone(),
        latest_stable_version: None,
        status: Status::Unknown,
        channel: build.channel,
        install_kind: build.install_kind,
        source_commit: build.source_commit.clone(),
        release_url: None,
        update_command: None,
        reason: Some(reason),
    }
}

/// A client for the check: read-only, carrying no credentials, and bounded so
/// one explicit check cannot hang the command that asked for it.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("buzzx/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10))
        .build()
        .expect("the client configuration is static")
}

/// Perform the check: fetch every page of release metadata, then decide. Every
/// way this can fail — connection, rate limit, unexpected status, body —
/// becomes `unknown` with a reason that says retrying may help. It is never
/// retried here; the user re-runs the command.
pub async fn check(
    http: &reqwest::Client,
    build: &BuildIdentity,
    include_prereleases: bool,
) -> UpdateReport {
    let mut page = 1usize;
    let mut releases = Vec::new();
    loop {
        let response = match http
            .get(format!(
                "{RELEASES_URL}?per_page={RELEASES_PAGE_SIZE}&page={page}"
            ))
            .header(reqwest::header::ACCEPT, GITHUB_ACCEPT)
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => return unknown(build, format!("network error: {error}")),
        };
        if !response.status().is_success() {
            return unknown(build, non_success_reason(response.status().as_u16()));
        }
        let body = match response.text().await {
            Ok(body) => body,
            Err(error) => return unknown(build, format!("network error: {error}")),
        };
        let page_releases = match parse_releases(&body) {
            Ok(releases) => releases,
            Err(detail) => {
                return unknown(build, format!("malformed release metadata: {detail}"));
            }
        };
        let count = page_releases.len();
        releases.extend(page_releases);
        if count < RELEASES_PAGE_SIZE {
            break;
        }
        page = match page.checked_add(1) {
            Some(page) => page,
            None => return unknown(build, "release pagination overflow".to_string()),
        };
    }
    decide(build, &releases, include_prereleases)
}

/// Why a non-success status still deserves a retry later. GitHub answers a
/// rate-limited caller with 403 or 429; both reset on their own.
fn non_success_reason(code: u16) -> String {
    if code == 403 || code == 429 {
        "rate limited by the GitHub API; retry later".to_string()
    } else {
        format!("unexpected HTTP status {code} from the GitHub API")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stable_build(version: &str) -> BuildIdentity {
        BuildIdentity {
            version: version.to_string(),
            channel: Channel::Stable,
            install_kind: InstallKind::Prebuilt,
            source_commit: None,
        }
    }

    fn source_build(version: &str, commit: &str) -> BuildIdentity {
        BuildIdentity {
            version: version.to_string(),
            channel: Channel::Dev,
            install_kind: InstallKind::Source,
            source_commit: Some(commit.to_string()),
        }
    }

    fn release(tag: &str, draft: bool, prerelease: bool) -> Release {
        Release {
            tag_name: tag.to_string(),
            draft,
            prerelease,
            html_url: Some(format!("https://github.com/{REPO}/releases/tag/{tag}")),
        }
    }

    #[test]
    fn parses_displayed_and_tagged_versions() {
        assert_eq!(parse_version("0.1.0"), parse_version("v0.1.0"));
        assert_eq!(parse_version("v0.1.0").unwrap().major, 0);
        assert_eq!(parse_version("v0.1.0").unwrap().minor, 1);
        assert_eq!(parse_version("v0.1.0").unwrap().patch, 0);
        assert!(parse_version("0.0.0-main.gabcdef123456").is_some());
        assert!(parse_version("0.0.0-dev.unknown").is_some());
        assert!(parse_version("0.0.0-main.gabcdef123456.dirty").is_some());
        assert!(parse_version("0.1.0-rc.1").is_some());
        assert!(parse_version("0.1.0+build.5").is_some());
    }

    #[test]
    fn rejects_malformed_versions() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version("0.1"), None);
        assert_eq!(parse_version("0.1.0.4"), None);
        assert_eq!(parse_version("0.1.x"), None);
        assert_eq!(parse_version("0.1.0-"), None);
        assert_eq!(parse_version("0.1.0-rc."), None);
        assert_eq!(parse_version("0.1.0-01"), None);
        assert_eq!(parse_version("0.1.0-rc;touch /tmp/pwn"), None);
        assert_eq!(parse_version("0.1.0+build?"), None);
        assert_eq!(parse_version("01.1.0"), None);
    }

    #[test]
    fn orders_versions_by_semver_precedence() {
        let chain = [
            "0.1.0-alpha",
            "0.1.0-alpha.1",
            "0.1.0-alpha.beta",
            "0.1.0-beta",
            "0.1.0-beta.2",
            "0.1.0-beta.11",
            "0.1.0-rc.1",
            "0.1.0",
        ];
        for pair in chain.windows(2) {
            let lower = parse_version(pair[0]).unwrap();
            let higher = parse_version(pair[1]).unwrap();
            assert_eq!(
                lower.cmp(&higher),
                Ordering::Less,
                "{} < {}",
                pair[0],
                pair[1]
            );
        }
        assert!(parse_version("0.19.0").unwrap() > parse_version("0.2.0").unwrap());
        assert!(parse_version("1.0.0").unwrap() > parse_version("0.9.9").unwrap());
        assert_eq!(
            parse_version("v0.1.0").unwrap(),
            parse_version("0.1.0").unwrap()
        );
    }

    #[test]
    fn parses_release_lists_and_ignores_extra_fields() {
        let body = r#"[
            {"tag_name":"v0.1.1","name":"0.1.1","prerelease":false,
             "html_url":"https://github.com/suraciii/buzzx/releases/tag/v0.1.1",
             "assets":[{"name":"buzzx-installer.sh"}]},
            {"tag_name":"v0.1.0"}
        ]"#;
        let releases = parse_releases(body).expect("valid list");
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].tag_name, "v0.1.1");
        assert!(!releases[0].draft);
        assert_eq!(releases[1].html_url, None);
    }

    #[test]
    fn rejects_malformed_release_metadata() {
        assert!(parse_releases("not json").is_err());
        assert!(parse_releases("{}").is_err());
        assert!(parse_releases("[{\"draft\":false}]").is_err());
        assert!(parse_releases("[]").is_ok());
    }

    #[test]
    fn prebuilt_up_to_date_when_no_newer_release() {
        let report = decide(
            &stable_build("0.1.0"),
            &[release("v0.1.0", false, false)],
            false,
        );
        assert_eq!(report.status, Status::UpToDate);
        assert_eq!(report.latest_stable_version.as_deref(), Some("0.1.0"));
        assert_eq!(report.update_command, None);
        assert_eq!(
            report.release_url.as_deref(),
            Some("https://github.com/suraciii/buzzx/releases/tag/v0.1.0")
        );
    }

    #[test]
    fn prebuilt_newer_current_stays_up_to_date() {
        let report = decide(
            &stable_build("0.2.0"),
            &[release("v0.1.0", false, false)],
            false,
        );
        assert_eq!(report.status, Status::UpToDate);
    }

    #[test]
    fn prebuilt_update_available_names_the_installer() {
        let report = decide(
            &stable_build("0.1.0"),
            &[release("v0.1.1", false, false)],
            false,
        );
        assert_eq!(report.status, Status::UpdateAvailable);
        assert_eq!(report.latest_stable_version.as_deref(), Some("0.1.1"));
        let expected = if cfg!(windows) {
            "powershell -ExecutionPolicy Bypass -c \"irm https://github.com/suraciii/buzzx/releases/download/v0.1.1/buzzx-installer.ps1 | iex\""
        } else {
            "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/suraciii/buzzx/releases/download/v0.1.1/buzzx-installer.sh | sh"
        };
        assert_eq!(report.update_command.as_deref(), Some(expected));
    }

    #[test]
    fn drafts_and_prereleases_do_not_qualify_as_stable() {
        let releases = [
            release("v0.2.0-rc.1", false, true),
            release("v0.2.0-draft", true, false),
            release("v0.1.0", false, false),
        ];
        let report = decide(&stable_build("0.1.0"), &releases, false);
        assert_eq!(report.status, Status::UpToDate);
        assert_eq!(report.latest_stable_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn unparseable_tags_are_not_update_targets() {
        let releases = [
            release("not-a-version", false, false),
            release("v0.1.0", false, false),
        ];
        let report = decide(&stable_build("0.1.0"), &releases, false);
        assert_eq!(report.status, Status::UpToDate);
        assert_eq!(report.latest_stable_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn prerelease_channel_requires_explicit_opt_in() {
        let build = BuildIdentity {
            version: "0.1.0-rc.1".to_string(),
            channel: Channel::Prerelease,
            install_kind: InstallKind::Prebuilt,
            source_commit: None,
        };
        let report = decide(&build, &[release("v0.1.0", false, false)], false);
        assert_eq!(report.status, Status::Unknown);
        assert!(
            report
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("--prerelease"))
        );
    }

    #[test]
    fn highest_semver_wins_even_when_release_order_is_oldest_first() {
        let releases = [
            release("v0.1.0", false, false),
            release("v0.1.2", false, false),
        ];
        let report = decide(&stable_build("0.1.0"), &releases, false);
        assert_eq!(report.status, Status::UpdateAvailable);
        assert_eq!(report.latest_stable_version.as_deref(), Some("0.1.2"));
    }

    #[test]
    fn prereleases_compared_only_when_requested() {
        let releases = [release("v0.1.0-rc.2", false, true)];
        let current = BuildIdentity {
            version: "0.1.0-rc.1".to_string(),
            channel: Channel::Prerelease,
            install_kind: InstallKind::Prebuilt,
            source_commit: None,
        };
        let stable_only = decide(&current, &releases, false);
        assert_eq!(stable_only.status, Status::Unknown);
        assert!(stable_only.reason.is_some());
        let with_prereleases = decide(&current, &releases, true);
        assert_eq!(with_prereleases.status, Status::UpdateAvailable);
        let expected_command = if cfg!(windows) {
            "powershell -ExecutionPolicy Bypass -c \"irm https://github.com/suraciii/buzzx/releases/download/v0.1.0-rc.2/buzzx-installer.ps1 | iex\""
        } else {
            "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/suraciii/buzzx/releases/download/v0.1.0-rc.2/buzzx-installer.sh | sh"
        };
        assert_eq!(
            with_prereleases.update_command.as_deref(),
            Some(expected_command)
        );
        assert_eq!(
            with_prereleases.release_url.as_deref(),
            Some("https://github.com/suraciii/buzzx/releases/tag/v0.1.0-rc.2")
        );
    }

    #[test]
    fn prerelease_target_beats_requesting_prereleases_when_equal() {
        let releases = [release("v0.1.0-rc.2", false, true)];
        let current = BuildIdentity {
            version: "0.1.0-rc.2".to_string(),
            channel: Channel::Prerelease,
            install_kind: InstallKind::Prebuilt,
            source_commit: None,
        };
        assert_eq!(decide(&current, &releases, true).status, Status::UpToDate);
    }

    #[test]
    fn source_build_reports_commit_and_checkout_command() {
        let build = source_build("0.0.0-main.gabcdef123456", "abcdef123456");
        let report = decide(&build, &[release("v0.1.0", false, false)], false);
        assert_eq!(report.status, Status::SourceBuild);
        assert_eq!(report.channel, Channel::Dev);
        assert_eq!(report.install_kind, InstallKind::Source);
        assert_eq!(report.source_commit.as_deref(), Some("abcdef123456"));
        assert_eq!(report.latest_stable_version.as_deref(), Some("0.1.0"));
        assert_eq!(
            report.update_command.as_deref(),
            Some("git checkout v0.1.0 && cargo install --locked --path . --force")
        );
    }

    #[test]
    fn source_build_without_releases_stays_a_source_build() {
        let build = source_build("0.0.0-dev.unknown", "unknown");
        let report = decide(&build, &[], false);
        assert_eq!(report.status, Status::SourceBuild);
        assert_eq!(report.latest_stable_version, None);
        assert_eq!(report.update_command, None);
        assert_eq!(report.release_url, None);
    }

    #[test]
    fn prebuilt_without_releases_is_unknown() {
        let report = decide(&stable_build("0.1.0"), &[], false);
        assert_eq!(report.status, Status::Unknown);
        assert!(
            report
                .reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty())
        );
    }

    #[test]
    fn invalid_current_version_is_unknown() {
        let report = decide(
            &stable_build("banana"),
            &[release("v0.1.0", false, false)],
            false,
        );
        assert_eq!(report.status, Status::Unknown);
        assert!(report.reason.is_some());
    }

    #[test]
    fn non_success_statuses_carry_a_retryable_reason() {
        assert!(non_success_reason(403).contains("rate limited"));
        assert!(non_success_reason(429).contains("rate limited"));
        assert!(non_success_reason(500).contains("500"));
    }

    #[test]
    fn report_serializes_to_the_specified_shape() {
        let build = source_build("0.0.0-main.gabcdef123456", "abcdef123456");
        let report = decide(&build, &[release("v0.1.0", false, false)], false);
        let value = serde_json::to_value(&report).expect("serializable");
        assert_eq!(
            value,
            serde_json::json!({
                "current_version": "0.0.0-main.gabcdef123456",
                "latest_stable_version": "0.1.0",
                "status": "source_build",
                "channel": "dev",
                "install_kind": "source",
                "source_commit": "abcdef123456",
                "release_url": "https://github.com/suraciii/buzzx/releases/tag/v0.1.0",
                "update_command": "git checkout v0.1.0 && cargo install --locked --path . --force"
            })
        );
    }

    #[test]
    fn unknown_reports_carry_their_reason() {
        let report = decide(&stable_build("0.1.0"), &[], false);
        let value = serde_json::to_value(&report).expect("serializable");
        assert_eq!(value["status"], serde_json::json!("unknown"));
        assert!(value["reason"].is_string());
    }
}
