//! Identity, relay, and auth-tag resolution. The contract is
//! docs/configuration.md: flags override the environment, which overrides the
//! config file, and nothing is ever prompted for.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nostr::Keys;
use nostr::nips::nip19::ToBech32;
use serde::Deserialize;
use sha2::{Digest, Sha256};

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

/// One saved community: the relay a profile names, plus the authorization
/// and local state that belong to that relay alone. The private key stays
/// global; only relay-scoped facts live here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct CommunityProfile {
    /// Local, generated once, never recomputed: a rename must not change
    /// what `--community` addresses.
    pub id: String,
    /// The user's name for the relay; the host until they choose one.
    pub name: String,
    /// Normalized HTTP form, the one form duplicate detection uses.
    pub relay_url: String,
    /// This community's NIP-OA tag, if it has one. Never printed.
    #[serde(default)]
    pub auth_tag: Option<String>,
    #[serde(default)]
    pub last_channel: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<u64>,
    /// The last connection outcome, with its observation time. Stale by
    /// construction; it is never a live connection state.
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_checked_at: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ConfigFile {
    /// The pre-profile relay field. Read once, wrapped into a profile, and
    /// never written back; it is not a second runtime source.
    pub relay_url: Option<String>,
    pub private_key: Option<String>,
    /// The pre-profile global tag. Migrates into the profile of its relay;
    /// an orphan with no relay is dropped, because after profiles no relay
    /// can own it.
    pub auth_tag: Option<String>,
    #[serde(default)]
    pub active_community: Option<String>,
    #[serde(default)]
    pub communities: Vec<CommunityProfile>,
}

impl ConfigFile {
    /// The profile with this id.
    pub fn profile(&self, id: &str) -> Option<&CommunityProfile> {
        self.communities.iter().find(|c| c.id == id)
    }

    /// The profile with this id, mutably.
    pub fn profile_mut(&mut self, id: &str) -> Option<&mut CommunityProfile> {
        self.communities.iter_mut().find(|c| c.id == id)
    }

    /// The profile saved for this normalized URL, if any.
    pub fn profile_by_url(&self, http_url: &str) -> Option<&CommunityProfile> {
        self.communities.iter().find(|c| c.relay_url == http_url)
    }

    /// True when the file still carries the pre-profile shape and the next
    /// successful read must wrap it.
    fn needs_migration(&self) -> bool {
        self.relay_url.is_some() || self.auth_tag.is_some()
    }

    /// Wrap the old single-relay fields as one profile, in memory. The same
    /// wrap runs on every read, so a hand-built legacy config and a file
    /// resolve through one path. A file that mixes old fields with profiles
    /// is a hand edit gone wrong: refuse it rather than guess which relay
    /// owns the global tag.
    fn migrated(mut self) -> Result<ConfigFile, StartupError> {
        if !self.needs_migration() {
            return Ok(self);
        }
        if !self.communities.is_empty() {
            return Err(StartupError::usage(
                "config mixes relay_url/auth_tag with [[communities]]; remove the old fields",
            ));
        }
        let auth_tag = self.auth_tag.take();
        if let Some(relay) = self.relay_url.take() {
            let http_url = normalize_relay_url(&relay)?;
            let profile = CommunityProfile {
                id: migrated_profile_id(&http_url),
                name: host_of(&http_url),
                relay_url: http_url,
                auth_tag,
                ..CommunityProfile::default()
            };
            self.active_community = Some(profile.id.clone());
            self.communities.push(profile);
        }
        Ok(self)
    }
}

/// The normalized relay URL: the HTTP base, trailing slash gone. Profile
/// storage and duplicate detection compare this one form, so `ws://h:1` and
/// `http://h:1/` name one community.
pub fn normalize_relay_url(raw: &str) -> Result<String, StartupError> {
    let (http_url, _) = split_relay_url(raw)?;
    Ok(http_url)
}

