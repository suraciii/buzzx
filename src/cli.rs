//! The one-shot commands: one call, one JSON value, one exit code. The
//! product contract is docs/browse-collab-cli.md; the exit codes are
//! docs/configuration.md. Nothing here holds state between calls.

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use buzz_sdk::extract_channel_id;
use clap::Subcommand;
use nostr::Event;
use serde_json::{Value, json};

use crate::client::{ChannelDraft, Client, CreateOutcome, WriteOutcome, channel_id, event_id};
use crate::config::{self, CommunityProfile, ConfigFile, Resolved};
use crate::content;
use crate::failure::{Category, Failure};
use crate::mentions;
use crate::{update, version};

/// The default `messages get --limit`.
const DEFAULT_LIMIT: u64 = 20;

#[derive(Subcommand)]
pub enum ChannelsCommand {
    /// List the channels the identity can access.
    List {
        /// Include channels archived by the relay.
        #[arg(long)]
        include_archived: bool,
    },
    /// Read one channel's authoritative metadata.
    Get {
        #[arg(long)]
        channel: String,
    },
    /// Create a channel in the active community.
    Create {
        /// Channel name. Required.
        #[arg(long)]
        name: Option<String>,
        /// Channel type: `stream` or `forum`.
        #[arg(long = "type")]
        kind: Option<String>,
        /// Visibility: `open` or `private`.
        #[arg(long)]
        visibility: Option<String>,
        /// Optional description.
        #[arg(long)]
        description: Option<String>,
    },
    Update {
        #[arg(long)]
        channel: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        visibility: Option<String>,
    },
    Archive {
        #[arg(long)]
        channel: String,
    },
    Unarchive {
        #[arg(long)]
        channel: String,
    },
    Members {
        #[arg(long)]
        channel: String,
    },
    AddMember {
        #[arg(long)]
        channel: String,
        #[arg(long)]
        pubkey: String,
        #[arg(long)]
        role: Option<String>,
    },
    SetRole {
        #[arg(long)]
        channel: String,
        #[arg(long)]
        pubkey: String,
        #[arg(long)]
        role: String,
    },
    RemoveMember {
        #[arg(long)]
        channel: String,
        #[arg(long)]
        pubkey: String,
    },
    Leave {
        #[arg(long)]
        channel: String,
    },
}

#[derive(Subcommand)]
pub enum UpdateCommand {
    /// Check the canonical GitHub Releases metadata without installing.
    Check {
        /// Include prereleases when selecting an update target.
        #[arg(long)]
        prerelease: bool,
    },
}

