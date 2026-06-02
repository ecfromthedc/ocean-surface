#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellIcon {
    Vault,
    Files,
    Editor,
    Inspector,
    Report,
}

impl ShellIcon {
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            ShellIcon::Vault => "icons/ocean-gui/book.svg",
            ShellIcon::Files => "icons/ocean-gui/folder_open.svg",
            ShellIcon::Editor => "icons/ocean-gui/file_markdown.svg",
            ShellIcon::Inspector => "icons/ocean-gui/file_doc.svg",
            ShellIcon::Report => "icons/ocean-gui/file_doc.svg",
        }
    }
}
