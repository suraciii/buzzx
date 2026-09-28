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

use std::collections::{HashMap, HashSet};

use buzz_sdk::mentions::{
    MENTION_CAP, extract_at_mentions_with_known, extract_nostr_uris, normalize_mention_pubkeys,
    strip_code_regions,
};
use nostr::{PublicKey, ToBech32};

/// How many rows one suggestion list shows. It is a rendering bound only: the
/// signed recipient cap stays the SDK's own.
pub const SUGGESTION_CAP: usize = 50;

/// One identity a composer can offer for an `@` query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Lowercase hex public key.
    pub pubkey: String,
    /// The display name the profile carries. It is the label an insertion
    /// writes into the draft.
    pub label: String,
    /// The profile's avatar URL, when it carries one.
    pub picture: Option<String>,
    /// The relay's member role is `bot`: this identity works for someone.
    pub agent: bool,
    /// The relay's member role is `owner` or `admin`.
    pub admin: bool,
}

impl Candidate {
    /// The display-only disambiguator a repeated name needs. It is never
    /// accepted as an input identity.
    pub fn short_key(&self) -> String {
        let head: String = self.pubkey.chars().take(8).collect();
        format!("{head}…")
    }
}

/// One occurrence selected from the picker, and the identity it stands for.
///
/// `start` is the byte position of `@` in the original draft. Distinct
/// occurrences with the same label can select different members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub start: usize,
    pub label: String,
    pub pubkey: String,
}

/// The `@query` under the cursor: what a picker filters on, and the range an
/// insertion replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    /// The byte offset of the `@` in the draft.
    pub start: usize,
    /// The byte offset just past the query text.
    pub end: usize,
    /// The text between the `@` and the cursor.
    pub query: String,
}

/// The membership and member profiles one preflight reads.
///
/// `members` is every current member's lowercase hex public key. `profiles`
/// holds the raw kind 0 `content` of the members that have one; a member
/// without a profile cannot be named in a draft, which is why the pair is
/// kept together rather than as two independent lists. `roles` holds the
/// relay's role for the members its roster describes.
#[derive(Debug, Default, Clone)]
pub struct Directory {
    pub members: Vec<String>,
    pub profiles: Vec<(String, String)>,
    pub roles: Vec<(String, String)>,
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

    /// The identities a suggestion list can offer, in roster order.
    ///
    /// A member without a profile carries no name to insert, so it is not
    /// offered: the preflight could never resolve a name for it anyway.
    pub fn candidates(&self) -> Vec<Candidate> {
        let profiles: HashMap<&str, &str> = self
            .profiles
            .iter()
            .map(|(pubkey, content)| (pubkey.as_str(), content.as_str()))
            .collect();
        let roles: HashMap<&str, &str> = self
            .roles
            .iter()
            .map(|(pubkey, role)| (pubkey.as_str(), role.as_str()))
            .collect();
        self.members
            .iter()
            .filter_map(|pubkey| {
                let content = profiles.get(pubkey.as_str())?;
                let label = display_name(content)?;
                let role = roles.get(pubkey.as_str()).copied().unwrap_or_default();
                Some(Candidate {
                    pubkey: pubkey.clone(),
                    label,
                    picture: picture(content),
                    agent: role == "bot",
                    admin: matches!(role, "owner" | "admin"),
                })
            })
            .collect()
    }
}

/// The `@query` the cursor sits in, when a composer should offer suggestions.
///
/// The `@` opens a name only at a word boundary, the query never crosses
/// whitespace, and an `@` inside a code region stays literal. All three rules
/// are the send-time preflight's own, so a picker and the resolution that
/// follows it read the same fragments.
pub fn active_query(text: &str, cursor: usize) -> Option<Active> {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let at = text[..cursor].rfind('@')?;
    if at > 0 && !text.as_bytes()[at - 1].is_ascii_whitespace() {
        return None;
    }
    let query = &text[at + 1..cursor];
    if query.chars().any(char::is_whitespace) || in_code(text, at) {
        return None;
    }
    // The fragment may continue past the cursor; a selection replaces all of
    // it, not just the part already typed.
    let tail = text[cursor..]
        .find(char::is_whitespace)
        .unwrap_or(text.len() - cursor);
    Some(Active {
        start: at,
        end: cursor + tail,
        query: query.to_owned(),
    })
}

