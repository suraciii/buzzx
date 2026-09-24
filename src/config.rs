//! Identity, relay, and auth-tag resolution. The contract is
//! docs/configuration.md: flags override the environment, which overrides the
//! config file, and nothing is ever prompted for.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nostr::Keys;
use nostr::nips::nip19::ToBech32;
use serde::Deserialize;

/// Exit code for a bad input, before any network call.
pub const EXIT_USAGE: i32 = 1;
/// Exit code for a relay or network failure.
pub const EXIT_NETWORK: i32 = 2;
/// Exit code for an identity or authentication failure.
pub const EXIT_AUTH: i32 = 3;
/// Exit code for any other failure.
pub const EXIT_OTHER: i32 = 4;

/// A startup failure carrying the exit code docs/configuration.md defines.
#[derive(Debug)]
pub struct StartupError {
    pub code: i32,
    pub message: String,
}

impl StartupError {
    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_USAGE,
            message: message.into(),
        }
    }

    pub(crate) fn auth(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_AUTH,
            message: message.into(),
        }
    }

    pub(crate) fn other(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_OTHER,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ConfigFile {
    pub relay_url: Option<String>,
    pub private_key: Option<String>,
    pub auth_tag: Option<String>,
}

/// Where the identity key came from. `remote` does not exist yet: no
/// remote-signing session is implemented, so no source can be it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Flag,
    Env,
    File,
}

/// Where the relay URL came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaySource {
    Flag,
    Env,
    File,
    Default,
}

/// The winning sources of one resolution. `whoami` reports them; nothing
/// else reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sources {
    pub key: KeySource,
    pub relay: RelaySource,
}

impl KeySource {
    /// The label `whoami` prints.
    pub fn label(self) -> &'static str {
        match self {
            KeySource::Flag => "flag",
            KeySource::Env => "env",
            KeySource::File => "file",
        }
    }
}

impl RelaySource {
    /// The label `whoami` prints.
    pub fn label(self) -> &'static str {
        match self {
            RelaySource::Flag => "flag",
            RelaySource::Env => "env",
            RelaySource::File => "file",
            RelaySource::Default => "default",
        }
    }
}

/// Everything a session needs to start: one identity, one relay.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub keys: Keys,
    /// HTTP base URL, no trailing slash. The WebSocket form is derived on use.
    pub http_url: String,
    pub auth_tag: Option<String>,
    pub sources: Sources,
}

/// Turn any of the four accepted relay schemes into the HTTP base and the
/// WebSocket root. A missing scheme is an error, not a guess.
pub fn split_relay_url(raw: &str) -> Result<(String, String), StartupError> {
    let trimmed = raw.trim().trim_end_matches('/');
    let (http, ws) = if let Some(rest) = trimmed.strip_prefix("https://") {
        (format!("https://{rest}"), format!("wss://{rest}"))
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        (format!("http://{rest}"), format!("ws://{rest}"))
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        (format!("https://{rest}"), format!("wss://{rest}"))
    } else if let Some(rest) = trimmed.strip_prefix("ws://") {
        (format!("http://{rest}"), format!("ws://{rest}"))
    } else {
        return Err(StartupError::usage(format!(
            "relay URL {raw:?} has no scheme; use http://, https://, ws://, or wss://"
        )));
    };
    Ok((http, ws))
}

pub(crate) fn config_path() -> PathBuf {
    if let Ok(custom) = std::env::var("BUZZX_CONFIG") {
        return PathBuf::from(custom);
    }
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("buzzx").join("config.toml")
}
pub(crate) fn read_config_file(path: &Path) -> Result<ConfigFile, StartupError> {
    let meta = fs::metadata(path)
        .map_err(|e| StartupError::usage(format!("cannot read config {}: {e}", path.display())))?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(StartupError::auth(format!(
            "config {} is readable by other users; run chmod 600 on it",
            path.display()
        )));
    }
    let text = fs::read_to_string(path)
        .map_err(|e| StartupError::usage(format!("cannot read config {}: {e}", path.display())))?;
    toml::from_str(&text)
        .map_err(|e| StartupError::usage(format!("invalid config {}: {e}", path.display())))
}

