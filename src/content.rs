//! The event-to-row mapping and the overlay rules. The contract is
//! design/render-contract.md. Everything here is pure: no I/O, no clock.

use nostr::{Event, Tag};

/// Kinds that become timeline rows and are requested in the history query and
/// the live subscription.
pub const TIMELINE_KINDS: [u32; 5] = [9, 40002, 40008, 45001, 45003];
/// Kinds that never render their own row; they overlay a target row.
pub const AUX_KINDS: [u32; 4] = [7, 40003, 5, 9005];
/// Membership roster (kind 39002) and channel metadata (kind 39000).
pub const MEMBERSHIP_KIND: u32 = 39002;
pub const CHANNEL_METADATA_KIND: u32 = 39000;
pub const PROFILE_KIND: u32 = 0;

/// The default reaction emoji, sent when the composer has no emoji body.
pub const DEFAULT_REACTION: &str = "\u{1f44d}";

/// One rendered timeline entry. A row is a per-session view of one event;
/// overlays mutate it in place.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Hex event id, or `pending:<uuid>` for an optimistic local send.
    pub event_id: String,
    /// Author pubkey, hex.
    pub pubkey: String,
    /// Display name, resolved from profiles, falling back to a shortened key.
    pub author: String,
    pub created_at: u64,
    pub body: String,
    pub kind: u32,
    pub root_id: Option<String>,
    /// The immediate parent for a direct reply, when the event carries one.
    pub parent_id: Option<String>,
    pub broadcast: bool,
    pub mentions_me: bool,
    pub reactions: Vec<(String, u32)>,
    /// Filename from the first `imeta` attachment tag, if any.
    pub attachment: Option<String>,
    pub pending: bool,
    /// A write whose result is unknown. Kept, never retried.
    pub uncertain: bool,
    pub edited: bool,
}

/// An auxiliary event resolved against its target row.
#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    /// Add one emoji counter for the target row.
    Reaction { emoji: String },
    /// Replace the target row's body.
    Edit { body: String },
    /// Remove the target row.
    Delete,
}

fn tag_strings(tag: &Tag) -> &[String] {
    tag.as_slice()
}

fn first_tag_value<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    event.tags.iter().find_map(|t| {
        let parts = tag_strings(t);
        (parts.first().map(String::as_str) == Some(name))
            .then(|| parts.get(1).map(String::as_str))
            .flatten()
    })
}

/// The root of a thread: the `root` marker, else the `reply` marker, else the
/// single parent id. Matching the order render-contract.md fixes.
pub fn root_of(event: &Event) -> Option<String> {
    for marker in ["root", "reply"] {
        for tag in event.tags.iter() {
            let parts = tag_strings(tag);
            if parts.first().map(String::as_str) == Some("e")
                && parts.get(3).map(String::as_str) == Some(marker)
            {
                return parts.get(1).cloned();
            }
        }
    }
    parent_of(event)
}

/// The immediate parent: the `reply` marker only, then a bare single e tag.
pub fn parent_of(event: &Event) -> Option<String> {
    let mut bare = None;
    let mut bare_count = 0;
    for tag in event.tags.iter() {
        let parts = tag_strings(tag);
        if parts.first().map(String::as_str) != Some("e") {
            continue;
        }
        if parts.get(3).map(String::as_str) == Some("reply") {
            return parts.get(1).cloned();
        }
        if parts.len() == 2 {
            bare_count += 1;
            bare = parts.get(1).cloned();
        }
    }
    (bare_count == 1).then_some(bare).flatten()
}

/// Parse the filename or URL out of an `imeta` tag's `key value` pairs.
pub fn imeta_filename(parts: &[String]) -> Option<String> {
    let mut filename = None;
    let mut url = None;
    for entry in parts.iter().skip(1) {
        if let Some(rest) = entry.strip_prefix("filename ") {
            filename = Some(rest.to_owned());
        } else if let Some(rest) = entry.strip_prefix("url ") {
            url = Some(rest.to_owned());
        }
    }
    filename.or_else(|| {
        url.map(|u| {
            u.rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(&u)
                .to_owned()
        })
    })
}

