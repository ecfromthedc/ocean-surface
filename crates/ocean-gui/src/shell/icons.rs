#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellIcon {
    Vault,
    Files,
    Editor,
    Inspector,
    Report,
    Blocks,
    Chat,
    Check,
    Code,
    Diff,
    FileText,
    Search,
    Server,
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
            ShellIcon::Blocks => "icons/ocean-gui/blocks.svg",
            ShellIcon::Chat => "icons/ocean-gui/chat.svg",
            ShellIcon::Check => "icons/ocean-gui/check_circle.svg",
            ShellIcon::Code => "icons/ocean-gui/code.svg",
            ShellIcon::Diff => "icons/ocean-gui/diff.svg",
            ShellIcon::FileText => "icons/ocean-gui/file_text.svg",
            ShellIcon::Search => "icons/ocean-gui/tool_search.svg",
            ShellIcon::Server => "icons/ocean-gui/server.svg",
        }
    }
}