/// The host, port included, of a normalized URL: the default profile name.
pub(crate) fn host_of(http_url: &str) -> String {
    let rest = http_url
        .strip_prefix("https://")
        .or_else(|| http_url.strip_prefix("http://"))
        .unwrap_or(http_url);
    rest.split('/').next().unwrap_or(rest).to_owned()
}

/// A short deterministic id for the profile created by legacy migration.
/// Migration may be retried after a failed atomic write, so it cannot use a
/// random id and still preserve the local reference.
fn migrated_profile_id(relay_url: &str) -> String {
    let digest = Sha256::digest(relay_url.as_bytes());
    hex::encode(digest)[..8].to_owned()
}

/// A short local id unlike every given one. Generated once per profile;
/// uniqueness among the saved ids is all that matters.
pub(crate) fn fresh_profile_id(taken: &[String]) -> String {
    loop {
        let candidate = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
}

/// A TOML basic string. Names come from users; quotes, backslashes, and
/// control characters must survive a round trip.
fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The file body in profile form. The old top-level fields are never
/// written: migration wraps them into a profile before any render, so the
/// relay a login or init saves always has one.
pub(crate) fn render_config(file: &ConfigFile) -> String {
    let mut body = String::new();
    if let Some(key) = &file.private_key {
        body.push_str(&format!("private_key = {}\n", toml_string(key)));
    }
    if let Some(active) = &file.active_community {
        body.push_str(&format!("active_community = {}\n", toml_string(active)));
    }
    for profile in &file.communities {
        body.push_str("\n[[communities]]\n");
        body.push_str(&format!("id = {}\n", toml_string(&profile.id)));
        body.push_str(&format!("name = {}\n", toml_string(&profile.name)));
        body.push_str(&format!(
            "relay_url = {}\n",
            toml_string(&profile.relay_url)
        ));
        if let Some(tag) = &profile.auth_tag {
            body.push_str(&format!("auth_tag = {}\n", toml_string(tag)));
        }
        if let Some(channel) = &profile.last_channel {
            body.push_str(&format!("last_channel = {}\n", toml_string(channel)));
        }
        if let Some(at) = profile.last_used_at {
            body.push_str(&format!("last_used_at = {at}\n"));
        }
        if let Some(status) = &profile.last_status {
            body.push_str(&format!("last_status = {}\n", toml_string(status)));
        }
        if let Some(at) = profile.last_checked_at {
            body.push_str(&format!("last_checked_at = {at}\n"));
        }
    }
    body
}

/// Persist a config atomically. A failure at any step leaves the previous
/// file untouched.
pub(crate) fn save_config_at(path: &Path, file: &ConfigFile) -> Result<PathBuf, StartupError> {
    write_atomic(path, &render_config(file))
}

/// Record that an interactive surface selected a saved profile. This is local
/// usage metadata, not proof that the relay is currently connected.
pub(crate) fn mark_profile_used(id: &str) -> Result<(), StartupError> {
    let path = config_path();
    let mut file = read_config_file(&path)?;
    let profile = file
        .profile_mut(id)
        .ok_or_else(|| StartupError::usage(format!("saved community {id:?} disappeared")))?;
    profile.last_used_at = Some(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
    );
    save_config_at(&path, &file).map(|_| ())
}

/// Record the last channel opened for a saved profile.
pub(crate) fn mark_profile_channel(id: &str, channel: &str) -> Result<(), StartupError> {
    let path = config_path();
    let mut file = read_config_file(&path)?;
    let profile = file
        .profile_mut(id)
        .ok_or_else(|| StartupError::usage(format!("saved community {id:?} disappeared")))?;
    profile.last_channel = Some(channel.to_owned());
    save_config_at(&path, &file).map(|_| ())
}

/// Read a config without the strict checks, for writers that must preserve
/// what is there. An unreadable or unparsable file reads as absent, the
/// same blindness a plain overwrite always had.
fn read_config_lenient(path: &Path) -> Result<Option<ConfigFile>, StartupError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Ok(None),
    };
    match toml::from_str::<ConfigFile>(&text) {
        Ok(file) => Ok(Some(file.migrated()?)),
        Err(_) => Ok(None),
    }
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
    /// A saved profile, picked by `--community` or by the active one.
    Profile,
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
            RelaySource::Profile => "profile",
            RelaySource::Default => "default",
        }
    }
}

