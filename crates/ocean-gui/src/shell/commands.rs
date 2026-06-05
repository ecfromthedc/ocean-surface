#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellCommand {
    OpenVault,
    NewNote,
    RenameNote,
    DeleteNote,
    RevealNote,
    RefreshVault,
    EditExternal,
    ReloadNote,
    SaveNote,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub kind: ShellCommand,
    pub label: &'static str,
    pub shortcut: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        kind: ShellCommand::OpenVault,
        label: "Open vault",
        shortcut: "cmd o",
    },
    CommandSpec {
        kind: ShellCommand::NewNote,
        label: "New note",
        shortcut: "cmd n",
    },
    CommandSpec {
        kind: ShellCommand::RenameNote,
        label: "Rename note",
        shortcut: "",
    },
    CommandSpec {
        kind: ShellCommand::DeleteNote,
        label: "Delete note",
        shortcut: "cmd del",
    },
    CommandSpec {
        kind: ShellCommand::RevealNote,
        label: "Reveal note",
        shortcut: "",
    },
    CommandSpec {
        kind: ShellCommand::RefreshVault,
        label: "Refresh vault",
        shortcut: "cmd shift r",
    },
    CommandSpec {
        kind: ShellCommand::EditExternal,
        label: "Edit external",
        shortcut: "",
    },
    CommandSpec {
        kind: ShellCommand::ReloadNote,
        label: "Reload note",
        shortcut: "cmd r",
    },
    CommandSpec {
        kind: ShellCommand::SaveNote,
        label: "Save note",
        shortcut: "cmd s",
    },
];

pub fn filtered_commands(query: &str) -> Vec<CommandSpec> {
    let query = normalize_query(query);
    if query.is_empty() {
        return COMMANDS.to_vec();
    }

    let mut scored = COMMANDS
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(index, command)| {
            command_rank(command.label, &query).map(|rank| (rank, index, command))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|(rank, index, _)| (*rank, *index));
    scored.into_iter().map(|(_, _, command)| command).collect()
}

fn normalize_query(query: &str) -> String {
    query
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn command_rank(label: &str, query: &str) -> Option<(u8, usize)> {
    let compact_label = normalize_query(label);
    if compact_label.starts_with(query) {
        return Some((0, 0));
    }

    let acronym = label
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if acronym.starts_with(query) {
        return Some((1, 0));
    }

    compact_label
        .find(query)
        .map(|index| (2, index))
        .or_else(|| {
            subsequence_score(&compact_label, query).map(|(start, span)| (3, start * 100 + span))
        })
}

fn subsequence_score(label: &str, query: &str) -> Option<(usize, usize)> {
    let mut start = None;
    let mut end = 0;
    let mut label_chars = label.char_indices();

    for needle in query.chars() {
        let Some((index, _)) = label_chars.find(|(_, candidate)| *candidate == needle) else {
            return None;
        };
        start.get_or_insert(index);
        end = index;
    }

    let start = start.unwrap_or(0);
    Some((start, end.saturating_sub(start)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_all_commands() {
        assert_eq!(filtered_commands("").len(), COMMANDS.len());
    }

    #[test]
    fn query_filters_by_subsequence() {
        let commands = filtered_commands("rn");

        assert_eq!(commands[0].kind, ShellCommand::RenameNote);
    }

    #[test]
    fn query_ignores_whitespace_and_case() {
        let commands = filtered_commands("  SV  ");

        assert_eq!(commands[0].kind, ShellCommand::SaveNote);
    }
}