/// The local community profiles: what a session connects to. `add` and
/// `verify` reach the relay; the rest are offline facts about the config
/// file, and none of them prints a credential.
#[derive(Subcommand)]
pub enum CommunityCommand {
    /// List the saved profiles with their last known status. Offline.
    List,
    /// Verify a relay and save it as a new profile, then make it active.
    /// The relay URL comes from the global --relay.
    Add {
        /// Profile name; the relay host when omitted.
        #[arg(long)]
        name: Option<String>,
    },
    /// Re-verify one saved profile and record the outcome. The active
    /// profile does not change.
    Verify {
        /// Profile id, as `community list` shows it.
        id: String,
    },
    /// Make one saved profile the active community. Offline: nothing here
    /// claims the relay was reached.
    Use {
        /// Profile id.
        id: String,
    },
    /// Rename one saved profile.
    Rename {
        /// Profile id.
        id: String,
        /// The new name.
        name: String,
    },
    /// Remove one saved profile. Local only: nothing on the relay changes.
    Remove {
        /// Profile id.
        id: String,
        /// Permit removing the active profile without first selecting another.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum MessagesCommand {
    /// Read recent messages in one channel, or one event.
    Get {
        /// Channel UUID.
        #[arg(long)]
        channel: Option<String>,
        /// Event id, instead of `--channel`.
        #[arg(long)]
        event: Option<String>,
        /// How many of the latest messages to return, oldest first.
        #[arg(long)]
        limit: Option<String>,
    },
    /// Read one thread: the root event and its replies.
    Thread {
        /// Root event id.
        #[arg(long)]
        event: Option<String>,
    },
    /// Send a top-level message.
    Send {
        /// Channel UUID.
        #[arg(long)]
        channel: Option<String>,
        /// Message content, or `-` to read it from stdin.
        #[arg(long)]
        content: Option<String>,
    },
    /// Reply to an event. The channel and the thread context come from it.
    Reply {
        /// Event id to answer.
        #[arg(long)]
        event: Option<String>,
        /// Message content, or `-` to read it from stdin.
        #[arg(long)]
        content: Option<String>,
    },
}

pub async fn run_channels(resolved: &Resolved, action: ChannelsCommand) -> i32 {
    let client = Client::new(resolved);
    match action {
        ChannelsCommand::List { include_archived } => match client.channels().await {
            Ok(roster) => {
                let list: Vec<Value> = roster
                    .items
                    .iter()
                    .filter(|channel| !channel.hidden && (include_archived || !channel.archived))
                    .map(|channel| {
                        json!({
                            "channel_id": channel.id.to_string(),
                            "name": channel.name,
                            "community": community_json(resolved),
                        })
                    })
                    .collect();
                if !roster.complete {
                    print(&json!({
                        "status": "incomplete",
                        "channels": list,
                        "error": "timeout_unknown",
                        "message": "channel metadata is incomplete; refresh before relying on this list",
                    }));
                    2
                } else {
                    print(&json!(list));
                    0
                }
            }
            Err(failure) => fail(&failure, None),
        },
        ChannelsCommand::Get { channel } => {
            let id = match channel_id(&channel) {
                Ok(id) => id,
                Err(failure) => return fail(&failure, Some(("channel_id", &channel))),
            };
            match client.channel(id).await {
                Ok(info) => {
                    print(&channel_json(resolved, &info));
                    0
                }
                Err(failure) => fail(&failure, Some(("channel_id", &channel))),
            }
        }
        ChannelsCommand::Create {
            name,
            kind,
            visibility,
            description,
        } => {
            create_channel(
                resolved,
                &client,
                name.as_deref(),
                kind.as_deref(),
                visibility.as_deref(),
                description.as_deref(),
            )
            .await
        }
        ChannelsCommand::Update {
            channel,
            name,
            description,
            visibility,
        } => {
            let id = match channel_id(&channel) {
                Ok(id) => id,
                Err(failure) => return channel_refuse(resolved, &channel, "update", &failure),
            };
            let visibility = match visibility {
                Some(value) => match value.parse() {
                    Ok(value) => Some(value),
                    Err(reason) => {
                        return channel_refuse(
                            resolved,
                            &channel,
                            "update",
                            &Failure::invalid_input(reason),
                        );
                    }
                },
                None => None,
            };
            let update = crate::client::ChannelUpdate {
                name,
                description,
                visibility,
            };
            channel_write(
                resolved,
                &client,
                id,
                client.update_channel(id, &update).await,
                "update",
            )
        }
        ChannelsCommand::Archive { channel } => {
            archive_write(resolved, &client, &channel, true).await
        }
        ChannelsCommand::Unarchive { channel } => {
            archive_write(resolved, &client, &channel, false).await
        }
        ChannelsCommand::Members { channel } => {
            let id = match channel_id(&channel) {
                Ok(id) => id,
                Err(failure) => return fail(&failure, Some(("channel_id", &channel))),
            };
            match client.members(id).await {
                Ok(members) => {
                    let rows: Vec<Value> = members
                        .items
                        .iter()
                        .map(|member| {
                            json!({
                                "pubkey": member.pubkey,
                                "role": member.role.as_str(),
                                "name": member.name,
                            })
                        })
                        .collect();
                    let complete = members.complete;
                    print(&json!({
                        "community": community_json(resolved),
                        "channel_id": id.to_string(),
                        "members": rows,
                        "complete": complete,
                        "status": if complete { "ok" } else { "incomplete" },
                        "error": if complete { Value::Null } else { json!("timeout_unknown") },
                    }));
                    if complete { 0 } else { 2 }
                }
                Err(failure) => fail(&failure, Some(("channel_id", &channel))),
            }
        }
        ChannelsCommand::AddMember {
            channel,
            pubkey,
            role,
        } => {
            member_write(
                resolved,
                &client,
                &channel,
                &pubkey,
                role.as_deref().unwrap_or("member"),
                "add-member",
            )
            .await
        }
        ChannelsCommand::SetRole {
            channel,
            pubkey,
            role,
        } => member_write(resolved, &client, &channel, &pubkey, &role, "set-role").await,
        ChannelsCommand::RemoveMember { channel, pubkey } => {
            let id = match channel_id(&channel) {
                Ok(id) => id,
                Err(failure) => {
                    return channel_refuse(resolved, &channel, "remove-member", &failure);
                }
            };
            let pubkey = match canonical_pubkey(&pubkey) {
                Ok(key) => key,
                Err(failure) => {
                    return channel_refuse(resolved, &channel, "remove-member", &failure);
                }
            };
            channel_write(
                resolved,
                &client,
                id,
                client.remove_member(id, &pubkey).await,
                "remove-member",
            )
        }
        ChannelsCommand::Leave { channel } => {
            let id = match channel_id(&channel) {
                Ok(id) => id,
                Err(failure) => return channel_refuse(resolved, &channel, "leave", &failure),
            };
            channel_write(
                resolved,
                &client,
                id,
                client.leave_channel(id).await,
                "leave",
            )
        }
    }
}

fn channel_json(resolved: &Resolved, channel: &crate::client::ChannelInfo) -> Value {
    json!({
        "community": community_json(resolved),
        "channel_id": channel.id.to_string(),
        "name": channel.name,
        "description": channel.description,
        "type": channel.channel_type.map(|kind| kind.as_str()).unwrap_or_else(|| match channel.kind {
            crate::client::ChannelKind::Dm => "dm",
            crate::client::ChannelKind::Channel => "channel",
            crate::client::ChannelKind::Unknown => "unknown",
        }),
        "visibility": channel.visibility.map(|value| value.as_str()),
        "archived": channel.archived,
        "role": channel.my_role.map(|role| role.as_str()),
    })
}

fn channel_write(
    resolved: &Resolved,
    _client: &Client,
    channel: uuid::Uuid,
    outcome: WriteOutcome,
    action: &str,
) -> i32 {
    let (status, event_id, error) = match &outcome {
        WriteOutcome::Stored { event_id } => ("confirmed", Some(event_id.clone()), None),
        WriteOutcome::Refused { category, reason } => {
            ("refused", None, Some((category.as_str(), reason.clone())))
        }
        WriteOutcome::Unknown { category, reason } => {
            ("unknown", None, Some((category.as_str(), reason.clone())))
        }
    };
    let mut value = json!({
        "community": community_json(resolved),
        "channel_id": channel.to_string(),
        "action": action,
        "status": status,
        "event_id": event_id,
        "readback_status": if matches!(&outcome, WriteOutcome::Stored { .. }) {
            "pending_refresh"
        } else if matches!(&outcome, WriteOutcome::Unknown { .. }) {
            "unknown"
        } else {
            "not_applicable"
        },
        "refresh_hint": matches!(&outcome, WriteOutcome::Unknown { .. }),
    });
    if let Some((category, reason)) = error {
        value["error"] = json!(category);
        value["message"] = json!(reason);
    }
    print(&value);
    match outcome {
        WriteOutcome::Stored { .. } => 0,
        WriteOutcome::Refused { category, reason } | WriteOutcome::Unknown { category, reason } => {
            eprintln!("buzzx: {reason}");
            exit_code(category)
        }
    }
}

async fn member_write(
    resolved: &Resolved,
    client: &Client,
    raw_channel: &str,
    pubkey: &str,
    raw_role: &str,
    action: &str,
) -> i32 {
    let channel = match channel_id(raw_channel) {
        Ok(channel) => channel,
        Err(failure) => return channel_refuse(resolved, raw_channel, action, &failure),
    };
    let pubkey = match canonical_pubkey(pubkey) {
        Ok(key) => key,
        Err(failure) => return channel_refuse(resolved, raw_channel, action, &failure),
    };
    let role = match raw_role.parse::<buzz_core::channel::MemberRole>() {
        Ok(role) => role,
        Err(reason) => {
            return channel_refuse(
                resolved,
                raw_channel,
                action,
                &Failure::invalid_input(reason),
            );
        }
    };
    let outcome = if action == "set-role" {
        client.set_role(channel, &pubkey, role).await
    } else {
        client.add_member(channel, &pubkey, role).await
    };
    channel_write(resolved, client, channel, outcome, action)
}

async fn archive_write(
    resolved: &Resolved,
    client: &Client,
    raw_channel: &str,
    archive: bool,
) -> i32 {
    let channel = match channel_id(raw_channel) {
        Ok(channel) => channel,
        Err(failure) => {
            return channel_refuse(
                resolved,
                raw_channel,
                if archive { "archive" } else { "unarchive" },
                &failure,
            );
        }
    };
    let outcome = if archive {
        client.archive_channel(channel).await
    } else {
        client.unarchive_channel(channel).await
    };
    channel_write(
        resolved,
        client,
        channel,
        outcome,
        if archive { "archive" } else { "unarchive" },
    )
}

pub async fn run_messages(resolved: &Resolved, action: MessagesCommand) -> i32 {
    let client = Client::new(resolved);
    match action {
        MessagesCommand::Get {
            channel,
            event,
            limit,
        } => match (channel, event) {
            (Some(channel), None) => {
                read_channel(resolved, &client, &channel, limit.as_deref()).await
            }
            (None, Some(event)) => match limit {
                // A single event has no window to apply a limit to.
                Some(_) => fail(
                    &Failure::invalid_input("--limit applies to --channel reads"),
                    None,
                ),
                None => read_event(resolved, &client, &event).await,
            },
            (Some(_), Some(_)) => fail(
                &Failure::invalid_input("--channel and --event are mutually exclusive"),
                None,
            ),
            (None, None) => fail(
                &Failure::invalid_input("--channel or --event is required"),
                None,
            ),
        },
        MessagesCommand::Thread { event } => read_thread(resolved, &client, event.as_deref()).await,
        MessagesCommand::Send { channel, content } => {
            let channel = match channel.as_deref() {
                Some(raw) => match channel_id(raw) {
                    Ok(channel) => channel,
                    Err(failure) => return refuse(&failure, Some(raw), None),
                },
                None => {
                    return refuse(&Failure::invalid_input("--channel is required"), None, None);
                }
            };
            let content = match content_of(content.as_deref()) {
                Ok(content) => content,
                Err(failure) => return refuse(&failure, Some(&channel.to_string()), None),
            };
            // The same preflight the composer runs: a visible name resolves to
            // a signed recipient, or the write does not happen.
            let recipients = match client.plan_mentions(channel, &content, &[]).await {
                Ok(recipients) => recipients,
                Err(block) => {
                    return block_result(resolved, &block, Some(&channel.to_string()), None);
                }
            };
            write_result_with_community(
                resolved,
                client
                    .send_message(channel, &content, None, &recipients)
                    .await,
                Some(&channel.to_string()),
                None,
                &recipients,
            )
        }
        MessagesCommand::Reply { event, content } => {
            // The target as the caller gave it: a failed reply carries it in
            // `reply_to`, never in `event_id`, which names the new event.
            let given = event.as_deref();
            let target = match given {
                Some(raw) => match event_id(raw) {
                    Ok(target) => target,
                    Err(failure) => return refuse(&failure, None, given),
                },
                None => return refuse(&Failure::invalid_input("--event is required"), None, None),
            };
            let target_hex = target.to_hex();
            let content = match content_of(content.as_deref()) {
                Ok(content) => content,
                Err(failure) => return refuse(&failure, None, Some(&target_hex)),
            };
            // One read, then one write: the routing comes from the event the
            // relay holds, not from what the caller remembers about it.
            let resolved_event = match client.event(target).await {
                Ok(event) => event,
                Err(failure) => return refuse(&failure, None, Some(&target_hex)),
            };
            let Some(channel) = extract_channel_id(&resolved_event) else {
                return refuse(
                    &Failure::invalid_input(format!("event {target_hex} is not channel-scoped")),
                    None,
                    Some(&target_hex),
                );
            };
            let channel_hex = channel.to_string();
            let recipients = match client.plan_mentions(channel, &content, &[]).await {
                Ok(recipients) => recipients,
                Err(block) => {
                    return block_result(resolved, &block, Some(&channel_hex), Some(&target_hex));
                }
            };
            write_result_with_community(
                resolved,
                client.reply(&resolved_event, &content, &recipients).await,
                Some(&channel_hex),
                Some(&target_hex),
                &recipients,
            )
        }
    }
}

/// `buzzx update check`: one unauthenticated, read-only release metadata query.
pub async fn run_update(action: UpdateCommand) -> i32 {
    match action {
        UpdateCommand::Check { prerelease } => {
            let identity = update::BuildIdentity {
                version: version::version().to_owned(),
                channel: match version::channel() {
                    version::Channel::Stable => update::Channel::Stable,
                    version::Channel::Prerelease => update::Channel::Prerelease,
                    version::Channel::Dev => update::Channel::Dev,
                },
                install_kind: match version::install_kind() {
                    version::InstallKind::Source => update::InstallKind::Source,
                    version::InstallKind::Prebuilt => update::InstallKind::Prebuilt,
                },
                source_commit: version::commit().map(str::to_owned),
            };
            let report = update::check(&update::http_client(), &identity, prerelease).await;
            let unknown = report.status == update::Status::Unknown;
            print(&serde_json::to_value(report).expect("update report is serializable"));
            if unknown { config::EXIT_NETWORK } else { 0 }
        }
    }
}

/// `buzzx community`: manage the saved profiles. The one JSON-per-run
/// contract of the relay commands holds here too, and no output carries a
/// private key or an auth tag. Returns the process exit code.
pub fn run_community(
    action: CommunityCommand,
    flag_relay: Option<&str>,
    flag_key: Option<&str>,
    flag_auth_tag: Option<&str>,
    flag_community: Option<&str>,
) -> i32 {
    if flag_community.is_some() {
        return fail(
            &Failure::invalid_input(
                "--community cannot be combined with the community subcommand; \
                 its actions name profile ids themselves",
            ),
            None,
        );
    }
    if flag_relay.is_some() && !matches!(action, CommunityCommand::Add { .. }) {
        return fail(
            &Failure::invalid_input("--relay is only valid for community add"),
            None,
        );
    }
    let path = config::config_path();
    let mut file = if path.is_file() {
        match config::read_config_file(&path) {
            Ok(file) => file,
            Err(error) => return fail_config(&error),
        }
    } else {
        ConfigFile::default()
    };

    match action {
        CommunityCommand::List => {
            print(&list_json(&file));
            0
        }
        CommunityCommand::Use { id } => match plan_use(&mut file, &id) {
            Ok(value) => persist(&path, &file, &value),
            Err(failure) => fail(&failure, Some(("id", &id))),
        },
        CommunityCommand::Rename { id, name } => match plan_rename(&mut file, &id, &name) {
            Ok(value) => persist(&path, &file, &value),
            Err(failure) => fail(&failure, Some(("id", &id))),
        },
        CommunityCommand::Remove { id, yes } => {
            if file.active_community.as_deref() == Some(id.as_str()) && !yes {
                let message = if file.communities.len() > 1 {
                    "active community: run community use <other-id> first or pass --yes"
                } else {
                    "active community: pass --yes to confirm removing the last active profile"
                };
                return fail(&Failure::invalid_input(message), Some(("id", &id)));
            }
            match plan_remove(&mut file, &id) {
                Ok(value) => persist(&path, &file, &value),
                Err(failure) => fail(&failure, Some(("id", &id))),
            }
        }
        CommunityCommand::Add { name } => {
            let Some(relay) = flag_relay else {
                return fail(
                    &Failure::invalid_input("--relay is required for community add"),
                    None,
                );
            };
            let http_url = match config::normalize_relay_url(relay) {
                Ok(http_url) => http_url,
                Err(error) => return fail_config(&error),
            };
            let name = name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| config::host_of(&http_url));
            // The URL check runs before any network call: a second profile
            // for a saved relay is refused without touching the relay.
            if let Some(existing) = file.profile_by_url(&http_url) {
                return fail(
                    &Failure::invalid_input(format!(
                        "already_exists: community {:?} ({}) already uses {}",
                        existing.name, existing.id, existing.relay_url
                    )),
                    Some(("relay_url", &http_url)),
                );
            }
            let keys = match config::resolve_keys(flag_key) {
                Ok(keys) => keys,
                Err(error) => return fail_config(&error),
            };
            // Only an explicit tag applies: a new relay borrows nothing.
            let auth_tag = flag_auth_tag
                .map(str::to_owned)
                .or_else(|| std::env::var("BUZZ_AUTH_TAG").ok());
            if let Some(tag) = &auth_tag
                && let Err(e) = buzz_sdk::nip_oa::verify_auth_tag(tag, &keys.public_key())
            {
                return fail_config(&config::StartupError::auth(format!(
                    "auth tag does not verify for this identity: {e}"
                )));
            }
            // The profile is saved only after the relay accepted the
            // identity the way a session will present it.
            if let Err(error) = crate::login::verify_online(&http_url, &keys, auth_tag.as_deref()) {
                return fail_config(&error);
            }
            let value = plan_add(&mut file, &name, &http_url, auth_tag, now_secs());
            persist(&path, &file, &value)
        }
        CommunityCommand::Verify { id } => {
            let Some(profile) = file.profile(&id).cloned() else {
                return fail(
                    &Failure::invalid_input(format!(
                        "no saved community with id {id:?}; run buzzx community list"
                    )),
                    Some(("id", &id)),
                );
            };
            let keys = match config::resolve_keys(flag_key) {
                Ok(keys) => keys,
                Err(error) => return fail_config(&error),
            };
            // What a session would present for this profile: an explicit
            // tag, the environment, then the profile's own.
            let auth_tag = flag_auth_tag
                .map(str::to_owned)
                .or_else(|| std::env::var("BUZZ_AUTH_TAG").ok())
                .or_else(|| profile.auth_tag.clone());
            let outcome =
                crate::login::verify_online(&profile.relay_url, &keys, auth_tag.as_deref());
            let (status, exit) = match &outcome {
                Ok(()) => ("connected", 0),
                Err(error) => (observed_status(error), error.code),
            };
            let value = plan_observed(&mut file, &id, status, now_secs());
            if let Err(error) = config::save_config_at(&path, &file) {
                return fail_config(&error);
            }
            if exit == 0 {
                print(&value);
                0
            } else {
                // The outcome is both a recorded fact and this run's
                // failure: the error object carries the id it belongs to.
                let reason = outcome
                    .err()
                    .map(|error| observed_reason(&error, auth_tag.is_some()))
                    .unwrap_or_default();
                let mut failure = failure_json(
                    &Failure::new(config_category(exit), reason),
                    Some(("id", &id)),
                );
                failure["status"] = json!(status);
                print(&failure);
                eprintln!("buzzx: verify failed for community {id:?}: {status}");
                exit
            }
        }
    }
}

