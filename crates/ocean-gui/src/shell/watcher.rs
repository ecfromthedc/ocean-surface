use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultWatchEvent {
    pub paths: Vec<PathBuf>,
}

pub struct VaultWatcher {
    _watcher: RecommendedWatcher,
}

impl VaultWatcher {
    pub fn start(root: &Path) -> notify::Result<(Self, Receiver<VaultWatchEvent>)> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |result| {
            let Ok(event) = result else {
                return;
            };

            if let Some(event) = VaultWatchEvent::from_notify(event) {
                let _ = tx.send(event);
            }
        })?;

        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok((Self { _watcher: watcher }, rx))
    }
}

impl VaultWatchEvent {
    fn from_notify(event: Event) -> Option<Self> {
        if matches!(event.kind, EventKind::Access(_)) {
            return None;
        }

        let paths = event
            .paths
            .into_iter()
            .filter(|path| relevant_path(path, event.kind))
            .collect::<Vec<_>>();

        (!paths.is_empty()).then_some(Self { paths })
    }
}

fn relevant_path(path: &Path, kind: EventKind) -> bool {
    if ignored_path(path) {
        return false;
    }

    is_markdown_path(path) || path.is_dir() || kind.is_remove()
}

fn ignored_path(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        let name = name.to_string_lossy();
        name.starts_with('.') || matches!(name.as_ref(), "target" | "node_modules")
    })
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, CreateKind, RemoveKind};

    use super::*;

    #[test]
    fn filters_access_events() {
        let event = Event::new(EventKind::Access(AccessKind::Any))
            .add_path(PathBuf::from("/vault/note.md"));

        assert_eq!(VaultWatchEvent::from_notify(event), None);
    }

    #[test]
    fn accepts_markdown_mutations() {
        let event = Event::new(EventKind::Create(CreateKind::File))
            .add_path(PathBuf::from("/vault/note.md"));

        assert_eq!(
            VaultWatchEvent::from_notify(event),
            Some(VaultWatchEvent {
                paths: vec![PathBuf::from("/vault/note.md")]
            })
        );
    }

    #[test]
    fn ignores_build_artifacts() {
        let event = Event::new(EventKind::Remove(RemoveKind::File))
            .add_path(PathBuf::from("/vault/target/debug/file"));

        assert_eq!(VaultWatchEvent::from_notify(event), None);
    }
}
