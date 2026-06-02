use std::path::{Component, Path, PathBuf};

use crate::workspace::{WorkspaceEntry, WorkspaceEntryKind, WorkspaceTree};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VaultIndex {
    notes: Vec<IndexedNote>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedNote {
    pub path: PathBuf,
    pub label: String,
    pub parent_label: String,
    outgoing_links: Vec<IndexedLink>,
    normalized_label: String,
    normalized_stem: String,
    normalized_relative_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backlink {
    pub path: PathBuf,
    pub label: String,
    pub parent_label: String,
    pub line_number: usize,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexedLink {
    target: String,
    line_number: usize,
    snippet: String,
}

impl VaultIndex {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[cfg(test)]
    #[must_use]
    pub fn from_tree(root: &Path, tree: &WorkspaceTree) -> Self {
        Self::from_tree_with_reader(root, tree, |_| None)
    }

    #[must_use]
    pub fn from_tree_with_reader(
        root: &Path,
        tree: &WorkspaceTree,
        mut read_note: impl FnMut(&Path) -> Option<String>,
    ) -> Self {
        let mut notes = Vec::new();
        for entry in &tree.entries {
            collect_indexed_notes(entry, root, &mut read_note, &mut notes);
        }
        notes.sort_by(|left, right| left.path.cmp(&right.path));

        Self { notes }
    }

    #[cfg(test)]
    #[must_use]
    pub fn notes(&self) -> &[IndexedNote] {
        &self.notes
    }

    #[must_use]
    pub fn search_notes(&self, query: &str, limit: usize) -> Vec<IndexedNote> {
        let query = normalize_search_query(query);
        if query.is_empty() {
            return self.notes.iter().take(limit).cloned().collect();
        }

        let mut scored = self
            .notes
            .iter()
            .filter_map(|note| note_search_rank(note, &query).map(|rank| (rank, &note.path, note)))
            .collect::<Vec<_>>();
        scored.sort_by(|(left_rank, left_path, _), (right_rank, right_path, _)| {
            left_rank.cmp(right_rank).then(left_path.cmp(right_path))
        });

        scored
            .into_iter()
            .take(limit)
            .map(|(_, _, note)| note.clone())
            .collect()
    }

    #[must_use]
    pub fn resolve_wikilink_path(&self, raw: &str) -> Option<PathBuf> {
        let target = wikilink_target(raw)?;
        let relative = target_to_relative_markdown_path(&target)?;
        let normalized_relative = normalize_path_text(&relative);

        if let Some(note) = self
            .notes
            .iter()
            .find(|note| note.normalized_relative_path == normalized_relative)
        {
            return Some(note.path.clone());
        }

        if relative.components().count() == 1
            && let Some(target_stem) = normalized_stem(&relative)
            && let Some(note) = self
                .notes
                .iter()
                .find(|note| note.normalized_stem == target_stem)
        {
            return Some(note.path.clone());
        }

        None
    }

    #[must_use]
    pub fn backlinks_for(&self, target_path: &Path) -> Vec<Backlink> {
        let mut backlinks = Vec::new();

        for note in &self.notes {
            if note.path == target_path {
                continue;
            }

            backlinks.extend(
                note.outgoing_links
                    .iter()
                    .filter(|link| {
                        self.resolve_wikilink_path(&link.target).as_deref() == Some(target_path)
                    })
                    .map(|link| Backlink {
                        path: note.path.clone(),
                        label: note.label.clone(),
                        parent_label: note.parent_label.clone(),
                        line_number: link.line_number,
                        snippet: link.snippet.clone(),
                    }),
            );
        }

        backlinks.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.line_number.cmp(&right.line_number))
        });
        backlinks
    }
}

#[must_use]
pub fn wikilink_target(raw: &str) -> Option<String> {
    let target = raw
        .split_once('|')
        .map(|(target, _)| target)
        .unwrap_or(raw)
        .split_once('#')
        .map(|(target, _)| target)
        .unwrap_or_else(|| raw.split_once('|').map(|(target, _)| target).unwrap_or(raw))
        .trim();

    if target.is_empty() {
        return None;
    }

    Some(strip_markdown_extension(target).to_string())
}

#[must_use]
pub fn extract_wikilinks(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut remaining = text;

    while let Some(start) = remaining.find("[[") {
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find("]]") else {
            break;
        };

        if let Some(target) = wikilink_target(&after_start[..end])
            && !links.contains(&target)
        {
            links.push(target);
        }

        remaining = &after_start[end + 2..];
    }

    links.sort();
    links.dedup();
    links
}