/// Whether the offset sits inside a code region, by the stripper's own shape:
/// a fence is three backticks at the start of a line, and an inline span runs
/// from one backtick to the next on the same line.
fn in_code(text: &str, at: usize) -> bool {
    let mut fenced = false;
    let mut line_start = 0usize;
    for line in text.split('\n') {
        let end = line_start + line.len();
        if end < at {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
            }
            line_start = end + 1;
            continue;
        }
        if fenced {
            return true;
        }
        let mut open: Option<usize> = None;
        for (i, c) in line.char_indices() {
            if c != '`' {
                continue;
            }
            let tick = line_start + i;
            match open.take() {
                Some(start) if start < at && at < tick => return true,
                Some(_) => {}
                None => open = Some(tick),
            }
        }
        return false;
    }
    fenced
}

/// Order the candidates for one query.
///
/// The tiers are the specification's: an exact label first, then a label
/// prefix, a whole word, a word prefix, and finally the public key. Within a
/// tier the roster's own order holds, so two rows never swap between
/// keystrokes.
pub fn rank(query: &str, candidates: &[Candidate]) -> Vec<Candidate> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return candidates.iter().take(SUGGESTION_CAP).cloned().collect();
    }
    let mut scored: Vec<(u8, &Candidate)> = candidates
        .iter()
        .filter_map(|candidate| tier(&needle, candidate).map(|tier| (tier, candidate)))
        .collect();
    scored.sort_by_key(|(tier, _)| *tier);
    scored
        .into_iter()
        .take(SUGGESTION_CAP)
        .map(|(_, candidate)| candidate.clone())
        .collect()
}

/// The rank a candidate's match earns, or None when the query matches nothing
/// about it.
fn tier(needle: &str, candidate: &Candidate) -> Option<u8> {
    let label = candidate.label.to_lowercase();
    if label == needle {
        return Some(0);
    }
    if label.starts_with(needle) {
        return Some(1);
    }
    let words = || label.split_whitespace();
    if words().any(|word| word == needle) {
        return Some(2);
    }
    if words().any(|word| word.starts_with(needle)) {
        return Some(3);
    }
    let key = candidate.pubkey.to_lowercase();
    if key.starts_with(needle) {
        return Some(4);
    }
    if key.contains(needle) {
        return Some(5);
    }
    None
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
    /// The stable name a structured result carries. It is an identifier a
    /// caller branches on, not prose.
    pub fn kind(&self) -> &'static str {
        match self {
            Block::Unknown { .. } => "unknown",
            Block::Ambiguous { .. } => "ambiguous",
            Block::NotMember { .. } => "not_member",
            Block::OverCap { .. } => "over_cap",
            Block::LookupFailed { .. } => "directory_failed",
        }
    }

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

/// Whether `c` may continue the SDK's fallback name token:
/// `[A-Za-z0-9._-]`, the only characters it reads when no known name matches.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
}

/// The SDK's own rule for where a matched name ends: end of text, whitespace,
/// or one of these closing marks.
fn ends_a_name(s: &str) -> bool {
    s.chars().next().is_none_or(|c| {
        c.is_ascii_whitespace() || matches!(c, ',' | ';' | '.' | '!' | '?' | ':' | ')' | ']' | '}')
    })
}

