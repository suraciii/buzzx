//! Send-time mention resolution: the recipients a draft's text asks to notify.
//!
//! The rules are the contract in `docs/tui-use.md` § *Resolve mentions when
//! sending*. The work is pure: the caller reads current membership and member
//! profiles, and this module decides which identities the visible text names -
//! or why the draft cannot be published as written.
//!
//! The pipeline is the SDK's own, in the SDK's order:
//!
//! ```text
//! strip_code_regions ──► extract_at_mentions_with_known ──► names
//! strip_code_regions ──► extract_nostr_uris ──────────────► exact identities
//! names + profiles ────► match_names_to_profiles ─────────► pubkeys
//! ```

use std::collections::HashMap;

use buzz_sdk::mentions::{
    MENTION_CAP, extract_at_mentions_with_known, extract_nostr_uris, normalize_mention_pubkeys,
    strip_code_regions,
};
use nostr::{PublicKey, ToBech32};

/// The membership and member profiles one preflight reads.
///
/// `members` is every current member's lowercase hex public key. `profiles`
/// holds the raw kind 0 `content` of the members that have one; a member
/// without a profile cannot be named in a draft, which is why the pair is
/// kept together rather than as two independent lists.
#[derive(Debug, Default, Clone)]
pub struct Directory {
    pub members: Vec<String>,
    pub profiles: Vec<(String, String)>,
}

impl Directory {
    /// The display names this directory can match. A name that resolves to a
    /// non-member cannot exist: profiles are read for member authors only.
    fn display_names(&self) -> Vec<String> {
        self.profiles
            .iter()
            .filter_map(|(_, content)| display_name(content))
            .collect()
    }

    /// Lowercased display name to the pubkeys that answer to it.
    fn name_map(&self) -> HashMap<String, Vec<String>> {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (pubkey, content) in &self.profiles {
            let Some(name) = display_name(content) else {
                continue;
            };
            map.entry(name.to_ascii_lowercase())
                .or_default()
                .push(pubkey.clone());
        }
        map
    }
}

/// Why a draft's mention text cannot be published as written.
///
/// Every variant is a refusal before publication: the relay never saw the
/// message, and the composer keeps the draft and its reply target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A named fragment matches no current member's display name.
    Unknown { name: String },
    /// A named fragment matches more than one member and no exact reference
    /// in the draft picks one.
    Ambiguous {
        name: String,
        candidates: Vec<String>,
    },
    /// An exact `nostr:npub…` reference names an identity that is not a
    /// current member.
    NotMember { reference: String, pubkey: String },
    /// The draft names more recipients than the SDK's builder accepts.
    OverCap { count: usize },
    /// The membership or profile read failed. An incomplete list is not an
    /// empty one, so the draft is not published against a guess.
    LookupFailed { reason: String },
}

impl Block {
    /// The one-line status: what is wrong and what to do next.
    pub fn summary(&self) -> String {
        match self {
            Block::Unknown { name } => format!(
                "mention \"@{name}\" is not a current member here; fix the spelling, or use a code span to keep the text plain"
            ),
            Block::Ambiguous { name, candidates } => format!(
                "mention \"@{name}\" matches {} members; replace it with one exact reference (? for identities)",
                candidates.len()
            ),
            Block::NotMember { reference, .. } => format!(
                "reference {reference} is not a current member here; add the identity to the conversation, or remove it"
            ),
            Block::OverCap { count } => format!(
                "the draft names {count} recipients; the relay accepts at most {MENTION_CAP}"
            ),
            Block::LookupFailed { reason } => format!(
                "the mention check could not read this conversation: {reason}; nothing was sent"
            ),
        }
    }

    /// The lines the scrollable help surface shows: the exact references to
    /// paste back, one per line. A short key is never offered as input.
    pub fn details(&self) -> Vec<String> {
        match self {
            Block::Unknown { name } => {
                vec![format!("@{name}: no current member answers to this name")]
            }
            Block::Ambiguous { name, candidates } => candidates
                .iter()
                .map(|pubkey| format!("@{name} -> {}", reference(pubkey)))
                .collect(),
            Block::NotMember { reference, .. } => {
                vec![format!(
                    "{reference}: not a current member of this conversation"
                )]
            }
            Block::OverCap { count } => vec![format!(
                "{count} recipients: remove {} or more",
                count - MENTION_CAP
            )],
            Block::LookupFailed { reason } => vec![format!("lookup failed: {reason}")],
        }
    }
}

/// The full `nostr:npub…` reference for a hex public key. A key that cannot be
/// encoded stays as it came in, which keeps the line honest rather than empty.
fn reference(pubkey: &str) -> String {
    match PublicKey::parse(pubkey) {
        Ok(key) => key
            .to_bech32()
            .map(|npub| format!("nostr:{npub}"))
            .unwrap_or_else(|_| pubkey.to_owned()),
        Err(_) => pubkey.to_owned(),
    }
}

/// Whether a draft needs the membership and profile read at all. A draft with
/// no mention input is sent without it, and an `@` inside a word (an email) is
/// not mention input.
pub fn needs_lookup(content: &str) -> bool {
    let stripped = strip_code_regions(content);
    !extract_nostr_uris(&stripped).is_empty() || starts_a_name(&stripped)
}

