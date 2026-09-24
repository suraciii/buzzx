//! Identity, relay, and auth-tag resolution. The contract is
//! docs/configuration.md: flags override the environment, which overrides the
//! config file, and nothing is ever prompted for.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nostr::Keys;
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
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_USAGE,
            message: message.into(),
        }
    }

    fn auth(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_AUTH,
            message: message.into(),
        }
    }

    fn other(message: impl Into<String>) -> Self {
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

/// Everything a session needs to start: one identity, one relay.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub keys: Keys,
    /// HTTP base URL, no trailing slash. The WebSocket form is derived on use.
    pub http_url: String,
    pub auth_tag: Option<String>,
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

fn config_path() -> PathBuf {
    if let Ok(custom) = std::env::var("BUZZX_CONFIG") {
        return PathBuf::from(custom);
    }
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("buzzx").join("config.toml")
}

/// Refuse a config file that other users can read. It holds a private key.
fn read_config_file(path: &Path) -> Result<ConfigFile, StartupError> {
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
    let key_src = flag_key
        .map(str::to_owned)
        .or_else(|| env_key.map(str::to_owned))
        .or_else(|| file.private_key.clone())
        .ok_or_else(|| {
            StartupError::auth(
                "no identity: set BUZZ_PRIVATE_KEY, pass --private-key, or set private_key in the config file",
            )
        })?;
    let keys = Keys::parse(&key_src)
        .map_err(|e| StartupError::auth(format!("invalid private key: {e}")))?;

    let relay_src = flag_relay
        .map(str::to_owned)
        .or_else(|| env_relay.map(str::to_owned))
        .or_else(|| file.relay_url.clone())
        .unwrap_or_else(|| "http://localhost:3000".to_owned());
    let (http_url, _ws_url) = split_relay_url(&relay_src)?;

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

    let mut body = format!("relay_url = {http_url:?}\nprivate_key = {private_key:?}\n");
    if let Some(tag) = auth_tag {
        body.push_str(&format!("auth_tag = {tag:?}\n"));
    }

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
    fs::write(path, body)
        .map_err(|e| StartupError::other(format!("cannot write {}: {e}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|e| StartupError::other(format!("cannot chmod {}: {e}", path.display())))?;
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
}