/// Build a row from a timeline event. `me` is the identity's own pubkey, used
/// for the mention highlight only.
pub fn row_from_event(event: &Event, me: &str) -> Row {
    let mentions_me = event.tags.iter().any(|t| {
        let parts = tag_strings(t);
        parts.first().map(String::as_str) == Some("p")
            && parts.get(1).map(String::as_str) == Some(me)
    });
    let attachment = event.tags.iter().find_map(|t| {
        let parts = tag_strings(t);
        (parts.first().map(String::as_str) == Some("imeta"))
            .then(|| imeta_filename(parts))
            .flatten()
    });
    Row {
        event_id: event.id.to_hex(),
        pubkey: event.pubkey.to_hex(),
        author: short_pubkey(&event.pubkey.to_hex()),
        created_at: event.created_at.as_secs(),
        body: event.content.clone(),
        kind: u32::from(event.kind.as_u16()),
        root_id: root_of(event),
        parent_id: parent_of(event),
        broadcast: first_tag_value(event, "broadcast") == Some("1"),
        mentions_me,
        reactions: Vec::new(),
        attachment,
        pending: false,
        uncertain: false,
        edited: false,
    }
}

/// The identity's own key renders as `you` rather than a shortened key.
pub fn short_pubkey(hex: &str) -> String {
    format!(
        "{}..{}",
        &hex[..4.min(hex.len())],
        &hex[hex.len().saturating_sub(4)..]
    )
}

/// Classify an auxiliary event against its target. Returns the target event id
/// and the overlay it applies. Kind 5 resolves through `reactions`: a deletion
/// aimed at a reaction event id decrements the counter instead of removing a
/// row.
pub fn overlay_of(
    event: &Event,
    reactions: &dyn Fn(&str) -> Option<(String, String)>,
) -> Option<(String, Overlay)> {
    let target = || {
        event.tags.iter().find_map(|t| {
            let parts = tag_strings(t);
            (parts.first().map(String::as_str) == Some("e"))
                .then(|| parts.get(1).cloned())
                .flatten()
        })
    };
    match u32::from(event.kind.as_u16()) {
        7 => {
            let emoji = if event.content.trim().is_empty() {
                DEFAULT_REACTION
            } else {
                &event.content
            };
            Some((
                target()?,
                Overlay::Reaction {
                    emoji: emoji.to_owned(),
                },
            ))
        }
        40003 => Some((
            target()?,
            Overlay::Edit {
                body: event.content.clone(),
            },
        )),
        5 | 9005 => {
            let id = target()?;
            if event.kind.as_u16() == 5 && id.len() == 64 {
                // A kind 5 aimed at a reaction event removes that reaction.
                // The overlay targets the row the reaction belongs to.
                if let Some((row, emoji)) = reactions(&id) {
                    return Some((
                        row,
                        Overlay::Reaction {
                            emoji: format!("-{emoji}"),
                        },
                    ));
                }
            }
            Some((id, Overlay::Delete))
        }
        _ => None,
    }
}

/// Apply one overlay to a row. A reaction emoji prefixed with `-` decrements;
/// that form is produced only by `overlay_of` for a reaction removal.
pub fn apply_overlay(row: &mut Row, overlay: &Overlay) {
    match overlay {
        Overlay::Reaction { emoji } => {
            if let Some(minus) = emoji.strip_prefix('-') {
                if let Some(entry) = row.reactions.iter_mut().find(|(e, _)| e == minus) {
                    entry.1 = entry.1.saturating_sub(1);
                    if entry.1 == 0 {
                        row.reactions.retain(|(e, _)| e != minus);
                    }
                }
            } else if let Some(entry) = row.reactions.iter_mut().find(|(e, _)| e == emoji) {
                entry.1 += 1;
            } else {
                row.reactions.push((emoji.clone(), 1));
            }
        }
        Overlay::Edit { body } => {
            row.body = body.clone();
            row.edited = true;
        }
        Overlay::Delete => {}
    }
}

/// Build a synthetic row for an optimistic local send. It mirrors the tag
/// shape of the event that will be signed, so overlays behave the same way.
pub fn pending_row(
    me_pubkey: &str,
    local_id: &str,
    body: &str,
    thread: &Option<(String, String)>,
    me_mention: bool,
    created_at: u64,
) -> Row {
    let (root_id, parent_id) = match thread {
        Some((root, parent)) if root == parent => (None, Some(parent.clone())),
        Some((root, parent)) => (Some(root.clone()), Some(parent.clone())),
        None => (None, None),
    };
    Row {
        event_id: local_id.to_owned(),
        pubkey: me_pubkey.to_owned(),
        author: "you".to_owned(),
        created_at,
        body: body.to_owned(),
        kind: 9,
        root_id,
        parent_id,
        broadcast: false,
        mentions_me: me_mention,
        reactions: Vec::new(),
        attachment: None,
        pending: true,
        uncertain: false,
        edited: false,
    }
}