/// Whether any `@` starts a token that could name someone: at the start of the
/// text or after whitespace, with something other than whitespace after it.
/// This is the SDK extractor's own rule for where a name may begin, and it is
/// deliberately wider than the extractor's character set: a display name may
/// be Unicode, which only the known-name match can read.
fn starts_a_name(content: &str) -> bool {
    let bytes = content.as_bytes();
    content.match_indices('@').any(|(i, _)| {
        let starts = i == 0 || bytes[i - 1].is_ascii_whitespace();
        starts
            && bytes
                .get(i + 1)
                .is_some_and(|next| !next.is_ascii_whitespace())
    })
}

/// The recipients this draft asks to notify, or why it cannot be sent.
///
/// A unique complete member name resolves to that member. An exact reference
/// resolves an otherwise ambiguous name to the candidate it names, but never
/// hides another unresolved fragment. Duplicates collapse, and the sender's
/// own key is kept: naming yourself is allowed.
pub fn plan(content: &str, directory: &Directory) -> Result<Vec<String>, Block> {
    let stripped = strip_code_regions(content);
    let explicit = extract_nostr_uris(&stripped);

    let names = directory.display_names();
    let known: Vec<&str> = names.iter().map(String::as_str).collect();
    let named = extract_at_mentions_with_known(&stripped, &known);

    let map = directory.name_map();
    let mut resolved: Vec<String> = Vec::new();
    for name in &named {
        let candidates = map.get(name).map(Vec::as_slice).unwrap_or_default();
        match candidates {
            [] => return Err(Block::Unknown { name: name.clone() }),
            [only] => resolved.push(only.clone()),
            many => match many.iter().find(|pubkey| explicit.contains(pubkey)) {
                // The draft says which of them it means.
                Some(picked) => resolved.push(picked.clone()),
                None => {
                    return Err(Block::Ambiguous {
                        name: name.clone(),
                        candidates: many.to_vec(),
                    });
                }
            },
        }
    }

    let members: std::collections::HashSet<&str> =
        directory.members.iter().map(String::as_str).collect();
    for pubkey in &explicit {
        if !members.contains(pubkey.as_str()) {
            return Err(Block::NotMember {
                reference: reference(pubkey),
                pubkey: pubkey.clone(),
            });
        }
    }

    let mut all: Vec<String> = resolved;
    all.extend(explicit);
    let mentions = normalize_mention_pubkeys(&all, None);
    if mentions.len() > MENTION_CAP {
        return Err(Block::OverCap {
            count: mentions.len(),
        });
    }
    Ok(mentions)
}

