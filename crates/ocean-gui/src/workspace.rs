use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceEntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: WorkspaceEntryKind,
    pub children: Vec<WorkspaceEntry>,
}

impl WorkspaceEntry {
    fn from_path(path: PathBuf, depth: usize, max_depth: usize) -> Result<Self, WorkspaceError> {
        let metadata =
            fs::metadata(&path).map_err(|source| WorkspaceError::io(Some(path.clone()), source))?;
        let kind = if metadata.is_dir() {
            WorkspaceEntryKind::Directory
        } else {
            WorkspaceEntryKind::File
        };

        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        let mut children = Vec::new();
        if matches!(kind, WorkspaceEntryKind::Directory) && depth < max_depth {
            let mut entries = Vec::new();
            let read_dir = fs::read_dir(&path)
                .map_err(|source| WorkspaceError::io(Some(path.clone()), source))?;

            for child in read_dir {
                let child_path = match child {
                    Ok(entry) => entry.path(),
                    Err(_) => continue,
                };

                if is_hidden(&child_path) {
                    continue;
                }

                match WorkspaceEntry::from_path(child_path, depth + 1, max_depth) {
                    Ok(entry) => entries.push(entry),
                    Err(_) => continue,
                }
            }

            entries.sort_by(|left, right| left.path.cmp(&right.path));
            children = entries;
        }

        Ok(Self {
            path,
            name,
            kind,
            children,
        })
    }

    pub fn is_directory(&self) -> bool {
        matches!(self.kind, WorkspaceEntryKind::Directory)
    }

    pub fn is_file(&self) -> bool {
        matches!(self.kind, WorkspaceEntryKind::File)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceStats {
    pub folders: usize,
    pub files: usize,
}

impl WorkspaceStats {
    fn accumulate_entry(&mut self, entry: &WorkspaceEntry) {
        match entry.kind {
            WorkspaceEntryKind::Directory => {
                self.folders += 1;
                for child in &entry.children {
                    self.accumulate_entry(child);
                }
            }
            WorkspaceEntryKind::File => {
                self.files += 1;
            }
        }
    }

    fn from_entries(entries: &[WorkspaceEntry]) -> Self {
        let mut stats = Self::default();
        for entry in entries {
            stats.accumulate_entry(entry);
        }
        stats
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceTree {
    pub root: PathBuf,
    pub entries: Vec<WorkspaceEntry>,
    pub max_depth: usize,
    pub stats: WorkspaceStats,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        let root = root.into();
        ensure_directory(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn set_root(&mut self, root: impl Into<PathBuf>) -> Result<(), WorkspaceError> {
        let root = root.into();
        ensure_directory(&root)?;
        self.root = root;
        Ok(())
    }

    pub fn scan(&self, max_depth: usize) -> Result<WorkspaceTree, WorkspaceError> {
        scan_root(&self.root, max_depth)
    }

    pub fn stats(&self, max_depth: usize) -> Result<WorkspaceStats, WorkspaceError> {
        Ok(self.scan(max_depth)?.stats)
    }

    pub fn read_file(&self, path: impl AsRef<Path>) -> Result<String, WorkspaceError> {
        let path = resolve_path(self.root(), path.as_ref());
        fs::read_to_string(&path).map_err(|source| WorkspaceError::io(Some(path), source))
    }

    pub fn write_file(
        &self,
        path: impl AsRef<Path>,
        contents: impl AsRef<str>,
    ) -> Result<(), WorkspaceError> {
        let path = resolve_path(self.root(), path.as_ref());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| WorkspaceError::io(Some(parent.to_path_buf()), source))?;
        }
        fs::write(&path, contents.as_ref()).map_err(|source| WorkspaceError::io(Some(path), source))
    }

    pub fn rename_path(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
    ) -> Result<(), WorkspaceError> {
        let from = resolve_path(self.root(), from.as_ref());
        let to = resolve_path(self.root(), to.as_ref());
        fs::metadata(&from).map_err(|source| WorkspaceError::io(Some(from.clone()), source))?;
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| WorkspaceError::io(Some(parent.to_path_buf()), source))?;
        }
        fs::rename(&from, &to).map_err(|source| WorkspaceError::io(Some(from), source))
    }

    pub fn remove_file(&self, path: impl AsRef<Path>) -> Result<(), WorkspaceError> {
        let path = resolve_path(self.root(), path.as_ref());
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(WorkspaceError::NotAFile(path)),
            Err(source) => return Err(WorkspaceError::io(Some(path), source)),
        }

        fs::remove_file(&path).map_err(|source| WorkspaceError::io(Some(path), source))
    }
}