/// Save the planned config, then print the value it produced. A save
/// failure keeps the old file and fails the run: reporting a profile the
/// file does not hold would be a lie.
fn persist(path: &std::path::Path, file: &ConfigFile, value: &Value) -> i32 {
    match config::save_config_at(path, file) {
        Ok(_) => {
            print(value);
            0
        }
        Err(error) => fail_config(&error),
    }
}

/// Make one profile active. Nothing else about it changes: `use` is a
/// local choice, not a connection claim.
fn plan_use(file: &mut ConfigFile, id: &str) -> Result<Value, Failure> {
    let profile = file.profile(id).cloned().ok_or_else(|| unknown(id))?;
    file.active_community = Some(profile.id.clone());
    Ok(profile_json(&profile, file.active_community.as_deref()))
}

/// Rename one profile. The id is untouched: a rename must not break what
/// `--community` addresses.
fn plan_rename(file: &mut ConfigFile, id: &str, name: &str) -> Result<Value, Failure> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Failure::invalid_input("the new name is empty"));
    }
    let Some(profile) = file.profile_mut(id) else {
        return Err(unknown(id));
    };
    profile.name = name.to_owned();
    let snapshot = profile.clone();
    Ok(profile_json(&snapshot, file.active_community.as_deref()))
}

/// Remove one profile. Removing the active one leaves no active community,
/// stated in the result rather than silently re-pointed at another.
fn plan_remove(file: &mut ConfigFile, id: &str) -> Result<Value, Failure> {
    let Some(position) = file.communities.iter().position(|c| c.id == id) else {
        return Err(unknown(id));
    };
    let removed = file.communities.remove(position);
    if file.active_community.as_deref() == Some(id) {
        file.active_community = None;
    }
    Ok(json!({
        "id": removed.id,
        "name": removed.name,
        "relay_url": removed.relay_url,
        "removed": true,
        "active": file.active_community,
    }))
}