/// Which saved community a resolution targeted: what downstream surfaces
/// need to label state and writes. An ephemeral or default relay has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityIdentity {
    pub id: String,
    pub name: String,
}

/// Everything a session needs to start: one identity, one relay.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub keys: Keys,
    /// HTTP base URL, no trailing slash. The WebSocket form is derived on use.
    pub http_url: String,
    pub auth_tag: Option<String>,
    pub sources: Sources,
    /// The saved community the relay came from, when it came from one.
    pub community: Option<CommunityIdentity>,
}

/// Turn any of the four accepted relay schemes into the HTTP base and the
/// WebSocket root. A missing scheme is an error, not a guess.
pub fn split_relay_url(raw: &str) -> Result<(String, String), StartupError> {
    let trimmed = raw.trim().trim_end_matches('/').trim_end();
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
    let parsed: ConfigFile = toml::from_str(&text)
        .map_err(|e| StartupError::usage(format!("invalid config {}: {e}", path.display())))?;
    let was_legacy = parsed.needs_migration();
    let migrated = parsed.migrated()?;
    if was_legacy && let Err(e) = write_atomic(path, &render_config(&migrated)) {
        // The old file is intact and equivalent, so the session proceeds;
        // only the persistence of the wrap is deferred to the next read.
        eprintln!(
            "warning: cannot persist the community migration in {}: {e}; the file is unchanged",
            path.display()
        );
    }
    Ok(migrated)
}

/// Resolve identity and relay from explicit sources. Precedence: flag,
/// environment, config file, with the saved communities as the file's relay
/// source. The process entry points read the environment and delegate
/// here; tests pass values directly.
#[allow(clippy::too_many_arguments)]
pub fn resolve_from(
    flag_key: Option<&str>,
    flag_relay: Option<&str>,
    flag_auth_tag: Option<&str>,
    flag_community: Option<&str>,
    env_key: Option<&str>,
    env_relay: Option<&str>,
    env_auth_tag: Option<&str>,
    file: ConfigFile,
) -> Result<Resolved, StartupError> {
    // A command that names a community must not be quietly pointed at
    // another host: the selector and any relay override conflict before
    // anything else runs.
    if flag_community.is_some() && (flag_relay.is_some() || env_relay.is_some()) {
        return Err(StartupError::usage(format!(
            "conflicting_relay_selector: --community cannot be combined with {}",
            if flag_relay.is_some() {
                "--relay"
            } else {
                "BUZZ_RELAY_URL"
            }
        )));
    }
    let file = file.migrated()?;

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

    // The relay: an explicit override is ephemeral, a selector names a
    // saved profile, and only a profile lends its saved auth.
    let (http_url, relay_source, profile) = if let Some(relay) = flag_relay {
        (relay.to_owned(), RelaySource::Flag, None)
    } else if let Some(id) = flag_community {
        let profile = file.profile(id).ok_or_else(|| {
            StartupError::usage(format!(
                "no saved community with id {id:?}; run buzzx community list"
            ))
        })?;
        (
            profile.relay_url.clone(),
            RelaySource::Profile,
            Some(profile.clone()),
        )
    } else if let Some(relay) = env_relay {
        (relay.to_owned(), RelaySource::Env, None)
    } else if let Some(id) = &file.active_community {
        let profile = file.profile(id).ok_or_else(|| {
            StartupError::usage(format!(
                "the active community {id:?} is not saved; run buzzx community use <id>"
            ))
        })?;
        (
            profile.relay_url.clone(),
            RelaySource::Profile,
            Some(profile.clone()),
        )
    } else if !file.communities.is_empty() {
        return Err(StartupError::usage(
            "no active community: pass --community or run buzzx community use <id>",
        ));
    } else {
        (
            "http://localhost:3000".to_owned(),
            RelaySource::Default,
            None,
        )
    };
    let (http_url, _ws_url) = split_relay_url(&http_url)?;

    let auth_tag = flag_auth_tag
        .map(str::to_owned)
        .or_else(|| env_auth_tag.map(str::to_owned))
        .or_else(|| profile.as_ref().and_then(|p| p.auth_tag.clone()));
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
        community: profile.map(|p| CommunityIdentity {
            id: p.id,
            name: p.name,
        }),
    })
}