/// A profile's display name, the field the SDK matches on. `name` is read only
/// when `display_name` is absent, which is the SDK's own precedence.
fn display_name(content_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(content_json).ok()?;
    value
        .get("display_name")
        .or_else(|| value.get("name"))
        .and_then(|v| v.as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALPHA: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const BETA: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const GAMMA: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const OUTSIDER: &str = "4444444444444444444444444444444444444444444444444444444444444444";

    fn profile(name: &str) -> String {
        format!("{{\"display_name\":\"{name}\"}}")
    }

    fn directory() -> Directory {
        Directory {
            members: vec![ALPHA.to_owned(), BETA.to_owned(), GAMMA.to_owned()],
            profiles: vec![
                (ALPHA.to_owned(), profile("Buzzx Build")),
                (BETA.to_owned(), profile("Buzzx Product")),
                (GAMMA.to_owned(), profile("张三")),
            ],
        }
    }

    #[test]
    fn a_unique_complete_name_resolves_to_its_member() {
        let planned = plan("hey @Buzzx Build, take this", &directory()).expect("resolves");
        assert_eq!(planned, vec![ALPHA.to_owned()]);
    }

    #[test]
    fn a_multi_word_and_a_unicode_name_resolve() {
        let planned = plan("@Buzzx Build and @张三 please look", &directory()).expect("resolves");
        assert_eq!(planned, vec![ALPHA.to_owned(), GAMMA.to_owned()]);
    }

    #[test]
    fn a_name_is_matched_case_insensitively() {
        let planned = plan("@buzzx product ping", &directory()).expect("resolves");
        assert_eq!(planned, vec![BETA.to_owned()]);
    }

    #[test]
    fn repeated_names_collapse() {
        let planned = plan("@Buzzx Build and @buzzx build again", &directory()).expect("resolves");
        assert_eq!(planned, vec![ALPHA.to_owned()]);
    }

    #[test]
    fn an_unknown_name_blocks_the_draft() {
        let block = plan("@Nobody here", &directory()).expect_err("blocks");
        assert_eq!(
            block,
            Block::Unknown {
                name: "nobody".to_owned()
            }
        );
        assert!(block.summary().contains("@nobody"));
    }

    #[test]
    fn a_partial_name_is_not_expanded() {
        let block = plan("@Buzzx", &directory()).expect_err("blocks");
        assert_eq!(
            block,
            Block::Unknown {
                name: "buzzx".to_owned()
            }
        );
    }

    #[test]
    fn a_duplicate_name_is_ambiguous_and_lists_its_candidates() {
        let mut two = directory();
        two.profiles
            .push((OUTSIDER.to_owned(), profile("Buzzx Build")));
        two.members.push(OUTSIDER.to_owned());
        let block = plan("@Buzzx Build ping", &two).expect_err("blocks");
        let Block::Ambiguous { name, candidates } = &block else {
            panic!("expected ambiguity, got {block:?}");
        };
        assert_eq!(name, "buzzx build");
        assert_eq!(candidates, &vec![ALPHA.to_owned(), OUTSIDER.to_owned()]);
        let details = block.details();
        assert_eq!(details.len(), 2);
        assert!(
            details[0].starts_with("@buzzx build -> nostr:npub1"),
            "{details:?}"
        );
        assert!(details[1].contains(&reference(OUTSIDER)), "{details:?}");
    }

    #[test]
    fn an_exact_reference_picks_one_ambiguous_candidate() {
        let mut two = directory();
        two.profiles
            .push((OUTSIDER.to_owned(), profile("Buzzx Build")));
        two.members.push(OUTSIDER.to_owned());
        let text = format!("@Buzzx Build {} is the one", reference(OUTSIDER));
        let planned = plan(&text, &two).expect("resolves");
        assert_eq!(planned, vec![OUTSIDER.to_owned()]);
    }

    #[test]
    fn an_exact_reference_does_not_excuse_another_unresolved_name() {
        let text = format!("{} and @Nobody", reference(ALPHA));
        let block = plan(&text, &directory()).expect_err("blocks");
        assert_eq!(
            block,
            Block::Unknown {
                name: "nobody".to_owned()
            }
        );
    }

    #[test]
    fn an_exact_reference_to_a_non_member_blocks_the_draft() {
        let text = format!("hello {}", reference(OUTSIDER));
        let block = plan(&text, &directory()).expect_err("blocks");
        let Block::NotMember { reference, pubkey } = &block else {
            panic!("expected a non-member block, got {block:?}");
        };
        assert!(reference.starts_with("nostr:npub1"));
        assert_eq!(pubkey, OUTSIDER);
    }

    #[test]
    fn a_member_reference_carries_the_recipient_without_visible_text() {
        let text = format!("{} look at this", reference(BETA));
        let planned = plan(&text, &directory()).expect("resolves");
        assert_eq!(planned, vec![BETA.to_owned()]);
    }

    #[test]
    fn code_spans_fences_and_emails_do_not_name_anyone() {
        let text = "use `@Buzzx Build` here\n```\n@Buzzx Product\n```\nmail me at user@example.com";
        assert!(!needs_lookup(text), "a code span is not mention input");
        assert_eq!(
            plan(text, &directory()).expect("resolves"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_draft_without_mention_input_needs_no_lookup() {
        assert!(!needs_lookup("plain message"));
        assert!(!needs_lookup("@ not a name"));
        assert!(!needs_lookup("mail me at user@example.com"));
        assert!(needs_lookup("@Buzzx Build"));
        assert!(needs_lookup("@张三 please look"));
        assert!(needs_lookup(&reference(ALPHA)));
    }

    #[test]
    fn a_member_without_a_profile_cannot_be_named() {
        let sparse = Directory {
            members: vec![ALPHA.to_owned()],
            profiles: vec![],
        };
        let block = plan("@Buzzx Build", &sparse).expect_err("blocks");
        assert!(matches!(block, Block::Unknown { .. }));
    }

    #[test]
    fn naming_more_recipients_than_the_cap_blocks_without_truncating() {
        let mut many = Directory::default();
        let mut text = String::new();
        for i in 0..MENTION_CAP + 1 {
            let pubkey = format!("{i:064x}");
            many.members.push(pubkey.clone());
            many.profiles.push((pubkey, profile(&format!("member{i}"))));
            text.push_str(&format!("@member{i} "));
        }
        let block = plan(&text, &many).expect_err("blocks");
        assert_eq!(
            block,
            Block::OverCap {
                count: MENTION_CAP + 1
            }
        );
        assert!(block.summary().contains("51"));
    }

    #[test]
    fn the_cap_counts_unique_recipients() {
        let mut many = Directory::default();
        let mut text = String::new();
        for i in 0..MENTION_CAP {
            let pubkey = format!("{i:064x}");
            many.members.push(pubkey.clone());
            many.profiles.push((pubkey, profile(&format!("member{i}"))));
            text.push_str(&format!("@member{i} "));
        }
        // The same names again do not add recipients.
        let repeated = format!("{text}{text}");
        let planned = plan(&repeated, &many).expect("resolves");
        assert_eq!(planned.len(), MENTION_CAP);
    }

    #[test]
    fn a_lookup_failure_is_a_block_with_its_reason() {
        let block = Block::LookupFailed {
            reason: "relay unreachable".to_owned(),
        };
        assert!(block.summary().contains("relay unreachable"));
        assert_eq!(block.details(), vec!["lookup failed: relay unreachable"]);
    }
}