/// Save a verified relay as a new profile and make it active. The caller
/// has already verified the relay and checked the URL for duplicates.
fn plan_add(
    file: &mut ConfigFile,
    name: &str,
    http_url: &str,
    auth_tag: Option<String>,
    now: u64,
) -> Value {
    let profile = CommunityProfile {
        id: config::fresh_profile_id(
            &file
                .communities
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
        ),
        name: name.to_owned(),
        relay_url: http_url.to_owned(),
        auth_tag,
        last_used_at: Some(now),
        last_status: Some("connected".to_owned()),
        last_checked_at: Some(now),
        ..CommunityProfile::default()
    };
    file.active_community = Some(profile.id.clone());
    file.communities.push(profile.clone());
    profile_json(&profile, file.active_community.as_deref())
}

/// Record one observation on a profile. The active profile never moves:
/// a failed check on another community must not redirect a session.
fn plan_observed(file: &mut ConfigFile, id: &str, status: &str, now: u64) -> Value {
    let Some(profile) = file.profile_mut(id) else {
        return json!({"id": id, "status": status});
    };
    profile.last_status = Some(status.to_owned());
    profile.last_checked_at = Some(now);
    let snapshot = profile.clone();
    profile_json(&snapshot, file.active_community.as_deref())
}

/// The profiles in `list` order: the active one first, the rest by their
/// last use, never-used last, ids breaking ties.
fn list_json(file: &ConfigFile) -> Value {
    let active = file.active_community.as_deref();
    let mut rows: Vec<Value> = file
        .communities
        .iter()
        .map(|p| profile_json(p, active))
        .collect();
    let rank = |value: &Value| {
        (
            if value["active"].as_bool() == Some(true) {
                0
            } else {
                1
            },
            -(value["last_used_at"].as_u64().unwrap_or(0) as i64),
            value["id"].as_str().unwrap_or_default().to_owned(),
        )
    };
    rows.sort_by_key(rank);
    Value::Array(rows)
}

/// One profile as the contract's object. `auth_tag_configured` is a fact
/// about the file; the tag itself never appears.
fn profile_json(profile: &CommunityProfile, active: Option<&str>) -> Value {
    json!({
        "id": profile.id,
        "name": profile.name,
        "relay_url": profile.relay_url,
        "active": Some(profile.id.as_str()) == active,
        "auth_tag_configured": profile.auth_tag.is_some(),
        "last_used_at": profile.last_used_at,
        "last_status": profile.last_status,
        "last_checked_at": profile.last_checked_at,
    })
}

fn unknown(id: &str) -> Failure {
    Failure::invalid_input(format!(
        "no saved community with id {id:?}; run buzzx community list"
    ))
}

/// The stored outcome of one failed check, in the profile-status
/// vocabulary: stale the moment it is written, never a live state.
fn observed_status(error: &config::StartupError) -> &'static str {
    match error.code {
        config::EXIT_AUTH => "auth_failed",
        _ => "unavailable",
    }
}