#[derive(Debug)]
pub enum WorkspaceError {
    NotADirectory(PathBuf),
    NotAFile(PathBuf),
    Io {
        path: Option<PathBuf>,
        source: std::io::Error,
    },
}

impl WorkspaceError {
    fn io(path: Option<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path, source }
    }
}

impl Display for WorkspaceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceError::NotADirectory(path) => {
                write!(f, "{} is not a directory", path.display())
            }
            WorkspaceError::NotAFile(path) => {
                write!(f, "{} is not a file", path.display())
            }
            WorkspaceError::Io { path, source } => {
                if let Some(path) = path {
                    write!(f, "{}: {}", path.display(), source)
                } else {
                    write!(f, "{}", source)
                }
            }
        }
    }
}

impl Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            WorkspaceError::Io { source, .. } => Some(source),
            WorkspaceError::NotADirectory(_) | WorkspaceError::NotAFile(_) => None,
        }
    }
}

pub fn scan_root(
    root: impl AsRef<Path>,
    max_depth: usize,
) -> Result<WorkspaceTree, WorkspaceError> {
    let root = root.as_ref();
    ensure_directory(root)?;

    let mut entries = Vec::new();
    let read_dir = fs::read_dir(root)
        .map_err(|source| WorkspaceError::io(Some(root.to_path_buf()), source))?;

    for child in read_dir {
        let child_path = match child {
            Ok(entry) => entry.path(),
            Err(_) => continue,
        };

        if is_hidden(&child_path) {
            continue;
        }

        match WorkspaceEntry::from_path(child_path, 0, max_depth) {
            Ok(entry) => entries.push(entry),
            Err(_) => continue,
        }
    }

    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let stats = WorkspaceStats::from_entries(&entries);

    Ok(WorkspaceTree {
        root: root.to_path_buf(),
        entries,
        max_depth,
        stats,
    })
}

fn ensure_directory(path: &Path) -> Result<(), WorkspaceError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(WorkspaceError::NotADirectory(path.to_path_buf())),
        Err(source) => Err(WorkspaceError::io(Some(path.to_path_buf()), source)),
    }
}

fn resolve_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let id = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ocean_gui-workspace-test-{}-{}-{}",
            std::process::id(),
            id,
            stamp
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn scan_builds_tree_and_stats() {
        let root = temp_dir();
        fs::create_dir_all(root.join("notes")).expect("dir");
        fs::write(root.join("notes").join("todo.md"), "hello").expect("file");
        fs::write(root.join("draft.md"), "draft").expect("file");

        let tree = scan_root(&root, 8).expect("scan");
        assert_eq!(tree.root, root);
        assert_eq!(tree.stats.files, 2);
        assert_eq!(tree.stats.folders, 1);
        assert_eq!(tree.entries.len(), 2);
    }

    #[test]
    fn read_and_write_text_files() {
        let root = temp_dir();
        let workspace = Workspace::new(&root).expect("workspace");

        workspace
            .write_file("notes/hello.md", "# Hello")
            .expect("write");

        let text = workspace.read_file("notes/hello.md").expect("read");
        assert_eq!(text, "# Hello");
    }

    #[test]
    fn rename_path_moves_files_and_creates_parent_dirs() {
        let root = temp_dir();
        let workspace = Workspace::new(&root).expect("workspace");
        workspace.write_file("draft.md", "# Draft").expect("write");

        workspace
            .rename_path(root.join("draft.md"), root.join("notes/final.md"))
            .expect("rename");

        assert!(!root.join("draft.md").exists());
        assert_eq!(
            fs::read_to_string(root.join("notes/final.md")).expect("renamed file"),
            "# Draft"
        );
    }

    #[test]
    fn remove_file_deletes_files_but_rejects_directories() {
        let root = temp_dir();
        let workspace = Workspace::new(&root).expect("workspace");
        workspace.write_file("draft.md", "# Draft").expect("write");
        fs::create_dir_all(root.join("folder")).expect("folder");

        workspace
            .remove_file(root.join("draft.md"))
            .expect("delete");
        assert!(!root.join("draft.md").exists());

        let error = workspace
            .remove_file(root.join("folder"))
            .expect_err("directory deletion should be rejected");
        assert!(matches!(error, WorkspaceError::NotAFile(_)));
    }

    #[test]
    fn set_root_rejects_files() {
        let root = temp_dir();
        let file = root.join("not-a-folder.txt");
        fs::write(&file, "content").expect("file");

        let mut workspace = Workspace::new(&root).expect("workspace");
        let err = workspace.set_root(&file).expect_err("should reject file");
        assert!(matches!(err, WorkspaceError::NotADirectory(_)));
    }
}