/// Resolve identity and relay from explicit sources. Precedence: flag,
/// environment, config file. The process entry points read the environment and
/// delegate here; tests pass values directly.
pub fn resolve_from(
    flag_key: Option<&str>,
    flag_relay: Option<&str>,
    flag_auth_tag: Option<&str>,
    env_key: Option<&str>,
    env_relay: Option<&str>,
    env_auth_tag: Option<&str>,
    file: ConfigFile,
) -> Result<Resolved, StartupError> {
    let (key_text, key_source) = match flag_key {
        Some(key) => (key.to_owned(), KeySource::Flag),
        None => match env_key {
            Some(key) => (key.to_owned(), KeySource::Env),
            None => match file.private_key.clone() {
                Some(key) => (key, KeySource::File),
                None => {
                    return Err(StartupError::auth(
                        "no identity: set BUZZ_PRIVATE_KEY, pass --private-key, or set private_key in the config file",
                    ));
                }
            },
        },
    };
    let keys = Keys::parse(&key_text).map_err(|_| {
        StartupError::auth("invalid private key: expected 64 hex characters or nsec1...")
    })?;

    let (relay_text, relay_source) = match flag_relay {
        Some(relay) => (relay.to_owned(), RelaySource::Flag),
        None => match env_relay {
            Some(relay) => (relay.to_owned(), RelaySource::Env),
            None => match file.relay_url.clone() {
                Some(relay) => (relay, RelaySource::File),
                None => ("http://localhost:3000".to_owned(), RelaySource::Default),
            },
        },
    };
    let (http_url, _ws_url) = split_relay_url(&relay_text)?;

    let auth_tag = flag_auth_tag
        .map(str::to_owned)
        .or_else(|| env_auth_tag.map(str::to_owned))
        .or_else(|| file.auth_tag.clone());
    if let Some(tag) = &auth_tag {
        // Fail fast here rather than on the first write: a stale tag is the
        // most common cause of a relay 403.
        buzz_sdk::nip_oa::verify_auth_tag(tag, &keys.public_key()).map_err(|e| {
            StartupError::auth(format!("auth tag does not verify for this identity: {e}"))
        })?;
    }

    Ok(Resolved {
        keys,
        http_url,
        auth_tag,
        sources: Sources {
            key: key_source,
            relay: relay_source,
        },
    })
}

/// Resolve identity and relay from flags, the environment, and the config
/// file, in that order.
pub fn resolve(
    flag_key: Option<&str>,
    flag_relay: Option<&str>,
    flag_auth_tag: Option<&str>,
) -> Result<Resolved, StartupError> {
    let path = config_path();
    let file = if path.is_file() {
        read_config_file(&path)?
    } else {
        ConfigFile::default()
    };
    resolve_from(
        flag_key,
        flag_relay,
        flag_auth_tag,
        std::env::var("BUZZ_PRIVATE_KEY").ok().as_deref(),
        std::env::var("BUZZ_RELAY_URL").ok().as_deref(),
        std::env::var("BUZZ_AUTH_TAG").ok().as_deref(),
        file,
    )
}

/// `buzzx init`: write the config file at the conventional location.
pub fn init(
    relay: &str,
    private_key: &str,
    auth_tag: Option<&str>,
) -> Result<PathBuf, StartupError> {
    init_at(&config_path(), relay, private_key, auth_tag)
}

fn render_body(http_url: &str, private_key: &str, auth_tag: Option<&str>) -> String {
    let mut body = format!("relay_url = {http_url:?}\nprivate_key = {private_key:?}\n");
    if let Some(tag) = auth_tag {
        body.push_str(&format!("auth_tag = {tag:?}\n"));
    }
    body
}