/// The finer reason a check failed, as the error object prints it.
fn observed_reason(error: &config::StartupError, had_tag: bool) -> String {
    match error.code {
        config::EXIT_NETWORK => format!("network: {}", error.message),
        config::EXIT_AUTH if had_tag => format!("auth_invalid: {}", error.message),
        config::EXIT_AUTH => format!("forbidden: {}", error.message),
        _ => format!("relay_rejected: {}", error.message),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn read_channel(resolved: &Resolved, client: &Client, raw: &str, limit: Option<&str>) -> i32 {
    let channel = match channel_id(raw) {
        Ok(channel) => channel,
        Err(failure) => return fail(&failure, Some(("channel_id", raw))),
    };
    let limit = match parse_limit(limit) {
        Ok(limit) => limit,
        Err(failure) => return fail(&failure, Some(("channel_id", &channel.to_string()))),
    };
    match client.history(channel, limit).await {
        Ok(events) => {
            print(&Value::Array(
                events
                    .iter()
                    .map(|event| event_json_with_community(event, resolved))
                    .collect(),
            ));
            0
        }
        Err(failure) => fail(&failure, Some(("channel_id", &channel.to_string()))),
    }
}

async fn read_event(resolved: &Resolved, client: &Client, raw: &str) -> i32 {
    let id = match event_id(raw) {
        Ok(id) => id,
        Err(failure) => return fail(&failure, Some(("event_id", raw))),
    };
    match client.event(id).await {
        Ok(event) => {
            print(&event_json_with_community(&event, resolved));
            0
        }
        Err(failure) => fail(&failure, Some(("event_id", raw))),
    }
}

async fn read_thread(resolved: &Resolved, client: &Client, raw: Option<&str>) -> i32 {
    let id = match raw {
        Some(raw) => event_id(raw),
        None => Err(Failure::invalid_input("--event is required")),
    };
    let id = match id {
        Ok(id) => id,
        Err(failure) => return fail(&failure, Some(("event_id", raw.unwrap_or_default()))),
    };
    match client.thread(id, None).await {
        Ok(read) => {
            print(&Value::Array(
                read.events
                    .iter()
                    .map(|event| event_json_with_community(event, resolved))
                    .collect(),
            ));
            0
        }
        Err(failure) => fail(&failure, Some(("event_id", &id.to_hex()))),
    }
}

/// One event, as the contract's object: every field present, `null` when the
/// event does not carry it.
fn event_json(event: &Event) -> Value {
    json!({
        "channel_id": extract_channel_id(event).map(|channel| channel.to_string()),
        "event_id": event.id.to_hex(),
        "author": event.pubkey.to_hex(),
        "created_at": event.created_at.as_secs(),
        "thread_root": content::root_of(event),
        "reply_to": content::parent_of(event),
        "content": event.content,
    })
}

fn event_json_with_community(event: &Event, resolved: &Resolved) -> Value {
    let mut value = event_json(event);
    value["community"] = community_json(resolved);
    value
}

fn community_json(resolved: &Resolved) -> Value {
    match &resolved.community {
        Some(community) => json!({
            "id": community.id,
            "name": community.name,
            "relay_url": resolved.http_url,
        }),
        None => json!({
            "id": Value::Null,
            "name": "default",
            "relay_url": resolved.http_url,
        }),
    }
}

/// One write, as the contract's object. A write that is not confirmed names
/// its category and reason so a caller never parses prose. `channel_id` and
/// `reply_to` are present when the command has them.
fn write_json(outcome: &WriteOutcome, channel: Option<&str>, reply_to: Option<&str>) -> Value {
    let (status, event_id, error) = match outcome {
        WriteOutcome::Stored { event_id } => ("sent_confirmed", Some(event_id.clone()), None),
        WriteOutcome::Refused { category, reason } => ("not_sent", None, Some((*category, reason))),
        WriteOutcome::Unknown { category, reason } => {
            ("sent_unconfirmed", None, Some((*category, reason)))
        }
    };
    let mut value = json!({
        "status": status,
        "event_id": event_id,
    });
    if let Some(channel) = channel {
        value["channel_id"] = json!(channel);
    }
    if let Some(reply_to) = reply_to {
        value["reply_to"] = json!(reply_to);
    }
    if let Some((category, reason)) = error {
        value["error"] = json!(category.as_str());
        value["message"] = json!(reason);
    }
    value
}

fn write_result(outcome: WriteOutcome, channel: Option<&str>, reply_to: Option<&str>) -> i32 {
    print(&write_json(&outcome, channel, reply_to));
    match outcome {
        WriteOutcome::Stored { .. } => 0,
        WriteOutcome::Refused { category, reason } | WriteOutcome::Unknown { category, reason } => {
            eprintln!("buzzx: {reason}");
            exit_code(category)
        }
    }
}

fn write_result_with_community(
    resolved: &Resolved,
    outcome: WriteOutcome,
    channel: Option<&str>,
    reply_to: Option<&str>,
    mentions: &[String],
) -> i32 {
    let mut value = write_json(&outcome, channel, reply_to);
    if matches!(outcome, WriteOutcome::Stored { .. }) {
        value["mention_pubkeys"] = json!(mentions);
    }
    value["community"] = community_json(resolved);
    print(&value);
    match outcome {
        WriteOutcome::Stored { .. } => 0,
        WriteOutcome::Refused { category, reason } | WriteOutcome::Unknown { category, reason } => {
            eprintln!("buzzx: {reason}");
            exit_code(category)
        }
    }
}

/// One mention preflight refusal, as the contract's object: the write did not
/// happen, the block names its kind and the correction, and the exit code says
/// the caller must change the input. It is never a success shape.
fn block_result(
    resolved: &Resolved,
    block: &mentions::Block,
    channel: Option<&str>,
    reply_to: Option<&str>,
) -> i32 {
    let category = match block {
        mentions::Block::LookupFailed { category, .. } => *category,
        _ => Category::InvalidInput,
    };
    let mut value = json!({
        "status": "not_sent",
        "event_id": Value::Null,
        "error": category.as_str(),
        "message": block.summary(),
        "mention_block": {
            "kind": block.kind(),
            "summary": block.summary(),
            "details": block.details(),
        },
    });
    if let Some(channel) = channel {
        value["channel_id"] = json!(channel);
    }
    if let Some(reply_to) = reply_to {
        value["reply_to"] = json!(reply_to);
    }
    value["community"] = community_json(resolved);
    print(&value);
    eprintln!("buzzx: {}", block.summary());
    exit_code(category)
}

/// `channels create`: one validated draft, one write, one JSON result.
///
/// The client chooses the channel id and the relay establishes the channel;
/// this command never invents a channel row. An unconfirmed write is reported
/// as such and is never submitted a second time here.
async fn create_channel(
    resolved: &Resolved,
    client: &Client,
    name: Option<&str>,
    kind: Option<&str>,
    visibility: Option<&str>,
    description: Option<&str>,
) -> i32 {
    let submitted = SubmittedFields {
        name,
        kind,
        visibility,
        description,
    };
    let Some(name) = name else {
        return create_refused(
            resolved,
            &Failure::invalid_input("--name is required"),
            &submitted,
        );
    };
    let kind = match kind {
        None | Some("stream") => buzz_core::channel::ChannelType::Stream,
        Some("forum") => buzz_core::channel::ChannelType::Forum,
        Some(raw) => {
            return create_refused(
                resolved,
                &Failure::invalid_input(format!(
                    "unknown channel type: {raw:?}; expected stream or forum"
                )),
                &submitted,
            );
        }
    };
    let visibility = match visibility {
        None => buzz_core::channel::ChannelVisibility::Open,
        Some(raw) => match raw.parse() {
            Ok(visibility) => visibility,
            Err(reason) => {
                return create_refused(resolved, &Failure::invalid_input(reason), &submitted);
            }
        },
    };
    let draft = match ChannelDraft::new(name, kind, visibility, description) {
        Ok(draft) => draft,
        Err(failure) => return create_refused(resolved, &failure, &submitted),
    };
    let outcome = client.create_channel(&draft).await;
    let (status, error) = match &outcome {
        CreateOutcome::Confirmed { .. } => ("created_confirmed", None),
        CreateOutcome::Unconfirmed {
            category, reason, ..
        } => ("created_unconfirmed", Some((*category, reason.clone()))),
        CreateOutcome::Refused { category, reason } => {
            ("not_created", Some((*category, reason.clone())))
        }
    };
    let mut value = json!({
        "community": community_json(resolved),
        "status": status,
        "channel_id": outcome.channel_id().map(|id| id.to_string()),
        "name": draft.name,
        "type": draft.kind.as_str(),
        "visibility": draft.visibility.as_str(),
        "description": draft.description,
    });
    if let Some((category, reason)) = error {
        value["error"] = json!(category.as_str());
        value["message"] = json!(reason);
    }
    print(&value);
    match outcome {
        CreateOutcome::Confirmed { .. } => 0,
        CreateOutcome::Unconfirmed {
            category, reason, ..
        } => {
            eprintln!("buzzx: {reason}");
            exit_code(category)
        }
        CreateOutcome::Refused { category, reason } => {
            eprintln!("buzzx: {reason}");
            exit_code(category)
        }
    }
}

/// The fields the caller handed `channels create`, as typed. A refusal echoes
/// them beside the reason, so the caller sees exactly what was rejected.
struct SubmittedFields<'a> {
    name: Option<&'a str>,
    kind: Option<&'a str>,
    visibility: Option<&'a str>,
    description: Option<&'a str>,
}

/// A creation the command refused before anything was signed: it answers with
/// the same object a relay refusal does, so a caller reads `status` on every
/// path.
fn create_refused(resolved: &Resolved, failure: &Failure, submitted: &SubmittedFields) -> i32 {
    let value = json!({
        "community": community_json(resolved),
        "status": "not_created",
        "channel_id": Value::Null,
        "name": submitted.name,
        "type": submitted.kind,
        "visibility": submitted.visibility,
        "description": submitted.description,
        "error": failure.category.as_str(),
        "message": failure.detail,
    });
    print(&value);
    eprintln!("buzzx: {}", failure.detail);
    exit_code(failure.category)
}

/// The outcome of a write the command refused before the relay saw it.
fn refusal(failure: &Failure) -> WriteOutcome {
    WriteOutcome::Refused {
        category: failure.category,
        reason: failure.detail.clone(),
    }
}

/// A write the command refused before the relay saw it: its own validation,
/// its stdin, or the reply target failed. It answers with the write shape, so
/// a caller reads `status` on every recognized write.
fn refuse(failure: &Failure, channel: Option<&str>, reply_to: Option<&str>) -> i32 {
    write_result(refusal(failure), channel, reply_to)
}

fn channel_refuse(resolved: &Resolved, channel: &str, action: &str, failure: &Failure) -> i32 {
    print(&json!({
        "community": community_json(resolved),
        "channel_id": channel,
        "action": action,
        "status": "refused",
        "event_id": Value::Null,
        "readback_status": "not_applicable",
        "refresh_hint": false,
        "error": failure.category.as_str(),
        "message": failure.detail,
    }));
    eprintln!("buzzx: {}", failure.detail);
    exit_code(failure.category)
}

fn canonical_pubkey(raw: &str) -> Result<String, Failure> {
    let key = raw.trim();
    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Failure::invalid_input(
            "pubkey must be exactly 64 hexadecimal characters",
        ));
    }
    Ok(key.to_ascii_lowercase())
}