/// Extract `(name, display_name)` from a kind 0 profile event's content.
pub fn profile_names(event: &Event) -> Option<(Option<String>, Option<String>)> {
    if u32::from(event.kind.as_u16()) != PROFILE_KIND {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&event.content).ok()?;
    let pick = |field: &str| {
        value
            .get(field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Some((pick("name"), pick("display_name")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Timestamp};

    fn keys() -> Keys {
        Keys::generate()
    }

    fn signed(builder: EventBuilder, keys: &Keys) -> Event {
        builder
            .custom_created_at(Timestamp::from(1_700_000_000))
            .sign_with_keys(keys)
            .unwrap()
    }

    fn event_of(kind: u16, tags: Vec<&[&str]>, content: &str) -> EventBuilder {
        let parsed: Vec<Tag> = tags
            .iter()
            .map(|t| Tag::parse(t.iter().copied()).unwrap())
            .collect();
        // Self p-tags are stripped unless allowed; Buzz builders allow them.
        EventBuilder::new(Kind::Custom(kind), content)
            .tags(parsed)
            .allow_self_tagging()
    }

    fn message(tags: Vec<&[&str]>, content: &str) -> EventBuilder {
        event_of(9, tags, content)
    }

    fn find_row<'a>(rows: &'a mut [Row], id: &str) -> &'a mut Row {
        rows.iter_mut().find(|r| r.event_id == id).unwrap()
    }

    #[test]
    fn direct_and_nested_replies_resolve_their_thread_markers() {
        let keys = keys();
        let direct = signed(
            message(
                vec![&["h", "c"], &["e", "a".repeat(64).as_str(), "", "reply"]],
                "answer",
            ),
            &keys,
        );
        assert_eq!(root_of(&direct).as_deref(), Some("a".repeat(64).as_str()));
        assert_eq!(parent_of(&direct).as_deref(), Some("a".repeat(64).as_str()));

        let nested = signed(
            message(
                vec![
                    &["h", "c"],
                    &["e", &"b".repeat(64), "", "root"],
                    &["e", &"c".repeat(64), "", "reply"],
                ],
                "deep answer",
            ),
            &keys,
        );
        assert_eq!(root_of(&nested).as_deref(), Some("b".repeat(64).as_str()));
        assert_eq!(parent_of(&nested).as_deref(), Some("c".repeat(64).as_str()));
    }

    #[test]
    fn mention_broadcast_and_attachment_markers_travel_with_the_row() {
        let keys = keys();
        let me = keys.public_key().to_hex();
        let ev = signed(
            message(
                vec![
                    &["h", "c"],
                    &["p", &me],
                    &["broadcast", "1"],
                    &[
                        "imeta",
                        "url https://x/f.png",
                        "m image/png",
                        "x abc",
                        "filename f.png",
                    ],
                ],
                "look [f](https://x/f.png)",
            ),
            &keys,
        );
        let row = row_from_event(&ev, &me);
        assert!(row.mentions_me);
        assert!(row.broadcast);
        assert_eq!(row.attachment.as_deref(), Some("f.png"));
    }

    #[test]
    fn imeta_without_a_filename_falls_back_to_the_url_tail() {
        let parts: Vec<String> = [
            "imeta",
            "url https://host/dir/report.pdf",
            "m application/pdf",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(imeta_filename(&parts).as_deref(), Some("report.pdf"));
    }

    #[test]
    fn reactions_accumulate_and_decrement_per_emoji() {
        let mut rows = vec![Row {
            event_id: "r1".into(),
            reactions: vec![("\u{1f44d}".into(), 2)],
            ..base_row()
        }];
        apply_overlay(
            find_row(&mut rows, "r1"),
            &Overlay::Reaction {
                emoji: "\u{1f44d}".into(),
            },
        );
        assert_eq!(find_row(&mut rows, "r1").reactions[0].1, 3);
        apply_overlay(
            find_row(&mut rows, "r1"),
            &Overlay::Reaction {
                emoji: "-\u{1f44d}".into(),
            },
        );
        assert_eq!(find_row(&mut rows, "r1").reactions[0].1, 2);
        apply_overlay(
            find_row(&mut rows, "r1"),
            &Overlay::Reaction {
                emoji: "\u{1f680}".into(),
            },
        );
        assert_eq!(find_row(&mut rows, "r1").reactions.len(), 2);
    }

    #[test]
    fn a_decrement_to_zero_removes_the_counter() {
        let mut row = Row {
            reactions: vec![("x".into(), 1)],
            ..base_row()
        };
        apply_overlay(&mut row, &Overlay::Reaction { emoji: "-x".into() });
        assert!(row.reactions.is_empty());
    }

    #[test]
    fn an_edit_replaces_the_body_wholesale_and_marks_the_row() {
        let mut row = base_row();
        apply_overlay(&mut row, &Overlay::Edit { body: "new".into() });
        assert_eq!(row.body, "new");
        assert!(row.edited);
    }

    #[test]
    fn kind_five_against_a_reaction_id_maps_to_a_removal_overlay() {
        let keys = keys();
        let reaction_id = "f".repeat(64);
        let del = signed(event_of(5, vec![&["e", &reaction_id]], ""), &keys);
        let lookup = |id: &str| -> Option<(String, String)> {
            (id == reaction_id).then(|| ("row1".into(), "\u{1f44d}".into()))
        };
        let (target, overlay) = overlay_of(&del, &lookup).unwrap();
        assert_eq!(target, "row1", "the decrement lands on the message row");
        assert_eq!(
            overlay,
            Overlay::Reaction {
                emoji: "-\u{1f44d}".into()
            },
            "the reaction counter is decremented, not the row removed"
        );
    }

    #[test]
    fn kind_nine_zero_zero_five_against_a_message_maps_to_a_row_delete() {
        let keys = keys();
        let del = signed(
            event_of(9005, vec![&["h", "c"], &["e", &"a".repeat(64)]], ""),
            &keys,
        );
        assert_eq!(del.kind.as_u16(), 9005);
        let (target, overlay) = overlay_of(&del, &|_| None).unwrap();
        assert_eq!(target, "a".repeat(64));
        assert_eq!(overlay, Overlay::Delete);
    }

    #[test]
    fn an_empty_reaction_body_becomes_the_default_emoji() {
        let keys = keys();
        let ev = signed(event_of(7, vec![&["e", &"a".repeat(64)]], "  "), &keys);
        let (_, overlay) = overlay_of(&ev, &|_| None).unwrap();
        assert_eq!(
            overlay,
            Overlay::Reaction {
                emoji: DEFAULT_REACTION.to_owned()
            }
        );
    }

    #[test]
    fn an_overlay_without_a_target_is_dropped() {
        let keys = keys();
        let ev = signed(message(vec![], "body"), &keys);
        assert!(overlay_of(&ev, &|_| None).is_none());
    }

    #[test]
    fn pending_rows_mirror_the_final_tag_shape() {
        let me = "ab".repeat(32);
        let root = "a".repeat(64);
        let direct = pending_row(
            &me,
            "pending:1",
            "hi",
            &Some((root.clone(), root.clone())),
            false,
            12,
        );
        assert_eq!(direct.root_id, None, "a direct reply carries one tag");
        assert_eq!(direct.parent_id.as_deref(), Some(root.as_str()));
        assert_eq!(direct.pubkey, me);
        assert!(direct.pending);

        let nested = pending_row(
            &me,
            "pending:2",
            "hi",
            &Some((root.clone(), "b".repeat(64))),
            false,
            12,
        );
        assert!(nested.root_id.is_some() && nested.parent_id.is_some());
    }

    #[test]
    fn profile_names_prefer_display_name_and_skip_empty_strings() {
        let keys = keys();
        let ev = signed(
            EventBuilder::new(Kind::Metadata, r#"{"name":"bob","display_name":"Bob B."}"#),
            &keys,
        );
        let (name, display) = profile_names(&ev).unwrap();
        assert_eq!(name.as_deref(), Some("bob"));
        assert_eq!(display.as_deref(), Some("Bob B."));

        let empty = signed(EventBuilder::new(Kind::Metadata, r#"{"name":""}"#), &keys);
        let (name, display) = profile_names(&empty).unwrap();
        assert_eq!(name, None);
        assert_eq!(display, None);
    }

    fn base_row() -> Row {
        Row {
            event_id: "r1".into(),
            pubkey: "p".into(),
            author: "a".into(),
            created_at: 0,
            body: "b".into(),
            kind: 9,
            root_id: None,
            parent_id: None,
            broadcast: false,
            mentions_me: false,
            reactions: Vec::new(),
            attachment: None,
            pending: false,
            uncertain: false,
            edited: false,
        }
    }
}
