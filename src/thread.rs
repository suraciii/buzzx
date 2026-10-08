//! Pure thread identity and branch helpers shared by the TUI renderer and state.
//!
//! A thread keeps one outer root, while a branch head selects the direct
//! children visible in the current drill-down. Event identity, not row index,
//! is the stable cursor across live merges and reloads.

use std::collections::{HashMap, HashSet};

use crate::content::Row;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSummary {
    pub child_id: String,
    pub replies: usize,
    pub last_at: u64,
    pub participants: Vec<String>,
}

/// The root plus the current head and its direct children. The current head is
/// included as context when a nested branch is open.
pub fn visible_indices(rows: &[Row], root: &str, head: &str) -> Vec<usize> {
    let mut result = Vec::new();
    if let Some(index) = rows.iter().position(|row| row.event_id == root) {
        result.push(index);
    }
    if head != root
        && let Some(index) = rows.iter().position(|row| row.event_id == head)
        && !result.contains(&index)
    {
        result.push(index);
    }
    for (index, row) in rows.iter().enumerate() {
        if row.parent_id.as_deref() == Some(head) && !result.contains(&index) {
            result.push(index);
        }
    }
    result.sort_unstable();
    result
}

/// Return the direct child rows below a head in timeline order.
pub fn direct_child_indices(rows: &[Row], head: &str) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| (row.parent_id.as_deref() == Some(head)).then_some(index))
        .collect()
}

/// Build the summary for a direct child. Descendants are followed by parent
/// identity, so a same-second event or a reordered response cannot change the
/// branch count.
pub fn summary(rows: &[Row], child_id: &str) -> Option<BranchSummary> {
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        if let Some(parent) = &row.parent_id {
            children
                .entry(parent.clone())
                .or_default()
                .push(row.event_id.clone());
        }
    }
    let mut pending = children.get(child_id)?.clone();
    if pending.is_empty() {
        return None;
    }
    let mut seen = HashSet::new();
    let mut last_at = 0;
    let mut participants = Vec::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(row) = rows.iter().find(|row| row.event_id == id) {
            last_at = last_at.max(row.created_at);
            if !participants.contains(&row.author) {
                participants.push(row.author.clone());
            }
        }
        if let Some(next) = children.get(&id) {
            pending.extend(next.iter().cloned());
        }
    }
    participants.sort();
    Some(BranchSummary {
        child_id: child_id.to_owned(),
        replies: seen.len(),
        last_at,
        participants,
    })
}

/// The ancestor path from the root to a target reply, excluding the target.
/// A malformed cycle or a missing parent returns no path rather than guessing
/// a branch and silently focusing a different row.
pub fn ancestor_path(rows: &[Row], root: &str, target: &str) -> Option<Vec<String>> {
    if target == root {
        return Some(Vec::new());
    }
    let by_id: HashMap<&str, &Row> = rows
        .iter()
        .map(|row| (row.event_id.as_str(), row))
        .collect();
    let mut path = Vec::new();
    let mut current = target;
    let mut seen = HashSet::new();
    loop {
        let row = by_id.get(current)?;
        let parent = row.parent_id.as_deref()?;
        if !seen.insert(parent) {
            return None;
        }
        if parent == root {
            path.reverse();
            return Some(path);
        }
        path.push(parent.to_owned());
        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, parent: Option<&str>, at: u64) -> Row {
        Row {
            event_id: id.to_owned(),
            pubkey: "a".to_owned(),
            author: id.to_owned(),
            created_at: at,
            body: id.to_owned(),
            kind: 9,
            root_id: Some("root".to_owned()),
            parent_id: parent.map(str::to_owned),
            broadcast: false,
            mentions_me: false,
            reactions: Vec::new(),
            attachment: None,
            pending: false,
            uncertain: false,
            edited: false,
        }
    }

    #[test]
    fn visible_rows_keep_root_head_and_direct_children() {
        let rows = vec![
            row("root", None, 1),
            row("a", Some("root"), 2),
            row("b", Some("a"), 3),
            row("c", Some("root"), 4),
        ];
        let ids: Vec<_> = visible_indices(&rows, "root", "a")
            .into_iter()
            .map(|index| rows[index].event_id.as_str())
            .collect();
        assert_eq!(ids, vec!["root", "a", "b"]);
    }

    #[test]
    fn summary_counts_all_descendants_and_participants() {
        let rows = vec![
            row("root", None, 1),
            row("a", Some("root"), 2),
            row("b", Some("a"), 3),
            row("c", Some("b"), 7),
        ];
        let summary = summary(&rows, "a").expect("branch");
        assert_eq!(summary.replies, 2);
        assert_eq!(summary.last_at, 7);
        assert_eq!(summary.participants, vec!["b", "c"]);
    }

    #[test]
    fn ancestor_path_rejects_cycles_and_excludes_target() {
        let rows = vec![
            row("root", None, 1),
            row("a", Some("root"), 2),
            row("b", Some("a"), 3),
        ];
        assert_eq!(
            ancestor_path(&rows, "root", "b"),
            Some(vec!["a".to_owned()])
        );
        let cyclic = vec![
            row("root", None, 1),
            row("a", Some("b"), 2),
            row("b", Some("a"), 3),
        ];
        assert_eq!(ancestor_path(&cyclic, "root", "b"), None);
    }
}