/// The default `--limit`, or the caller's own. Zero and nonsense are bad
/// input, not an empty read.
fn parse_limit(raw: Option<&str>) -> Result<u64, Failure> {
    match raw {
        None => Ok(DEFAULT_LIMIT),
        Some(text) => text
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|limit| *limit >= 1)
            .ok_or_else(|| Failure::invalid_input(format!("limit must be at least 1: {text}"))),
    }
}

/// The message content: the argument itself, or stdin when the argument is
/// `-`. Stdin keeps every byte, including a trailing newline; an empty or
/// non-UTF-8 stream is bad input before anything is signed.
fn content_of(raw: Option<&str>) -> Result<String, Failure> {
    let Some(raw) = raw else {
        return Err(Failure::invalid_input("--content is required"));
    };
    if raw != "-" {
        return if raw.is_empty() {
            Err(Failure::invalid_input("content is empty"))
        } else {
            Ok(raw.to_owned())
        };
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|e| Failure::invalid_input(format!("stdin: {e}")))?;
    if bytes.is_empty() {
        return Err(Failure::invalid_input("content from stdin is empty"));
    }
    String::from_utf8(bytes)
        .map_err(|_| Failure::invalid_input("content from stdin is not valid UTF-8"))
}

/// One JSON value per run: an array for a collection, an object for one event
/// or one write. Diagnostics go to stderr.
fn print(value: &Value) {
    println!("{value}");
}

/// A read failure carries the id the command was given or derived, when it
/// has one.
fn fail(failure: &Failure, id: Option<(&str, &str)>) -> i32 {
    print(&failure_json(failure, id));
    eprintln!("buzzx: {}", failure.detail);
    exit_code(failure.category)
}

fn failure_json(failure: &Failure, id: Option<(&str, &str)>) -> Value {
    let mut value = json!({
        "error": failure.category.as_str(),
        "message": failure.detail,
    });
    if let Some((field, text)) = id {
        value[field] = json!(text);
    }
    value
}

fn exit_code(category: Category) -> i32 {
    match category {
        Category::InvalidInput | Category::NotFound => config::EXIT_USAGE,
        Category::Network | Category::TimeoutUnknown => config::EXIT_NETWORK,
        Category::Forbidden => config::EXIT_AUTH,
        Category::RelayRejected => config::EXIT_OTHER,
    }
}

/// A failure raised before a command ran: the identity or the relay did not
/// resolve. It prints the same one-object failure as a failed read, so a
/// caller parses one shape on every path.
pub fn fail_startup(code: i32, message: &str) -> i32 {
    fail(&Failure::new(startup_category(code), message), None)
}

/// The shape a failed `messages` invocation answers with. The recognized
/// command decides it, and the ids come from the command line, so the shape
/// is read before the command consumes its arguments.
pub struct FailureShape {
    write: bool,
    channel: Option<String>,
    reply_to: Option<String>,
}

/// What the recognized `messages` command answers with when it fails before
/// it runs. A write keeps its ids: `--channel` for a send, the target for a
/// reply.
pub fn failure_shape(action: &MessagesCommand) -> FailureShape {
    match action {
        MessagesCommand::Send { channel, .. } => FailureShape {
            write: true,
            channel: channel.clone(),
            reply_to: None,
        },
        MessagesCommand::Reply { event, .. } => FailureShape {
            write: true,
            channel: None,
            reply_to: event.clone(),
        },
        MessagesCommand::Get { .. } | MessagesCommand::Thread { .. } => FailureShape {
            write: false,
            channel: None,
            reply_to: None,
        },
    }
}

/// A failure raised before a `messages` command ran. A write answers with the
/// write shape, so a caller reads `status` on every recognized write; a read
/// answers with its error object.
pub fn fail_message_startup(shape: &FailureShape, code: i32, message: &str) -> i32 {
    let failure = Failure::new(startup_category(code), message);
    if shape.write {
        refuse(
            &failure,
            shape.channel.as_deref(),
            shape.reply_to.as_deref(),
        )
    } else {
        fail(&failure, None)
    }
}

/// The category a startup failure carries. Startup failures are raised
/// before any relay call: code 1 is bad input, code 3 is the identity, and
/// the catch-all code 4 is the relay's own bucket. The category decides the
/// code, so the two cannot disagree.
/// A config or verification failure, with the exit code it carries kept
/// distinct: the community commands report network (2) and auth (3)
/// separately instead of folding both into the catch-all.
fn fail_config(error: &config::StartupError) -> i32 {
    fail(
        &Failure::new(config_category(error.code), error.message.clone()),
        None,
    )
}

/// The category a community-command failure carries. Unlike the relay
/// commands' startup mapping, the network code stays itself: `community
/// add` and `verify` answer for reaching a relay, where 2 and 4 differ.
fn config_category(code: i32) -> Category {
    match code {
        config::EXIT_USAGE => Category::InvalidInput,
        config::EXIT_AUTH => Category::Forbidden,
        config::EXIT_NETWORK => Category::Network,
        _ => Category::RelayRejected,
    }
}