/// Resolve identity and relay from flags, the environment, and the config
/// file, in that order, with the saved communities as the file's source.
pub fn resolve(
    flag_key: Option<&str>,
    flag_relay: Option<&str>,
    flag_auth_tag: Option<&str>,
    flag_community: Option<&str>,
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
        flag_community,
        std::env::var("BUZZ_PRIVATE_KEY").ok().as_deref(),
        std::env::var("BUZZ_RELAY_URL").ok().as_deref(),
        std::env::var("BUZZ_AUTH_TAG").ok().as_deref(),
        file,
    )
}

/// The identity alone, for commands that carry their own relay: the same
/// precedence every subcommand uses, none of the relay selection.
pub fn resolve_keys(flag_key: Option<&str>) -> Result<Keys, StartupError> {
    let path = config_path();
    let file = if path.is_file() {
        read_config_file(&path)?
    } else {
        ConfigFile::default()
    };
    let (key_text, _) = match flag_key {
        Some(key) => (key.to_owned(), KeySource::Flag),
        None => match std::env::var("BUZZ_PRIVATE_KEY") {
            Ok(key) => (key, KeySource::Env),
            Err(_) => match file.private_key {
                Some(key) => (key, KeySource::File),
                None => {
                    return Err(StartupError::auth(
                        "no identity: set BUZZ_PRIVATE_KEY, pass --private-key, or set private_key in the config file",
                    ));
                }
            },
        },
    };
    Keys::parse(&key_text).map_err(|_| {
        StartupError::auth("invalid private key: expected 64 hex characters or nsec1...")
    })
}