#[must_use]
pub fn wikilink_at_column(line: &str, column: usize) -> Option<String> {
    let mut search_start = 0;
    while let Some(relative_start) = line[search_start..].find("[[") {
        let start_byte = search_start + relative_start;
        let body_start = start_byte + 2;
        let Some(relative_end) = line[body_start..].find("]]") else {
            return None;
        };
        let end_byte = body_start + relative_end + 2;
        let start_column = line[..start_byte].chars().count();
        let end_column = line[..end_byte].chars().count();

        if start_column <= column && column < end_column {
            return Some(line[body_start..end_byte - 2].trim().to_string());
        }

        search_start = end_byte;
    }

    None
}

#[must_use]
pub fn new_wikilink_path(root: &Path, active_path: Option<&Path>, raw: &str) -> Option<PathBuf> {
    let target = wikilink_target(raw)?;
    let relative = target_to_relative_markdown_path(&target)?;

    if relative.components().count() > 1 {
        return Some(root.join(relative));
    }

    active_path
        .and_then(Path::parent)
        .map(|parent| parent.join(&relative))
        .or_else(|| Some(root.join(relative)))
}

#[must_use]
pub fn wikilink_title(raw: &str) -> Option<String> {
    let target = wikilink_target(raw)?;
    target_to_relative_markdown_path(&target)?
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .filter(|title| !title.trim().is_empty())
}

fn strip_markdown_extension(target: &str) -> &str {
    target
        .strip_suffix(".markdown")
        .or_else(|| target.strip_suffix(".md"))
        .unwrap_or(target)
}

fn target_to_relative_markdown_path(target: &str) -> Option<PathBuf> {
    let path_text = target.replace('\\', "/");
    let path = PathBuf::from(path_text);
    if path.is_absolute() || !safe_relative_path(&path) {
        return None;
    }

    let mut path = path;
    if !is_markdown_path(&path) {
        path.set_extension("md");
    }
    Some(path)
}

fn safe_relative_path(path: &Path) -> bool {
    path.components().all(|component| {
        matches!(component, Component::Normal(part) if !part.to_string_lossy().is_empty())
    })
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown"))
        .unwrap_or(false)
}

fn normalized_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().trim().to_ascii_lowercase())
        .filter(|stem| !stem.is_empty())
}

fn collect_indexed_notes(
    entry: &WorkspaceEntry,
    root: &Path,
    read_note: &mut impl FnMut(&Path) -> Option<String>,
    notes: &mut Vec<IndexedNote>,
) {
    match entry.kind {
        WorkspaceEntryKind::File => {
            if is_markdown_path(&entry.path)
                && let Some(note) = indexed_note_from_entry(entry, root, read_note)
            {
                notes.push(note);
            }
        }
        WorkspaceEntryKind::Directory => {
            for child in &entry.children {
                collect_indexed_notes(child, root, read_note, notes);
            }
        }
    }
}

fn indexed_note_from_entry(
    entry: &WorkspaceEntry,
    root: &Path,
    read_note: &mut impl FnMut(&Path) -> Option<String>,
) -> Option<IndexedNote> {
    let relative = entry.path.strip_prefix(root).unwrap_or(&entry.path);
    let parent_label = entry
        .path
        .parent()
        .and_then(|parent| parent.strip_prefix(root).ok())
        .map(display_search_parent)
        .unwrap_or_default();
    let normalized_stem = normalized_stem(&entry.path)?;
    let label = entry.name.clone();

    Some(IndexedNote {
        path: entry.path.clone(),
        outgoing_links: read_note(&entry.path)
            .map(|text| extract_indexed_links(&text))
            .unwrap_or_default(),
        normalized_label: normalize_search_query(&label),
        normalized_stem,
        normalized_relative_path: normalize_path_text(relative),
        label,
        parent_label,
    })
}

fn display_search_parent(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        String::new()
    } else {
        path.to_string_lossy().to_string()
    }
}