fn startup_category(code: i32) -> Category {
    match code {
        config::EXIT_USAGE => Category::InvalidInput,
        config::EXIT_AUTH => Category::Forbidden,
        _ => Category::RelayRejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Tag};
    use uuid::Uuid;

    fn keys() -> Keys {
        Keys::generate()
    }

    fn channel() -> Uuid {
        Uuid::from_u128(0x7e5f_aaba_948a_47b5_8ca0_20c6_e479_53d3)
    }

    fn message(keys: &Keys, tags: Vec<Tag>) -> Event {
        EventBuilder::new(Kind::Custom(9), "hello")
            .tags(tags)
            .sign_with_keys(keys)
            .unwrap()
    }

    fn h_tag() -> Tag {
        Tag::parse(["h", &channel().to_string()]).unwrap()
    }

    #[test]
    fn an_event_object_carries_every_field() {
        let keys = keys();
        let top = message(&keys, vec![h_tag()]);
        let object = event_json(&top);
        assert_eq!(object["channel_id"], json!(channel().to_string()));
        assert_eq!(object["event_id"], json!(top.id.to_hex()));
        assert_eq!(object["author"], json!(keys.public_key().to_hex()));
        assert_eq!(object["created_at"], json!(top.created_at.as_secs()));
        assert_eq!(object["content"], json!("hello"));
        // A top-level message starts its own thread.
        assert_eq!(object["thread_root"], Value::Null);
        assert_eq!(object["reply_to"], Value::Null);

        let reply = message(
            &keys,
            vec![
                h_tag(),
                Tag::parse(["e", &top.id.to_hex(), "", "reply"]).unwrap(),
            ],
        );
        let nested = message(
            &keys,
            vec![
                h_tag(),
                Tag::parse(["e", &top.id.to_hex(), "", "root"]).unwrap(),
                Tag::parse(["e", &reply.id.to_hex(), "", "reply"]).unwrap(),
            ],
        );
        let object = event_json(&nested);
        assert_eq!(object["thread_root"], json!(top.id.to_hex()));
        assert_eq!(object["reply_to"], json!(reply.id.to_hex()));
    }

    #[test]
    fn an_event_without_a_channel_reports_null() {
        let object = event_json(&message(&keys(), Vec::new()));
        assert_eq!(object["channel_id"], Value::Null);
    }

    #[test]
    fn a_confirmed_write_carries_the_relays_id() {
        let outcome = WriteOutcome::Stored {
            event_id: "ab".repeat(32),
        };
        let object = write_json(&outcome, Some(&channel().to_string()), Some("cd"));
        assert_eq!(object["status"], json!("sent_confirmed"));
        assert_eq!(object["event_id"], json!("ab".repeat(32)));
        assert_eq!(object["channel_id"], json!(channel().to_string()));
        assert_eq!(object["reply_to"], json!("cd"));
        assert!(object.get("error").is_none());
    }

    #[test]
    fn a_refused_write_names_its_category_and_an_unknown_one_exits_two() {
        let refused = WriteOutcome::Refused {
            category: Category::Forbidden,
            reason: "not a member".to_owned(),
        };
        let object = write_json(&refused, Some(&channel().to_string()), None);
        assert_eq!(object["status"], json!("not_sent"));
        assert_eq!(object["event_id"], Value::Null);
        assert_eq!(object["error"], json!("forbidden"));
        assert_eq!(object["message"], json!("not a member"));
        assert!(object.get("reply_to").is_none());
        assert_eq!(
            write_result(refused, Some(&channel().to_string()), None),
            config::EXIT_AUTH
        );

        let unknown = WriteOutcome::Unknown {
            category: Category::TimeoutUnknown,
            reason: "timeout".to_owned(),
        };
        let object = write_json(&unknown, Some(&channel().to_string()), None);
        assert_eq!(object["status"], json!("sent_unconfirmed"));
        assert_eq!(object["error"], json!("timeout_unknown"));
        assert_eq!(
            write_result(unknown, Some(&channel().to_string()), None),
            config::EXIT_NETWORK
        );

        // An unconfirmed write exits by its category, so a relay that
        // answered and failed is not reported as a lost answer.
        let failed = WriteOutcome::Unknown {
            category: Category::RelayRejected,
            reason: "HTTP 500: boom".to_owned(),
        };
        let object = write_json(&failed, Some(&channel().to_string()), None);
        assert_eq!(object["status"], json!("sent_unconfirmed"));
        assert_eq!(object["error"], json!("relay_rejected"));
        assert_eq!(
            write_result(failed, Some(&channel().to_string()), None),
            config::EXIT_OTHER
        );
    }

    #[test]
    fn a_write_that_never_left_the_client_keeps_the_write_shape() {
        // A refusal with no channel yet: the object still carries the state
        // a caller branches on, and no id it does not have.
        let failure = Failure::invalid_input("--channel is required");
        let object = write_json(&refusal(&failure), None, None);
        assert_eq!(object["status"], json!("not_sent"));
        assert_eq!(object["event_id"], Value::Null);
        assert_eq!(object["error"], json!("invalid_input"));
        assert_eq!(object["message"], json!("--channel is required"));
        assert!(object.get("channel_id").is_none(), "{object}");

        // A failed reply names its target, never the event it did not store.
        let object = write_json(&refusal(&failure), Some("not-a-uuid"), Some("cd"));
        assert_eq!(object["status"], json!("not_sent"));
        assert_eq!(object["reply_to"], json!("cd"));
        assert_eq!(object["channel_id"], json!("not-a-uuid"));
        assert_eq!(object["event_id"], Value::Null);
    }

    #[test]
    fn the_recognized_command_decides_the_failure_shape() {
        let send = failure_shape(&MessagesCommand::Send {
            channel: Some("7e5f".to_owned()),
            content: None,
        });
        assert!(send.write);
        assert_eq!(send.channel.as_deref(), Some("7e5f"));
        assert_eq!(send.reply_to, None);

        let reply = failure_shape(&MessagesCommand::Reply {
            event: Some("cd".repeat(32)),
            content: None,
        });
        assert!(reply.write);
        assert_eq!(reply.channel, None);
        assert_eq!(reply.reply_to.as_deref(), Some("cd".repeat(32).as_str()));

        let read = failure_shape(&MessagesCommand::Get {
            channel: Some("7e5f".to_owned()),
            event: None,
            limit: None,
        });
        assert!(!read.write);
    }

    #[test]
    fn a_startup_failure_carries_the_category_of_its_code() {
        assert_eq!(startup_category(config::EXIT_USAGE), Category::InvalidInput);
        assert_eq!(startup_category(config::EXIT_AUTH), Category::Forbidden);
        assert_eq!(
            startup_category(config::EXIT_OTHER),
            Category::RelayRejected
        );
        // The category decides the code the caller sees, so the JSON never
        // names a category whose exit code is different.
        for code in [config::EXIT_USAGE, config::EXIT_AUTH, config::EXIT_OTHER] {
            assert_eq!(exit_code(startup_category(code)), code);
        }
    }

    #[test]
    fn a_read_failure_carries_the_id_it_was_given() {
        let failure = Failure::not_found("no event ab");
        assert_eq!(
            failure_json(&failure, Some(("event_id", "ab"))),
            json!({"error": "not_found", "message": "no event ab", "event_id": "ab"})
        );
        assert_eq!(
            failure_json(&failure, None),
            json!({"error": "not_found", "message": "no event ab"})
        );
        assert_eq!(fail(&failure, None), config::EXIT_USAGE);
    }

    #[test]
    fn a_limit_defaults_and_rejects_nonsense() {
        assert_eq!(parse_limit(None).unwrap(), DEFAULT_LIMIT);
        assert_eq!(parse_limit(Some("5")).unwrap(), 5);
        assert_eq!(
            parse_limit(Some("0")).unwrap_err().category,
            Category::InvalidInput
        );
        assert_eq!(
            parse_limit(Some("many")).unwrap_err().category,
            Category::InvalidInput
        );
    }

    #[test]
    fn every_category_has_the_documented_exit_code() {
        assert_eq!(exit_code(Category::InvalidInput), 1);
        assert_eq!(exit_code(Category::NotFound), 1);
        assert_eq!(exit_code(Category::Network), 2);
        assert_eq!(exit_code(Category::TimeoutUnknown), 2);
        assert_eq!(exit_code(Category::Forbidden), 3);
        assert_eq!(exit_code(Category::RelayRejected), 4);
    }

    mod community {
        use super::super::*;
        use crate::config::CommunityProfile;

        fn profile(id: &str, name: &str, relay_url: &str) -> CommunityProfile {
            CommunityProfile {
                id: id.to_owned(),
                name: name.to_owned(),
                relay_url: relay_url.to_owned(),
                ..CommunityProfile::default()
            }
        }

        fn file(active: Option<&str>, profiles: Vec<CommunityProfile>) -> ConfigFile {
            ConfigFile {
                private_key: Some("k".into()),
                active_community: active.map(str::to_owned),
                communities: profiles,
                ..ConfigFile::default()
            }
        }

        #[test]
        fn add_refuses_a_second_profile_for_one_normalized_url() {
            let mut state = file(
                Some("a"),
                vec![profile("a", "work", "https://work.example")],
            );
            // Every transport spelling of one relay is the one URL the
            // duplicate check compares.
            assert_eq!(
                config::normalize_relay_url("wss://work.example/").unwrap(),
                "https://work.example"
            );
            assert!(state.profile_by_url("https://work.example").is_some());

            let value = plan_add(&mut state, "again", "http://other.example", None, 1_000);
            assert_eq!(state.communities.len(), 2);
            assert_eq!(value["active"], json!(true));
            assert_eq!(value["relay_url"], "http://other.example");
            assert_eq!(value["last_status"], "connected");
            assert_eq!(
                state.active_community.as_deref(),
                Some(state.communities[1].id.as_str())
            );
        }

        #[test]
        fn no_json_for_a_profile_carries_a_credential() {
            let mut state = file(
                Some("a"),
                vec![CommunityProfile {
                    auth_tag: Some(r#"["auth","owner","kind=9","sig"]"#.into()),
                    last_used_at: Some(5),
                    last_status: Some("connected".into()),
                    last_checked_at: Some(6),
                    ..profile("a", "work", "https://work.example")
                }],
            );
            let secret = state.communities[0].auth_tag.clone().unwrap();
            let listed = list_json(&state).to_string();
            assert!(!listed.contains(&secret), "{listed}");
            assert!(
                !listed.contains("\"k\""),
                "the private key stays out: {listed}"
            );
            assert!(listed.contains("\"auth_tag_configured\":true"), "{listed}");

            let renamed = plan_rename(&mut state, "a", "day job").unwrap();
            assert!(!renamed.to_string().contains(&secret), "{renamed}");
            let used = plan_use(&mut state, "a").unwrap();
            assert!(!used.to_string().contains(&secret), "{used}");
            let observed = plan_observed(&mut state, "a", "unavailable", 9);
            assert!(!observed.to_string().contains(&secret), "{observed}");
        }

        #[test]
        fn list_orders_the_active_profile_first_then_by_last_use() {
            let state = file(
                Some("old-active"),
                vec![
                    CommunityProfile {
                        last_used_at: Some(1),
                        ..profile("never", "never", "http://never.example")
                    },
                    CommunityProfile {
                        last_used_at: Some(9),
                        ..profile("recent", "recent", "http://recent.example")
                    },
                    CommunityProfile {
                        last_used_at: Some(4),
                        ..profile("old-active", "old", "http://old.example")
                    },
                ],
            );
            let listed = list_json(&state);
            let ids: Vec<&str> = listed
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["id"].as_str().unwrap())
                .collect();
            assert_eq!(ids, vec!["old-active", "recent", "never"]);
            assert_eq!(listed[0]["active"], json!(true));
            assert_eq!(listed[1]["active"], json!(false));
        }

        #[test]
        fn use_changes_only_the_active_reference() {
            let mut state = file(
                Some("a"),
                vec![
                    profile("a", "work", "https://work.example"),
                    profile("b", "home", "http://home.example"),
                ],
            );
            let value = plan_use(&mut state, "b").unwrap();
            assert_eq!(state.active_community.as_deref(), Some("b"));
            assert_eq!(state.communities.len(), 2);
            assert_eq!(state.communities[0].name, "work", "nothing else moves");
            assert_eq!(value["id"], "b");
            assert_eq!(value["active"], json!(true));
        }

        #[test]
        fn rename_keeps_the_id_and_rejects_empty_names() {
            let mut state = file(
                Some("a"),
                vec![profile("a", "work", "https://work.example")],
            );
            let value = plan_rename(&mut state, "a", " day job ").unwrap();
            assert_eq!(state.communities[0].name, "day job");
            assert_eq!(state.communities[0].id, "a");
            assert_eq!(value["name"], "day job");

            let err = plan_rename(&mut state, "a", "  ").unwrap_err();
            assert_eq!(err.category, Category::InvalidInput);
            let err = plan_rename(&mut state, "nope", "x").unwrap_err();
            assert_eq!(err.category, Category::InvalidInput);
        }

        #[test]
        fn removing_the_active_profile_leaves_none_active() {
            let mut state = file(
                Some("a"),
                vec![
                    profile("a", "work", "https://work.example"),
                    profile("b", "home", "http://home.example"),
                ],
            );
            let value = plan_remove(&mut state, "a").unwrap();
            assert_eq!(value["removed"], json!(true));
            assert_eq!(value["active"], serde_json::Value::Null);
            assert_eq!(state.active_community, None);
            assert_eq!(state.communities.len(), 1);
            assert_eq!(state.communities[0].id, "b");

            // Removing the last profile leaves an empty, usable config.
            plan_remove(&mut state, "b").unwrap();
            assert!(state.communities.is_empty());
        }

        #[test]
        fn a_recorded_observation_never_moves_the_active_profile() {
            let mut state = file(
                Some("a"),
                vec![
                    profile("a", "work", "https://work.example"),
                    profile("b", "home", "http://home.example"),
                ],
            );
            let value = plan_observed(&mut state, "b", "auth_failed", 42);
            assert_eq!(state.active_community.as_deref(), Some("a"));
            assert_eq!(
                state.communities[1].last_status.as_deref(),
                Some("auth_failed")
            );
            assert_eq!(state.communities[1].last_checked_at, Some(42));
            assert_eq!(state.communities[0].last_checked_at, None);
            assert_eq!(value["id"], "b");
            assert_eq!(value["active"], json!(false));
        }
    }
}