/// `buzzx init`: write the config file at the conventional location.
pub fn init(
    relay: &str,
    private_key: &str,
    auth_tag: Option<&str>,
    name: Option<&str>,
) -> Result<PathBuf, StartupError> {
    init_at(&config_path(), relay, private_key, auth_tag, name)
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

/// Write a config file with tight permissions. Refuses to clobber. The
/// relay becomes the first community profile; init is offline, so nothing
/// here claims the relay was reached.
pub fn init_at(
    path: &Path,
    relay: &str,
    private_key: &str,
    auth_tag: Option<&str>,
    name: Option<&str>,
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
    let file = first_profile_config(&http_url, private_key, auth_tag, name);
    write_atomic(path, &render_config(&file))
}

/// The identity plus one profile for the relay, active by definition.
fn first_profile_config(
    http_url: &str,
    private_key: &str,
    auth_tag: Option<&str>,
    name: Option<&str>,
) -> ConfigFile {
    let profile = CommunityProfile {
        id: fresh_profile_id(&[]),
        name: name
            .filter(|n| !n.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| host_of(http_url)),
        relay_url: http_url.to_owned(),
        auth_tag: auth_tag.map(str::to_owned),
        ..CommunityProfile::default()
    };
    ConfigFile {
        private_key: Some(private_key.to_owned()),
        active_community: Some(profile.id.clone()),
        communities: vec![profile],
        ..ConfigFile::default()
    }
}

/// Write the config file for `buzzx login`. Unlike `init_at` this replaces
/// an existing file, so the write is atomic: the new file is created 0600
/// next to the target and renamed over it. A failure at any step leaves the
/// previous file untouched. Saved profiles survive a login: the verified
/// relay activates its own profile (creating one when none matches) and
/// every other profile keeps its name, URL, and tag untouched.
pub fn replace_at(
    path: &Path,
    relay: &str,
    private_key: &str,
    auth_tag: Option<&str>,
) -> Result<PathBuf, StartupError> {
    let (http_url, _) = split_relay_url(relay)?;
    Keys::parse(private_key)
        .map_err(|e| StartupError::auth(format!("invalid private key: {e}")))?;
    let mut file = read_config_lenient(path)?.unwrap_or_default();
    match file.profile_by_url(&http_url) {
        Some(profile) => {
            let id = profile.id.clone();
            file.active_community = Some(id);
        }
        None => {
            let profile = CommunityProfile {
                id: fresh_profile_id(
                    &file
                        .communities
                        .iter()
                        .map(|c| c.id.clone())
                        .collect::<Vec<_>>(),
                ),
                name: host_of(&http_url),
                relay_url: http_url.clone(),
                ..CommunityProfile::default()
            };
            file.active_community = Some(profile.id.clone());
            file.communities.push(profile);
        }
    }
    file.private_key = Some(private_key.to_owned());
    // An explicit tag moves with the login; an absent one leaves whatever
    // the profile already had.
    if let Some(tag) = auth_tag {
        let id = file.active_community.clone().expect("just set");
        file.profile_mut(&id)
            .expect("just pushed or found")
            .auth_tag = Some(tag.to_owned());
    }
    write_atomic(path, &render_config(&file))
}

/// Write the config file for `buzzx logout`: the private key, every profile
/// tag, and the old global tag are gone; the profiles, their names, URLs,
/// and the active reference survive. The same atomic-write contract as
/// `replace_at` holds.
pub fn clear_login_at(path: &Path) -> Result<PathBuf, StartupError> {
    let mut file = read_config_lenient(path)?.unwrap_or_default();
    file.private_key = None;
    file.auth_tag = None;
    for profile in &mut file.communities {
        profile.auth_tag = None;
    }
    write_atomic(path, &render_config(&file))
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

    fn profile(id: &str, name: &str, relay_url: &str) -> CommunityProfile {
        CommunityProfile {
            id: id.to_owned(),
            name: name.to_owned(),
            relay_url: relay_url.to_owned(),
            ..CommunityProfile::default()
        }
    }

    /// A tag that verifies for the fixed identity: owner key differs, so
    /// `compute_auth_tag` accepts the pair.
    fn valid_tag() -> String {
        let agent = Keys::parse(&fixed_key()).expect("key");
        let owner = Keys::generate();
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=9")
            .expect("a verifying tag")
    }

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("buzzx-{label}-{}", uuid::Uuid::new_v4()))
    }

    fn write_private(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create config directory");
        }
        fs::write(path, body).expect("write config");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod 600");
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
    fn normalization_folds_every_transport_form_into_one_url() {
        assert_eq!(
            normalize_relay_url("wss://relay.example/").unwrap(),
            "https://relay.example"
        );
        assert_eq!(normalize_relay_url(" ws://h:1 ///").unwrap(), "http://h:1");
        assert_eq!(host_of("https://relay.example"), "relay.example");
        assert_eq!(host_of("http://localhost:3000"), "localhost:3000");
    }

    #[test]
    fn identity_precedence_is_flag_then_env_then_file() {
        let flag = Keys::generate();
        let env = Keys::generate();
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            relay_url: Some("http://file".into()),
            ..ConfigFile::default()
        };

        let r = resolve_from(
            Some(&flag.secret_key().to_secret_hex()),
            None,
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
        assert!(r.community.is_none(), "an env relay is not a profile");

        let r = resolve_from(None, None, None, None, None, None, None, file).unwrap();
        assert_eq!(r.http_url, "http://file");
        // The legacy relay wrapped into a profile on the way through.
        assert_eq!(r.community.as_ref().expect("wrapped").name, "file");
        assert_eq!(r.sources.relay, RelaySource::Profile);
    }

    #[test]
    fn no_identity_source_names_the_sources_and_exits_three() {
        let err = resolve_from(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            ConfigFile::default(),
        )
        .unwrap_err();
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
            None,
            ConfigFile::default(),
        )
        .unwrap();
        assert_eq!(r.http_url, "http://localhost:3000");
        assert!(r.community.is_none());
        assert_eq!(r.sources.relay, RelaySource::Default);
    }

    #[test]
    fn community_conflicts_with_a_relay_flag_and_with_the_env_relay() {
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            communities: vec![profile("a", "work", "https://work.example")],
            active_community: Some("a".into()),
            ..ConfigFile::default()
        };
        for (flag_relay, env_relay) in [(Some("http://other"), None), (None, Some("http://other"))]
        {
            let err = resolve_from(
                Some(&fixed_key()),
                flag_relay,
                None,
                Some("a"),
                None,
                env_relay,
                None,
                file.clone(),
            )
            .unwrap_err();
            assert_eq!(err.code, EXIT_USAGE, "{err}");
            assert!(
                err.message.contains("conflicting_relay_selector"),
                "names the conflict: {err}"
            );
        }
    }

    #[test]
    fn the_community_selector_wins_over_the_active_profile() {
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            communities: vec![
                profile("a", "work", "https://work.example"),
                profile("b", "personal", "http://home.example"),
            ],
            active_community: Some("a".into()),
            ..ConfigFile::default()
        };
        let r = resolve_from(None, None, None, Some("b"), None, None, None, file).unwrap();
        assert_eq!(r.http_url, "http://home.example");
        assert_eq!(r.community.as_ref().unwrap().id, "b");
        assert_eq!(r.community.as_ref().unwrap().name, "personal");
    }

    #[test]
    fn an_unknown_community_id_is_bad_input() {
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            communities: vec![profile("a", "work", "https://work.example")],
            active_community: Some("a".into()),
            ..ConfigFile::default()
        };
        let err = resolve_from(None, None, None, Some("nope"), None, None, None, file).unwrap_err();
        assert_eq!(err.code, EXIT_USAGE);
        assert!(err.message.contains("community list"), "{err}");
    }

    #[test]
    fn profiles_without_an_active_one_need_a_selector() {
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            communities: vec![profile("a", "work", "https://work.example")],
            ..ConfigFile::default()
        };
        let err = resolve_from(None, None, None, None, None, None, None, file).unwrap_err();
        assert_eq!(err.code, EXIT_USAGE);
        assert!(err.message.contains("community use"), "{err}");
    }

    #[test]
    fn an_env_relay_overrides_the_active_profile_without_its_auth() {
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            communities: vec![CommunityProfile {
                auth_tag: Some(valid_tag()),
                ..profile("a", "work", "https://work.example")
            }],
            active_community: Some("a".into()),
            ..ConfigFile::default()
        };
        let r = resolve_from(
            None,
            None,
            None,
            None,
            None,
            Some("http://other"),
            None,
            file,
        )
        .unwrap();
        assert_eq!(r.http_url, "http://other");
        assert!(r.community.is_none());
        assert!(
            r.auth_tag.is_none(),
            "an ephemeral relay never borrows a profile's tag"
        );
    }

    #[test]
    fn a_selected_profile_lends_its_saved_auth_tag() {
        let tag = valid_tag();
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            communities: vec![CommunityProfile {
                auth_tag: Some(tag.clone()),
                ..profile("a", "work", "https://work.example")
            }],
            active_community: Some("a".into()),
            ..ConfigFile::default()
        };
        let r = resolve_from(None, None, None, None, None, None, None, file).unwrap();
        assert_eq!(r.http_url, "https://work.example");
        assert_eq!(r.auth_tag.as_deref(), Some(tag.as_str()));
    }

    #[test]
    fn sources_report_where_identity_and_relay_won_from() {
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            communities: vec![profile("a", "work", "https://work.example")],
            active_community: Some("a".into()),
            ..ConfigFile::default()
        };
        let r = resolve_from(
            Some(&fixed_key()),
            None,
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
            None,
            Some(&fixed_key()),
            None,
            None,
            ConfigFile::default(),
        )
        .unwrap();
        assert_eq!(r.sources.relay, RelaySource::Default);
        assert_eq!(RelaySource::Default.label(), "default");
        assert_eq!(RelaySource::Profile.label(), "profile");
        assert_eq!(KeySource::File.label(), "file");
    }

    #[test]
    fn init_writes_one_profile_tight_permissions_and_refuses_to_clobber() {
        let dir = temp_dir("init");
        let path = dir.join("config.toml");
        let key = fixed_key();
        let written =
            init_at(&path, "https://relay.example/", &key, None, Some("work")).expect("init");
        assert_eq!(written, path);
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "config is not group or world accessible");
        let file = read_config_file(&path).unwrap();
        assert_eq!(file.communities.len(), 1);
        assert_eq!(file.communities[0].name, "work");
        assert_eq!(file.communities[0].relay_url, "https://relay.example");
        assert_eq!(
            file.active_community.as_deref(),
            Some(file.communities[0].id.as_str())
        );
        assert!(
            init_at(&path, "http://other", &key, None, None).is_err(),
            "second init must not clobber"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_names_the_profile_after_the_host_when_no_name_is_given() {
        let dir = temp_dir("init-host");
        let path = dir.join("config.toml");
        init_at(&path, "http://localhost:3000", &fixed_key(), None, None).expect("init");
        let file = read_config_file(&path).unwrap();
        assert_eq!(file.communities[0].name, "localhost:3000");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_legacy_config_migrates_on_first_read_and_keeps_working() {
        let dir = temp_dir("migrate");
        let path = dir.join("config.toml");
        write_private(
            &path,
            "relay_url = \"wss://relay.example/\"\nprivate_key = \"...\"\nauth_tag = \"tag\"\n",
        );

        let file = read_config_file(&path).unwrap();
        assert_eq!(file.communities.len(), 1);
        let migrated = &file.communities[0];
        assert_eq!(migrated.relay_url, "https://relay.example");
        assert_eq!(migrated.name, "relay.example");
        assert_eq!(migrated.auth_tag.as_deref(), Some("tag"));
        assert_eq!(file.active_community.as_deref(), Some(migrated.id.as_str()));
        assert_eq!(file.relay_url, None, "the old field is gone");
        assert_eq!(
            file.auth_tag, None,
            "the old global tag moved into the profile"
        );

        // The wrap is persisted, so the next read is stable: same id, no
        // second profile.
        let again = read_config_file(&path).unwrap();
        assert_eq!(again.communities.len(), 1);
        assert_eq!(again.communities[0].id, migrated.id);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_migration_that_cannot_persist_keeps_the_old_file() {
        let dir = temp_dir("migrate-fail");
        let path = dir.join("config.toml");
        write_private(
            &path,
            "relay_url = \"https://relay.example\"\nprivate_key = \"k\"\n",
        );
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).expect("lock dir");

        // A root process ignores directory permissions; the contract is
        // only checkable where the lock holds.
        let probe = dir.join("probe");
        let can_lock = fs::write(&probe, b"x").is_err();
        let _ = fs::remove_file(&probe);

        if can_lock {
            let file = read_config_file(&path).unwrap();
            assert_eq!(file.communities.len(), 1, "the wrap still resolves");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "relay_url = \"https://relay.example\"\nprivate_key = \"k\"\n",
                "the old file is untouched"
            );
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("unlock");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_mixing_old_fields_with_profiles_is_refused() {
        let dir = temp_dir("mixed");
        let path = dir.join("config.toml");
        write_private(
            &path,
            "relay_url = \"https://relay.example\"\n\n[[communities]]\nid = \"a\"\nname = \"work\"\nrelay_url = \"https://work.example\"\n",
        );
        let err = read_config_file(&path).unwrap_err();
        assert_eq!(err.code, EXIT_USAGE);
        assert!(err.message.contains("mixes"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_identity_saves_two_profiles_through_login() {
        let dir = temp_dir("two-profiles");
        let path = dir.join("config.toml");
        let other = Keys::generate().secret_key().to_secret_hex();

        replace_at(&path, "https://work.example/", &fixed_key(), None).expect("first");
        replace_at(&path, "http://home.example", &other, None).expect("second");

        let file = read_config_file(&path).unwrap();
        assert_eq!(file.communities.len(), 2);
        let urls: Vec<&str> = file
            .communities
            .iter()
            .map(|c| c.relay_url.as_str())
            .collect();
        assert!(urls.contains(&"https://work.example"));
        assert!(urls.contains(&"http://home.example"));
        assert_eq!(file.private_key.as_deref(), Some(other.as_str()));
        assert_eq!(
            file.active_community.as_deref(),
            Some(
                file.profile_by_url("http://home.example")
                    .unwrap()
                    .id
                    .as_str()
            ),
            "the verified relay becomes active"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn login_to_a_saved_relay_reuses_its_profile_instead_of_duplicating_it() {
        let dir = temp_dir("same-url");
        let path = dir.join("config.toml");
        replace_at(&path, "https://work.example", &fixed_key(), None).expect("first");
        replace_at(&path, "wss://work.example/", &fixed_key(), None).expect("again");

        let file = read_config_file(&path).unwrap();
        assert_eq!(file.communities.len(), 1, "one URL, one profile");
        assert_eq!(file.communities[0].relay_url, "https://work.example");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn logout_clears_every_credential_and_keeps_the_profiles() {
        let dir = temp_dir("logout");
        let path = dir.join("config.toml");
        init_at(
            &path,
            "https://work.example",
            &fixed_key(),
            Some(&valid_tag()),
            Some("work"),
        )
        .expect("init");

        clear_login_at(&path).expect("clear");

        let after = read_config_file(&path).unwrap();
        assert_eq!(after.private_key, None);
        assert_eq!(after.communities.len(), 1, "the profile survives");
        assert_eq!(after.communities[0].name, "work", "the name survives");
        assert_eq!(after.communities[0].relay_url, "https://work.example");
        assert_eq!(after.communities[0].auth_tag, None, "the tag is cleared");
        assert_eq!(
            after.active_community.as_deref(),
            Some(after.communities[0].id.as_str()),
            "the active reference survives"
        );
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "config stays user-only");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn logout_on_a_legacy_file_keeps_the_relay_as_a_profile() {
        let dir = temp_dir("logout-legacy");
        let path = dir.join("config.toml");
        write_private(
            &path,
            "relay_url = \"https://relay.example\"\nprivate_key = \"k\"\nauth_tag = \"tag\"\n",
        );

        clear_login_at(&path).expect("clear");

        let after = read_config_file(&path).unwrap();
        assert_eq!(after.private_key, None);
        assert_eq!(after.communities.len(), 1);
        assert_eq!(after.communities[0].relay_url, "https://relay.example");
        assert_eq!(after.communities[0].auth_tag, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_with_quotes_and_backslashes_survive_the_round_trip() {
        let dir = temp_dir("render");
        let path = dir.join("config.toml");
        let file = ConfigFile {
            private_key: Some(fixed_key()),
            active_community: Some("a".into()),
            communities: vec![profile("a", "wor\"k \\", "https://work.example")],
            ..ConfigFile::default()
        };
        save_config_at(&path, &file).expect("save");
        let back = read_config_file(&path).unwrap();
        assert_eq!(back.communities[0].name, "wor\"k \\");
        let _ = fs::remove_dir_all(&dir);
    }
}