fn normalize_path_text(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn note_search_rank(note: &IndexedNote, query: &str) -> Option<(u8, usize)> {
    if note.normalized_label.starts_with(query) {
        return Some((0, note.normalized_label.len()));
    }

    if note.normalized_stem.starts_with(query) {
        return Some((1, note.normalized_stem.len()));
    }

    if let Some(index) = note.normalized_relative_path.find(query) {
        return Some((2, index));
    }

    subsequence_search_score(&note.normalized_label, query)
        .map(|score| (3, score))
        .or_else(|| {
            subsequence_search_score(&note.normalized_relative_path, query).map(|score| (4, score))
        })
}

fn normalize_search_query(query: &str) -> String {
    query
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn subsequence_search_score(label: &str, query: &str) -> Option<usize> {
    let mut start = None;
    let mut end = 0;
    let mut label_chars = label.char_indices();

    for needle in query.chars() {
        let (index, _) = label_chars.find(|(_, candidate)| *candidate == needle)?;
        start.get_or_insert(index);
        end = index;
    }

    let start = start.unwrap_or(0);
    Some(start * 100 + end.saturating_sub(start))
}

fn extract_indexed_links(text: &str) -> Vec<IndexedLink> {
    let mut links = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        for target in extract_wikilinks(line) {
            links.push(IndexedLink {
                target,
                line_number: line_index + 1,
                snippet: backlink_snippet(line),
            });
        }
    }
    links
}

fn backlink_snippet(line: &str) -> String {
    const MAX_SNIPPET_CHARS: usize = 96;
    let trimmed = line.trim();
    let mut snippet = trimmed.chars().take(MAX_SNIPPET_CHARS).collect::<String>();
    if trimmed.chars().count() > MAX_SNIPPET_CHARS {
        snippet.push_str("...");
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{WorkspaceEntry, WorkspaceEntryKind, WorkspaceStats, WorkspaceTree};

    #[test]
    fn wikilink_target_strips_alias_heading_and_extension() {
        assert_eq!(
            wikilink_target(" Projects/Launch Plan.md#Risks | launch "),
            Some(String::from("Projects/Launch Plan"))
        );
    }

    #[test]
    fn extract_wikilinks_returns_unique_normalized_targets() {
        let links = extract_wikilinks("[[Alpha]] [[Beta|alias]] [[Alpha#Part]] [[ ]]");

        assert_eq!(links, vec![String::from("Alpha"), String::from("Beta")]);
    }

    #[test]
    fn wikilink_at_column_returns_raw_link_under_column() {
        let line = "See [[Alpha#Heading|alias]] and [[Beta]]";

        assert_eq!(
            wikilink_at_column(line, "See [[".chars().count()),
            Some(String::from("Alpha#Heading|alias"))
        );
        assert_eq!(
            wikilink_at_column(line, "See [[Alpha#Heading|alias]] and [[".chars().count()),
            Some(String::from("Beta"))
        );
    }

    #[test]
    fn wikilink_at_column_ignores_outside_and_unclosed_links() {
        assert_eq!(wikilink_at_column("See [[Alpha]]", 0), None);
        assert_eq!(wikilink_at_column("See [[Alpha", 7), None);
    }

    #[test]
    fn wikilink_at_column_handles_utf8_columns() {
        let line = "écho [[Béta]]";

        assert_eq!(wikilink_at_column(line, 7), Some(String::from("Béta")));
    }

    #[test]
    fn resolves_existing_note_by_stem_across_vault() {
        let root = PathBuf::from("/vault");
        let path = root.join("areas/projects/Launch Plan.md");
        let tree = tree_with_file(root.clone(), path.clone());
        let index = VaultIndex::from_tree(&root, &tree);

        assert_eq!(index.resolve_wikilink_path("Launch Plan"), Some(path));
    }

    #[test]
    fn new_link_path_uses_active_note_folder_for_plain_links() {
        let root = PathBuf::from("/vault");
        let active = root.join("areas/daily/today.md");

        assert_eq!(
            new_wikilink_path(&root, Some(&active), "Follow Up"),
            Some(root.join("areas/daily/Follow Up.md"))
        );
    }

    #[test]
    fn new_link_path_rejects_parent_traversal() {
        let root = PathBuf::from("/vault");

        assert_eq!(new_wikilink_path(&root, None, "../escape"), None);
    }

    #[test]
    fn vault_index_builds_nested_markdown_notes_only() {
        let root = PathBuf::from("/vault");
        let tree = tree_with_nested_entries(&root);

        let index = VaultIndex::from_tree(&root, &tree);

        let notes = index.notes();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].path, root.join("alpha.md"));
        assert_eq!(notes[0].label, "alpha.md");
        assert_eq!(notes[0].parent_label, "");
        assert_eq!(notes[1].path, root.join("archive/projects/gamma-index.md"));
        assert_eq!(notes[1].parent_label, "archive/projects");
    }

    #[test]
    fn vault_index_searches_cached_notes_by_subsequence() {
        let root = PathBuf::from("/vault");
        let tree = tree_with_nested_entries(&root);
        let index = VaultIndex::from_tree(&root, &tree);

        let results = index.search_notes("gm", 8);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].path,
            root.join("archive/projects/gamma-index.md")
        );
    }

    #[test]
    fn vault_index_resolves_links_by_relative_path_and_stem() {
        let root = PathBuf::from("/vault");
        let tree = tree_with_nested_entries(&root);
        let index = VaultIndex::from_tree(&root, &tree);

        assert_eq!(
            index.resolve_wikilink_path("archive/projects/gamma-index"),
            Some(root.join("archive/projects/gamma-index.md"))
        );
        assert_eq!(
            index.resolve_wikilink_path("gamma-index"),
            Some(root.join("archive/projects/gamma-index.md"))
        );
    }

    #[test]
    fn vault_index_tracks_incoming_backlinks() {
        let root = PathBuf::from("/vault");
        let tree = tree_with_backlink_entries(&root);
        let index = VaultIndex::from_tree_with_reader(&root, &tree, |path| {
            if path == root.join("daily.md") {
                Some(String::from(
                    "# Daily\n\nMet with [[Alpha]] about launch.\n[[Archive/Projects/Gamma Index]]",
                ))
            } else if path == root.join("archive/projects/related.md") {
                Some(String::from("[[alpha|home]]"))
            } else {
                None
            }
        });

        let backlinks = index.backlinks_for(&root.join("alpha.md"));

        assert_eq!(backlinks.len(), 2);
        assert_eq!(backlinks[0].path, root.join("archive/projects/related.md"));
        assert_eq!(backlinks[0].parent_label, "archive/projects");
        assert_eq!(backlinks[0].line_number, 1);
        assert_eq!(backlinks[0].snippet, "[[alpha|home]]");
        assert_eq!(backlinks[1].path, root.join("daily.md"));
        assert_eq!(backlinks[1].line_number, 3);
        assert_eq!(backlinks[1].snippet, "Met with [[Alpha]] about launch.");
    }

    fn tree_with_file(root: PathBuf, path: PathBuf) -> WorkspaceTree {
        WorkspaceTree {
            root,
            entries: vec![WorkspaceEntry {
                path,
                name: String::from("Launch Plan.md"),
                kind: WorkspaceEntryKind::File,
                children: Vec::new(),
            }],
            max_depth: 6,
            stats: WorkspaceStats {
                folders: 0,
                files: 1,
            },
        }
    }

    fn tree_with_nested_entries(root: &Path) -> WorkspaceTree {
        WorkspaceTree {
            root: root.to_path_buf(),
            entries: vec![
                WorkspaceEntry {
                    path: root.join("alpha.md"),
                    name: String::from("alpha.md"),
                    kind: WorkspaceEntryKind::File,
                    children: Vec::new(),
                },
                WorkspaceEntry {
                    path: root.join("archive"),
                    name: String::from("archive"),
                    kind: WorkspaceEntryKind::Directory,
                    children: vec![WorkspaceEntry {
                        path: root.join("archive/projects"),
                        name: String::from("projects"),
                        kind: WorkspaceEntryKind::Directory,
                        children: vec![
                            WorkspaceEntry {
                                path: root.join("archive/projects/gamma-index.md"),
                                name: String::from("gamma-index.md"),
                                kind: WorkspaceEntryKind::File,
                                children: Vec::new(),
                            },
                            WorkspaceEntry {
                                path: root.join("archive/projects/gamma.txt"),
                                name: String::from("gamma.txt"),
                                kind: WorkspaceEntryKind::File,
                                children: Vec::new(),
                            },
                        ],
                    }],
                },
            ],
            max_depth: 6,
            stats: WorkspaceStats {
                folders: 2,
                files: 3,
            },
        }
    }

    fn tree_with_backlink_entries(root: &Path) -> WorkspaceTree {
        WorkspaceTree {
            root: root.to_path_buf(),
            entries: vec![
                WorkspaceEntry {
                    path: root.join("alpha.md"),
                    name: String::from("alpha.md"),
                    kind: WorkspaceEntryKind::File,
                    children: Vec::new(),
                },
                WorkspaceEntry {
                    path: root.join("daily.md"),
                    name: String::from("daily.md"),
                    kind: WorkspaceEntryKind::File,
                    children: Vec::new(),
                },
                WorkspaceEntry {
                    path: root.join("archive"),
                    name: String::from("archive"),
                    kind: WorkspaceEntryKind::Directory,
                    children: vec![WorkspaceEntry {
                        path: root.join("archive/projects"),
                        name: String::from("projects"),
                        kind: WorkspaceEntryKind::Directory,
                        children: vec![
                            WorkspaceEntry {
                                path: root.join("archive/projects/Gamma Index.md"),
                                name: String::from("Gamma Index.md"),
                                kind: WorkspaceEntryKind::File,
                                children: Vec::new(),
                            },
                            WorkspaceEntry {
                                path: root.join("archive/projects/related.md"),
                                name: String::from("related.md"),
                                kind: WorkspaceEntryKind::File,
                                children: Vec::new(),
                            },
                        ],
                    }],
                },
            ],
            max_depth: 6,
            stats: WorkspaceStats {
                folders: 2,
                files: 4,
            },
        }
    }
}