/// The first `@` fragment the SDK extractor cannot read, in text order.
///
/// The extractor reads a name two ways: a known display name, matched
/// longest-first at the `@`, or a single token of `[A-Za-z0-9._-]`. A name
/// that begins with any other character - a display name in Chinese, say -
/// matches neither, and the extractor reports nothing for it. Left alone, the
/// fragment would vanish from the recipient list while the message published
/// as written, which is the one outcome the contract forbids: a fragment that
/// visibly names someone is either resolved or the draft does not go out.
///
/// The walk is the extractor's own - the whitespace rule, the longest-known-
/// name-first match, the word boundary, the lowercase - so a fragment reported
/// here is exactly one the extractor skipped. Its text runs to the next
/// whitespace, which is what the reader sees as the name.
fn unreadable_name(content: &str, known: &[&str]) -> Option<String> {
    let mut sorted: Vec<&str> = known
        .iter()
        .copied()
        .filter(|name| !name.trim().is_empty())
        .collect();
    sorted.sort_by_key(|name| std::cmp::Reverse(name.len()));

    for (i, _) in content.match_indices('@') {
        if i != 0 && !content.as_bytes()[i - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = &content[i + 1..];
        let matched_known = sorted.iter().any(|&name| {
            rest.get(..name.len()).is_some_and(|head| {
                head.eq_ignore_ascii_case(name) && ends_a_name(&rest[name.len()..])
            })
        });
        if matched_known {
            continue;
        }
        let Some(first) = rest.chars().next() else {
            continue;
        };
        if first.is_ascii_whitespace() || is_token_char(first) {
            // The extractor reads whitespace as no name and token characters
            // as its fallback name, which resolution reports if unknown.
            continue;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        // Reported as typed: the extractor never read this fragment, so there
        // is no lowercased name to keep, and the reader has to find the words
        // they wrote in the draft.
        return Some(rest[..end].to_owned());
    }
    None
}

/// The recipients this draft asks to notify, or why it cannot be sent.
///
/// Each picker binding resolves only its own occurrence. Remaining fragments
/// use the SDK name extractor, and explicit references are checked against
/// current membership before the message is signed.
pub fn plan(
    content: &str,
    directory: &Directory,
    bindings: &[Binding],
) -> Result<Vec<String>, Block> {
    let stripped = strip_code_regions(content);
    let explicit = extract_nostr_uris(&stripped);
    let mut remaining = content.to_owned();
    let map = directory.name_map();
    let mut resolved = Vec::new();
    let mut used = HashSet::new();
    for binding in bindings {
        let fragment = format!("@{}", binding.label);
        let valid = content.get(binding.start..binding.start + fragment.len())
            == Some(fragment.as_str())
            && (binding.start == 0 || content.as_bytes()[binding.start - 1].is_ascii_whitespace())
            && content
                .get(binding.start + fragment.len()..)
                .is_some_and(ends_a_name)
            && !in_code(content, binding.start)
            && used.insert(binding.start);
        if !valid {
            return Err(Block::Unknown {
                name: binding.label.clone(),
            });
        }
        let candidates = map
            .get(&binding.label.to_ascii_lowercase())
            .map(Vec::as_slice)
            .unwrap_or_default();
        if !directory.members.contains(&binding.pubkey) {
            return Err(Block::NotMember {
                reference: reference(&binding.pubkey),
                pubkey: binding.pubkey.clone(),
            });
        }
        if !candidates.contains(&binding.pubkey) {
            return Err(Block::Unknown {
                name: binding.label.clone(),
            });
        }
        resolved.push(binding.pubkey.clone());
        remaining.replace_range(
            binding.start..binding.start + fragment.len(),
            &" ".repeat(fragment.len()),
        );
    }

    let stripped = strip_code_regions(&remaining);
    let names = directory.display_names();
    let known: Vec<&str> = names.iter().map(String::as_str).collect();
    if let Some(name) = unreadable_name(&stripped, &known) {
        return Err(Block::Unknown { name });
    }
    for name in extract_at_mentions_with_known(&stripped, &known) {
        let candidates = map.get(&name).map(Vec::as_slice).unwrap_or_default();
        match candidates {
            [] => return Err(Block::Unknown { name }),
            [only] => resolved.push(only.clone()),
            many => resolved.push(pick(&name, many, &explicit)?),
        }
    }

    let members: HashSet<&str> = directory.members.iter().map(String::as_str).collect();
    for pubkey in &explicit {
        if !members.contains(pubkey.as_str()) {
            return Err(Block::NotMember {
                reference: reference(pubkey),
                pubkey: pubkey.clone(),
            });
        }
    }

    resolved.extend(explicit);
    let mentions = normalize_mention_pubkeys(&resolved, None);
    if mentions.len() > MENTION_CAP {
        return Err(Block::OverCap {
            count: mentions.len(),
        });
    }
    Ok(mentions)
}

/// An explicit reference can disambiguate an unbound name. Otherwise the
/// user must select the occurrence or change the text.
fn pick(name: &str, candidates: &[String], explicit: &[String]) -> Result<String, Block> {
    if let Some(pubkey) = candidates.iter().find(|key| explicit.contains(key)) {
        return Ok(pubkey.clone());
    }
    Err(Block::Ambiguous {
        name: name.to_owned(),
        candidates: candidates.to_vec(),
    })
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

/// A profile's avatar URL, when it carries one. It is presentation only: an
/// empty or absent picture is a row without an avatar, never a reason to drop
/// the identity.
fn picture(content_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(content_json).ok()?;
    value
        .get("picture")
        .and_then(|v| v.as_str())
        .filter(|url| !url.is_empty())
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
            roles: vec![(ALPHA.to_owned(), "owner".to_owned())],
        }
    }

    #[test]
    fn a_unique_complete_name_resolves_to_its_member() {
        let planned = plan("hey @Buzzx Build, take this", &directory(), &[]).expect("resolves");
        assert_eq!(planned, vec![ALPHA.to_owned()]);
    }

    #[test]
    fn a_multi_word_and_a_unicode_name_resolve() {
        let planned =
            plan("@Buzzx Build and @张三 please look", &directory(), &[]).expect("resolves");
        assert_eq!(planned, vec![ALPHA.to_owned(), GAMMA.to_owned()]);
    }

    #[test]
    fn a_name_is_matched_case_insensitively() {
        let planned = plan("@buzzx product ping", &directory(), &[]).expect("resolves");
        assert_eq!(planned, vec![BETA.to_owned()]);
    }

    #[test]
    fn repeated_names_collapse() {
        let planned =
            plan("@Buzzx Build and @buzzx build again", &directory(), &[]).expect("resolves");
        assert_eq!(planned, vec![ALPHA.to_owned()]);
    }

    #[test]
    fn an_unknown_name_blocks_the_draft() {
        let block = plan("@Nobody here", &directory(), &[]).expect_err("blocks");
        assert_eq!(
            block,
            Block::Unknown {
                name: "nobody".to_owned()
            }
        );
        assert!(block.summary().contains("@nobody"));
    }

    #[test]
    fn an_unknown_unicode_name_blocks_the_draft() {
        let block = plan("@李四 please look", &directory(), &[]).expect_err("blocks");
        assert_eq!(
            block,
            Block::Unknown {
                name: "李四".to_owned()
            }
        );
        assert!(block.summary().contains("@李四"));
    }

    #[test]
    fn a_partial_unicode_name_is_not_expanded() {
        let block = plan("@张 please look", &directory(), &[]).expect_err("blocks");
        assert_eq!(
            block,
            Block::Unknown {
                name: "张".to_owned()
            }
        );
    }

    #[test]
    fn an_unreadable_fragment_is_reported_as_typed() {
        let block = plan("@ÄBC please look", &directory(), &[]).expect_err("blocks");
        assert_eq!(
            block,
            Block::Unknown {
                name: "ÄBC".to_owned()
            }
        );
    }

    #[test]
    fn a_valid_name_does_not_excuse_an_unknown_unicode_name() {
        let block = plan("@Buzzx Build and @李四 please", &directory(), &[]).expect_err("blocks");
        assert_eq!(
            block,
            Block::Unknown {
                name: "李四".to_owned()
            }
        );
    }

    #[test]
    fn a_partial_name_is_not_expanded() {
        let block = plan("@Buzzx", &directory(), &[]).expect_err("blocks");
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
        let block = plan("@Buzzx Build ping", &two, &[]).expect_err("blocks");
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
        let planned = plan(&text, &two, &[]).expect("resolves");
        assert_eq!(planned, vec![OUTSIDER.to_owned()]);
    }

    #[test]
    fn an_exact_reference_does_not_excuse_another_unresolved_name() {
        let text = format!("{} and @Nobody", reference(ALPHA));
        let block = plan(&text, &directory(), &[]).expect_err("blocks");
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
        let block = plan(&text, &directory(), &[]).expect_err("blocks");
        let Block::NotMember { reference, pubkey } = &block else {
            panic!("expected a non-member block, got {block:?}");
        };
        assert!(reference.starts_with("nostr:npub1"));
        assert_eq!(pubkey, OUTSIDER);
    }

    #[test]
    fn a_member_reference_carries_the_recipient_without_visible_text() {
        let text = format!("{} look at this", reference(BETA));
        let planned = plan(&text, &directory(), &[]).expect("resolves");
        assert_eq!(planned, vec![BETA.to_owned()]);
    }

    #[test]
    fn code_spans_fences_and_emails_do_not_name_anyone() {
        let text = "use `@Buzzx Build` here\n```\n@Buzzx Product\n```\nmail me at user@example.com";
        assert!(!needs_lookup(text), "a code span is not mention input");
        assert_eq!(
            plan(text, &directory(), &[]).expect("resolves"),
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
            ..Directory::default()
        };
        let block = plan("@Buzzx Build", &sparse, &[]).expect_err("blocks");
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
        let block = plan(&text, &many, &[]).expect_err("blocks");
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
        let planned = plan(&repeated, &many, &[]).expect("resolves");
        assert_eq!(planned.len(), MENTION_CAP);
    }

    #[test]
    fn selected_same_name_occurrences_keep_distinct_recipients() {
        let mut two = directory();
        two.members.push(OUTSIDER.to_owned());
        two.profiles
            .push((OUTSIDER.to_owned(), profile("Buzzx Build")));
        let content = "@Buzzx Build and @Buzzx Build";
        let bindings = [
            Binding {
                start: 0,
                label: "Buzzx Build".into(),
                pubkey: ALPHA.into(),
            },
            Binding {
                start: 17,
                label: "Buzzx Build".into(),
                pubkey: OUTSIDER.into(),
            },
        ];
        assert_eq!(
            plan(content, &two, &bindings).unwrap(),
            vec![ALPHA, OUTSIDER]
        );
        assert!(matches!(
            plan(content, &two, &bindings[..1]),
            Err(Block::Ambiguous { .. })
        ));
    }

    #[test]
    fn stale_or_departed_binding_cannot_notify_the_wrong_member() {
        let mut two = directory();
        two.members.push(OUTSIDER.to_owned());
        two.profiles
            .push((OUTSIDER.to_owned(), profile("Buzzx Build")));
        let binding = Binding {
            start: 0,
            label: "Buzzx Build".into(),
            pubkey: OUTSIDER.into(),
        };
        assert!(matches!(
            plan("before @Buzzx Build", &two, std::slice::from_ref(&binding)),
            Err(Block::Unknown { .. })
        ));
        two.members.pop();
        assert!(matches!(
            plan("@Buzzx Build", &two, &[binding]),
            Err(Block::NotMember { .. })
        ));
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