fn ensure_parent(path: &Path) -> Result<(), StartupError> {
    if let Some(parent) = path.parent() {
        // Only a directory buzzx just created is chmodded; an existing
        // ~/.config belongs to the user, not to this tool.
        let created = !parent.exists();
        fs::create_dir_all(parent)
            .map_err(|e| StartupError::other(format!("cannot create {}: {e}", parent.display())))?;
        if created {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|e| {
                StartupError::other(format!("cannot chmod {}: {e}", parent.display()))
            })?;
        }
    }
    Ok(())
}

/// The npub with the middle elided: enough to recognize an identity, not
/// enough to copy one. Error and status messages show only this form.
pub fn npub_short(keys: &Keys) -> String {
    let npub = keys.public_key().to_bech32().unwrap_or_default();
    let chars: Vec<char> = npub.chars().collect();
    if chars.len() < 13 {
        return npub;
    }
    let head: String = chars[..8].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}...{tail}")
}

/// Write a config file with tight permissions. Refuses to clobber.
pub fn init_at(
    path: &Path,
    relay: &str,
    private_key: &str,
    auth_tag: Option<&str>,
) -> Result<PathBuf, StartupError> {
    if path.exists() {
        return Err(StartupError::usage(format!(
            "config {} already exists; edit it instead",
            path.display()
        )));
    }
    let (http_url, _) = split_relay_url(relay)?;
    Keys::parse(private_key)
        .map_err(|e| StartupError::auth(format!("invalid private key: {e}")))?;

    ensure_parent(path)?;
    fs::write(path, render_body(&http_url, private_key, auth_tag))
        .map_err(|e| StartupError::other(format!("cannot write {}: {e}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|e| StartupError::other(format!("cannot chmod {}: {e}", path.display())))?;
    Ok(path.to_path_buf())
}

/// Write the config file for `buzzx login`. Unlike `init_at` this replaces
/// an existing file, so the write is atomic: the new file is created 0600
/// next to the target and renamed over it. A failure at any step leaves the
/// previous file untouched.
pub fn replace_at(
    path: &Path,
    relay: &str,
    private_key: &str,
    auth_tag: Option<&str>,
) -> Result<PathBuf, StartupError> {
    let (http_url, _) = split_relay_url(relay)?;
    Keys::parse(private_key)
        .map_err(|e| StartupError::auth(format!("invalid private key: {e}")))?;
    write_atomic(path, &render_body(&http_url, private_key, auth_tag))
}

/// Write the config file for `buzzx logout`: the private key and auth tag
/// are gone, the relay preference survives exactly as the user wrote it.
/// The same atomic-write contract as `replace_at` holds.
pub fn clear_login_at(path: &Path, relay_url: Option<&str>) -> Result<PathBuf, StartupError> {
    let body = match relay_url {
        Some(relay) => format!("relay_url = {relay:?}\n"),
        None => String::new(),
    };
    write_atomic(path, &body)
}

/// Create the replacement 0600 beside the target and rename it over. A
/// failure at any step leaves the previous file untouched.
fn write_atomic(path: &Path, body: &str) -> Result<PathBuf, StartupError> {
    ensure_parent(path)?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let write = || -> Result<(), StartupError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| StartupError::other(format!("cannot write {}: {e}", tmp.display())))?;
        file.write_all(body.as_bytes())
            .map_err(|e| StartupError::other(format!("cannot write {}: {e}", tmp.display())))?;
        file.sync_all()
            .map_err(|e| StartupError::other(format!("cannot write {}: {e}", tmp.display())))?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(StartupError::other(format!(
            "cannot replace {}: {e}",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixed_key() -> String {
        let secret = nostr::SecretKey::from_slice(&[7u8; 32]).expect("a fixed test key");
        Keys::new(secret).secret_key().to_secret_hex()
    }

    #[test]
    fn relay_url_accepts_all_four_schemes_and_pairs_transport_forms() {
        assert_eq!(
            split_relay_url("http://localhost:3000").unwrap(),
            (
                "http://localhost:3000".to_owned(),
                "ws://localhost:3000".to_owned()
            )
        );
        assert_eq!(
            split_relay_url("https://relay.example/").unwrap(),
            (
                "https://relay.example".to_owned(),
                "wss://relay.example".to_owned()
            )
        );
        assert_eq!(
            split_relay_url("wss://relay.example").unwrap().0,
            "https://relay.example"
        );
        assert_eq!(split_relay_url("ws://h:1").unwrap().0, "http://h:1");
    }

    #[test]
    fn relay_url_without_a_scheme_is_rejected() {
        assert_eq!(
            split_relay_url("relay.example").unwrap_err().code,
            EXIT_USAGE
        );
    }

    #[test]
    fn identity_precedence_is_flag_then_env_then_file() {
        let flag = Keys::generate();
        let env = Keys::generate();
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            relay_url: Some("http://file".into()),
            auth_tag: None,
        };

        let r = resolve_from(
            Some(&flag.secret_key().to_secret_hex()),
            None,
            None,
            Some(&env.secret_key().to_secret_hex()),
            Some("http://env"),
            None,
            file.clone(),
        )
        .unwrap();
        assert_eq!(r.keys.public_key(), flag.public_key());
        assert_eq!(r.http_url, "http://env");

        let r = resolve_from(None, None, None, None, None, None, file).unwrap();
        assert_eq!(r.http_url, "http://file");
    }

    #[test]
    fn no_identity_source_names_the_sources_and_exits_three() {
        let err =
            resolve_from(None, None, None, None, None, None, ConfigFile::default()).unwrap_err();
        assert_eq!(err.code, EXIT_AUTH);
        assert!(
            err.message.contains("BUZZ_PRIVATE_KEY"),
            "names a source: {err}"
        );
    }

    #[test]
    fn relay_defaults_to_localhost_when_nothing_is_set() {
        let r = resolve_from(
            Some(&fixed_key()),
            None,
            None,
            None,
            None,
            None,
            ConfigFile::default(),
        )
        .unwrap();
        assert_eq!(r.http_url, "http://localhost:3000");
    }

    #[test]
    fn init_writes_tight_permissions_and_refuses_to_clobber() {
        let dir = std::env::temp_dir().join(format!("buzzx-init-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let key = fixed_key();
        let written = init_at(&path, "https://relay.example/", &key, None).expect("init writes");
        assert_eq!(written, path);
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "config is not group or world accessible");
        assert!(
            init_at(&path, "http://other", &key, None).is_err(),
            "second init must not clobber"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sources_report_where_identity_and_relay_won_from() {
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            relay_url: Some("http://file".into()),
            auth_tag: None,
        };
        let r = resolve_from(
            Some(&fixed_key()),
            None,
            None,
            None,
            Some("http://env"),
            None,
            file,
        )
        .unwrap();
        assert_eq!(r.sources.key, KeySource::Flag);
        assert_eq!(r.sources.relay, RelaySource::Env);

        let r = resolve_from(
            None,
            None,
            None,
            Some(&fixed_key()),
            None,
            None,
            ConfigFile::default(),
        )
        .unwrap();
        assert_eq!(r.sources.relay, RelaySource::Default);
        assert_eq!(RelaySource::Default.label(), "default");
        assert_eq!(KeySource::File.label(), "file");
    }

    #[test]
    fn clear_login_removes_credentials_and_keeps_the_relay() {
        let dir = std::env::temp_dir().join(format!("buzzx-logout-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        init_at(&path, "https://relay.example", &fixed_key(), None).expect("init writes");

        clear_login_at(&path, Some("https://relay.example")).expect("clear writes");

        let after = read_config_file(&path).unwrap();
        assert_eq!(after.private_key, None);
        assert_eq!(after.auth_tag, None);
        assert_eq!(after.relay_url.as_deref(), Some("https://relay.example"));
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "config stays user-only");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_login_without_a_relay_writes_an_empty_file() {
        let dir = std::env::temp_dir().join(format!("buzzx-logout-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        init_at(&path, "https://relay.example", &fixed_key(), None).expect("init writes");

        clear_login_at(&path, None).expect("clear writes");

        let after = read_config_file(&path).unwrap();
        assert_eq!(after.relay_url, None);
        let _ = fs::remove_dir_all(&dir);
    }
}
