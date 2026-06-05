use eframe::egui;
use ocean_gui::{Workspace, WorkspaceEntry, WorkspaceStats, WorkspaceTree};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Space {
    Vault,
    Dev,
    Mesh,
    Ops,
    Data,
}

impl Space {
    const ALL: [Self; 5] = [Self::Vault, Self::Dev, Self::Mesh, Self::Ops, Self::Data];

    fn label(self) -> &'static str {
        match self {
            Self::Vault => "Vault",
            Self::Dev => "Dev",
            Self::Mesh => "Mesh",
            Self::Ops => "Ops",
            Self::Data => "Data",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Vault => "Markdown, files, specs, and durable notes",
            Self::Dev => "Terminals, git, worktrees, code sessions",
            Self::Mesh => "Agents, sessions, models, skills, tickets, routines",
            Self::Ops => "Services, deploys, logs, health, approvals",
            Self::Data => "Databases, analytics, campaigns, saved reports",
        }
    }
}

#[derive(Clone, Debug)]
struct Source {
    id: &'static str,
    space: Space,
    label: &'static str,
    detail: &'static str,
    status: &'static str,
}

#[derive(Clone, Debug)]
struct Leaf {
    id: &'static str,
    space: Space,
    source_id: &'static str,
    entity_id: Option<&'static str>,
    title: &'static str,
    badge: &'static str,
    detail: &'static str,
}

#[derive(Clone, Debug)]
struct Entity {
    id: &'static str,
    source_id: &'static str,
    kind: &'static str,
    title: &'static str,
    status: &'static str,
    owner: &'static str,
    updated: &'static str,
    summary: &'static str,
    actions: &'static [&'static str],
    links: &'static [&'static str],
}

#[derive(Clone, Debug)]
struct Event {
    actor: &'static str,
    action: &'static str,
    target: &'static str,
    outcome: &'static str,
    at: &'static str,
}

pub struct OceanGuiApp {
    workspace: Workspace,
    tree: WorkspaceTree,
    selected_path: Option<PathBuf>,
    editor_text: String,
    dirty: bool,
    status: String,
    max_scan_depth: usize,
    active_space: Space,
    selected_source_id: &'static str,
    selected_entity_id: &'static str,
    active_leaf_id: &'static str,
    command_text: String,
    style_configured: bool,
}

impl OceanGuiApp {
    pub fn new() -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let max_scan_depth = 6;
        let workspace = Workspace::new(root).unwrap_or_else(|_| {
            Workspace::new(PathBuf::from(".")).expect("fallback workspace should exist")
        });
        let tree = workspace
            .scan(max_scan_depth)
            .unwrap_or_else(|_| WorkspaceTree {
                root: workspace.root().to_path_buf(),
                entries: Vec::new(),
                max_depth: max_scan_depth,
                stats: WorkspaceStats::default(),
            });

        Self {
            workspace,
            tree,
            selected_path: None,
            editor_text: String::new(),
            dirty: false,
            status: String::from("Ready"),
            max_scan_depth,
            active_space: Space::Mesh,
            selected_source_id: "mesh.sessions",
            selected_entity_id: "session-henry",
            active_leaf_id: "leaf-chat",
            command_text: String::new(),
            style_configured: false,
        }
    }

    fn apply_thoth_style(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgb(20, 18, 16);
        visuals.window_fill = egui::Color32::from_rgb(24, 22, 19);
        visuals.extreme_bg_color = egui::Color32::from_rgb(10, 10, 12);
        visuals.faint_bg_color = egui::Color32::from_rgb(31, 29, 25);
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(20, 18, 16);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(30, 28, 24);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(42, 39, 34);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(50, 159, 63);
        visuals.widgets.open.bg_fill = egui::Color32::from_rgb(34, 31, 27);
        visuals.selection.bg_fill = egui::Color32::from_rgb(50, 159, 63);
        visuals.hyperlink_color = egui::Color32::from_rgb(92, 181, 104);
        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.indent = 14.0;
        ctx.set_style(style);
    }

    fn refresh_workspace(&mut self) {
        self.tree = self
            .workspace
            .scan(self.max_scan_depth)
            .unwrap_or_else(|_| WorkspaceTree {
                root: self.workspace.root().to_path_buf(),
                entries: Vec::new(),
                max_depth: self.max_scan_depth,
                stats: WorkspaceStats::default(),
            });
        self.status = format!("Loaded workspace: {}", self.workspace.root().display());
    }

    fn set_workspace(&mut self, root: PathBuf) {
        if self.workspace.set_root(root).is_ok() {
            self.selected_path = None;
            self.editor_text.clear();
            self.dirty = false;
            self.refresh_workspace();
        }
    }

    fn open_selected(&mut self, path: &Path) {
        if path.is_dir() {
            self.selected_path = Some(path.to_path_buf());
            self.editor_text.clear();
            self.dirty = false;
            self.status = format!("Folder selected: {}", path.display());
            return;
        }

        match self.workspace.read_file(path) {
            Ok(content) => {
                self.selected_path = Some(path.to_path_buf());
                self.editor_text = content;
                self.dirty = false;
                self.status = format!("Opened file: {}", path.display());
            }
            Err(err) => {
                self.status = format!("Unable to open {}: {}", path.display(), err);
            }
        }
    }

    fn save_selected(&mut self) {
        let Some(path) = self.selected_path.clone() else {
            self.status = String::from("No file selected");
            return;
        };

        if path.is_dir() {
            self.status = String::from("Selected item is a folder");
            return;
        }

        match self.workspace.write_file(&path, &self.editor_text) {
            Ok(_) => {
                self.dirty = false;
                self.status = format!("Saved {}", path.display());
            }
            Err(err) => {
                self.status = format!("Failed to save {}: {}", path.display(), err);
            }
        }
    }

    fn workspace_stats(&self) -> (usize, usize) {
        (self.tree.stats.files, self.tree.stats.folders)
    }

    fn render_tree(&mut self, ui: &mut egui::Ui, nodes: &[WorkspaceEntry]) {
        for node in nodes {
            if node.is_directory() {
                egui::CollapsingHeader::new(&node.name)
                    .default_open(false)
                    .show(ui, |ui| {
                        let response = ui.selectable_label(
                            self.selected_path.as_deref() == Some(node.path.as_path()),
                            "Open folder",
                        );
                        if response.clicked() {
                            self.open_selected(&node.path);
                            self.selected_source_id = "vault.files";
                            self.selected_entity_id = "";
                            self.active_space = Space::Vault;
                            self.active_leaf_id = "leaf-files";
                        }
                        self.render_tree(ui, &node.children);
                    });
            } else {
                let selected = self.selected_path.as_deref() == Some(node.path.as_path());
                if ui.selectable_label(selected, &node.name).clicked() {
                    self.open_selected(&node.path);
                    self.selected_source_id = "vault.files";
                    self.selected_entity_id = "";
                    self.active_space = Space::Vault;
                    self.active_leaf_id = "leaf-files";
                }
            }
        }
    }

    fn select_space(&mut self, space: Space) {
        self.active_space = space;
        if let Some(leaf) = leaves().into_iter().find(|leaf| leaf.space == space) {
            self.select_leaf(leaf.id);
        } else if let Some(source) = sources().iter().find(|source| source.space == space) {
            self.select_source(source.id);
        }
    }

    fn select_source(&mut self, source_id: &'static str) {
        self.selected_source_id = source_id;
        if let Some(entity) = entities()
            .iter()
            .find(|entity| entity.source_id == source_id)
        {
            self.selected_entity_id = entity.id;
        } else {
            self.selected_entity_id = "";
        }

        if let Some(leaf) = leaves()
            .into_iter()
            .find(|leaf| leaf.source_id == source_id)
        {
            self.active_leaf_id = leaf.id;
        }
    }

    fn select_leaf(&mut self, leaf_id: &'static str) {
        let Some(leaf) = leaves().into_iter().find(|leaf| leaf.id == leaf_id) else {
            return;
        };

        self.active_leaf_id = leaf.id;
        self.active_space = leaf.space;
        self.selected_source_id = leaf.source_id;
        self.selected_entity_id = leaf.entity_id.unwrap_or("");
    }

    fn selected_leaf(&self) -> Option<Leaf> {
        leaves()
            .into_iter()
            .find(|leaf| leaf.id == self.active_leaf_id)
    }

    fn selected_source(&self) -> Option<Source> {
        sources()
            .into_iter()
            .find(|source| source.id == self.selected_source_id)
    }

    fn selected_entity(&self) -> Option<Entity> {
        entities().into_iter().find(|entity| {
            entity.id == self.selected_entity_id && entity.source_id == self.selected_source_id
        })
    }

    fn source_entities(&self) -> Vec<Entity> {
        entities()
            .into_iter()
            .filter(|entity| entity.source_id == self.selected_source_id)
            .collect()
    }

    fn run_demo_command(&mut self) {
        let command = self.command_text.trim();
        if command.is_empty() {
            return;
        }

        self.status = format!("Queued command: {}", command);
        self.command_text.clear();
    }

    fn render_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Ocean GUI");
                ui.separator();
                ui.label(self.active_space.label());
                ui.separator();
                ui.monospace(self.workspace.root().display().to_string());

                ui.separator();
                let command = egui::TextEdit::singleline(&mut self.command_text)
                    .hint_text("Command: ask Brick, open CAMP-33, show stale agents...");
                let response = ui.add_sized([420.0, 24.0], command);
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    self.run_demo_command();
                }
                if ui.button("Run").clicked() {
                    self.run_demo_command();
                }

                if ui.button("Open workspace").clicked() {
                    if let Some(folder) = rfd::FileDialog::new()
                        .set_directory(self.workspace.root())
                        .pick_folder()
                    {
                        self.set_workspace(folder);
                    }
                }

                if ui.button("Refresh").clicked() {
                    self.refresh_workspace();
                }

                if ui.button("Save").clicked() {
                    self.save_selected();
                }
            });
        });
    }

    fn render_left_rail(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("left_rail")
            .resizable(true)
            .default_width(310.0)
            .show(ctx, |ui| {
                ui.heading("Spaces");
                ui.add_space(4.0);
                for space in Space::ALL {
                    if ui
                        .selectable_label(self.active_space == space, space.label())
                        .on_hover_text(space.description())
                        .clicked()
                    {
                        self.select_space(space);
                    }
                }

                ui.separator();
                ui.heading("Sources");
                let current_sources: Vec<Source> = sources()
                    .into_iter()
                    .filter(|source| source.space == self.active_space)
                    .collect();
                for source in current_sources {
                    let selected = self.selected_source_id == source.id;
                    if ui
                        .selectable_label(selected, source.label)
                        .on_hover_text(source.detail)
                        .clicked()
                    {
                        self.select_source(source.id);
                    }
                    if selected {
                        ui.small(format!("{} · {}", source.status, source.detail));
                    }
                }

                if self.active_space == Space::Vault {
                    ui.separator();
                    ui.heading("Files");
                    ui.small("Local markdown and project files stay first-class.");
                    let tree = self.tree.entries.clone();
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .show(ui, |ui| self.render_tree(ui, &tree));
                }
            });
    }

    fn render_inspector(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("inspector")
            .resizable(true)
            .default_width(330.0)
            .show(ctx, |ui| {
                ui.heading("Inspector");
                ui.separator();

                if let Some(entity) = self.selected_entity() {
                    ui.label(format!("{} · {}", entity.kind, entity.id));
                    ui.heading(entity.title);
                    ui.label(entity.summary);
                    ui.separator();
                    ui.label(format!("Status: {}", entity.status));
                    ui.label(format!("Owner: {}", entity.owner));
                    ui.label(format!("Updated: {}", entity.updated));

                    ui.separator();
                    ui.heading("Actions");
                    for action in entity.actions {
                        if ui.button(*action).clicked() {
                            self.status = format!("Action queued: {} -> {}", action, entity.id);
                        }
                    }

                    ui.separator();
                    ui.heading("Links");
                    for link in entity.links {
                        ui.monospace(*link);
                    }
                } else if self.selected_source_id == "vault.files" {
                    ui.label("Local file");
                    if let Some(path) = self.selected_path.as_ref() {
                        ui.heading(path.file_name().unwrap_or_default().to_string_lossy());
                        ui.monospace(path.display().to_string());
                    }
                }

                let (files, dirs) = self.workspace_stats();
                ui.separator();
                ui.heading("Workspace");
                ui.label(format!("Folders: {}", dirs));
                ui.label(format!("Files: {}", files));
                ui.label(format!("Dirty: {}", if self.dirty { "yes" } else { "no" }));
            });
    }

    fn render_center(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_workbench_header(ui);
            ui.separator();

            ui.horizontal(|ui| {
                self.render_leaf_stack(ui);
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    self.render_leaf_surface(ui);
                });
            });
        });
    }

    fn render_workbench_header(&mut self, ui: &mut egui::Ui) {
        let source = self.selected_source();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(self.active_space.label());
                ui.label(self.active_space.description());
            });
            ui.separator();
            if let Some(source) = source.as_ref() {
                ui.vertical(|ui| {
                    ui.label(source.label);
                    ui.small(source.detail);
                });
            }
        });
    }

    fn render_leaf_stack(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.set_width(128.0);
            ui.heading("Leaves");
            ui.small("Stacked tabs");
            ui.add_space(4.0);

            for leaf in leaves() {
                let selected = self.active_leaf_id == leaf.id;
                let label = format!("{}  {}", leaf.badge, leaf.title);
                let response = ui
                    .add_sized([120.0, 32.0], egui::SelectableLabel::new(selected, label))
                    .on_hover_text(leaf.detail);

                if response.clicked() {
                    self.select_leaf(leaf.id);
                }

                if selected {
                    ui.small(leaf.detail);
                    ui.add_space(4.0);
                }
            }
        });
    }

    fn render_leaf_surface(&mut self, ui: &mut egui::Ui) {
        if let Some(leaf) = self.selected_leaf() {
            ui.horizontal(|ui| {
                ui.monospace(leaf.badge);
                ui.heading(leaf.title);
                ui.separator();
                ui.label(leaf.detail);
            });
        }

        ui.separator();

        if self.selected_source_id == "vault.files" {
            self.render_file_editor(ui);
            return;
        }

        ui.columns(2, |columns| {
            columns[0].heading("Entities");
            columns[0].small("Every source becomes searchable objects with common actions.");
            columns[0].separator();

            for entity in self.source_entities() {
                let selected = self.selected_entity_id == entity.id;
                let label = format!("{}  {}", entity.id, entity.title);
                if columns[0].selectable_label(selected, label).clicked() {
                    self.selected_entity_id = entity.id;
                }
                columns[0].small(format!(
                    "{} · {} · {}",
                    entity.kind, entity.status, entity.owner
                ));
                columns[0].add_space(6.0);
            }

            columns[1].heading("Work Surface");
            columns[1].small(
                "The selected object decides whether this is an editor, terminal, ticket, dashboard, or analysis view.",
            );
            columns[1].separator();

            if let Some(entity) = self.selected_entity() {
                columns[1].heading(entity.title);
                columns[1].label(entity.summary);
                columns[1].add_space(10.0);

                egui::Grid::new("entity_grid")
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(&mut columns[1], |ui| {
                        ui.label("Kind");
                        ui.monospace(entity.kind);
                        ui.end_row();
                        ui.label("Status");
                        ui.monospace(entity.status);
                        ui.end_row();
                        ui.label("Owner");
                        ui.monospace(entity.owner);
                        ui.end_row();
                        ui.label("Source");
                        ui.monospace(entity.source_id);
                        ui.end_row();
                    });

                columns[1].separator();
                columns[1].label("Canonical flow");
                columns[1].monospace(format!(
                    "{} -> {} -> action -> event -> note",
                    entity.source_id, entity.kind
                ));
            }
        });
    }

    fn render_file_editor(&mut self, ui: &mut egui::Ui) {
        let path = self.selected_path.clone();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                if let Some(path) = path.as_ref() {
                    ui.heading(path.file_name().unwrap_or_default().to_string_lossy());
                    ui.monospace(path.display().to_string());
                }
            });
        });
        ui.separator();

        match path.as_ref() {
            Some(path) if path.is_dir() => {
                ui.label("Folder selected");
            }
            Some(_) => {
                let editor = egui::TextEdit::multiline(&mut self.editor_text)
                    .desired_rows(24)
                    .lock_focus(true)
                    .code_editor();
                let response = ui.add(editor);
                if response.changed() {
                    self.dirty = true;
                }
            }
            None => {
                ui.label("Select a file from the Vault source.");
            }
        }
    }

    fn render_event_log(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("event_log")
            .resizable(true)
            .default_height(150.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Events");
                    ui.separator();
                    ui.label(&self.status);
                    if self.dirty {
                        ui.separator();
                        ui.label("Unsaved changes");
                    }
                });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for event in events() {
                        ui.horizontal(|ui| {
                            ui.monospace(event.at);
                            ui.label(event.actor);
                            ui.monospace(event.action);
                            ui.label(event.target);
                            ui.label(event.outcome);
                        });
                    }
                });
            });
    }
}

impl eframe::App for OceanGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.style_configured {
            Self::apply_thoth_style(ctx);
            self.style_configured = true;
        }

        self.render_top_bar(ctx);
        self.render_left_rail(ctx);
        self.render_inspector(ctx);
        self.render_event_log(ctx);
        self.render_center(ctx);
    }
}

fn leaves() -> Vec<Leaf> {
    vec![
        Leaf {
            id: "leaf-files",
            space: Space::Vault,
            source_id: "vault.files",
            entity_id: None,
            title: "Files",
            badge: "FILE",
            detail: "Vault tree, markdown source, local file edits",
        },
        Leaf {
            id: "leaf-chat",
            space: Space::Mesh,
            source_id: "mesh.sessions",
            entity_id: Some("session-henry"),
            title: "Vault Chat",
            badge: "CHAT",
            detail: "Agent transcript and Tides Mesh turns",
        },
        Leaf {
            id: "leaf-terminal",
            space: Space::Dev,
            source_id: "dev.terminals",
            entity_id: Some("term-shell-1"),
            title: "Terminal: zsh",
            badge: "TERM",
            detail: "Future native PTY surface for workflow execution",
        },
        Leaf {
            id: "leaf-model",
            space: Space::Mesh,
            source_id: "mesh.models",
            entity_id: Some("model-gpt55"),
            title: "Model",
            badge: "MODL",
            detail: "Provider, model, and thinking-level routing",
        },
        Leaf {
            id: "leaf-tickets",
            space: Space::Mesh,
            source_id: "mesh.tickets",
            entity_id: Some("THO-17"),
            title: "Tickets",
            badge: "TICK",
            detail: "Markdown-native issues and task state",
        },
        Leaf {
            id: "leaf-skills",
            space: Space::Mesh,
            source_id: "mesh.skills",
            entity_id: Some("skill-cloudflare"),
            title: "Skills",
            badge: "SKIL",
            detail: "Capabilities, MCP servers, and action inventory",
        },
        Leaf {
            id: "leaf-crons",
            space: Space::Mesh,
            source_id: "mesh.routines",
            entity_id: Some("routine-vaultkeeper"),
            title: "Crons",
            badge: "CRON",
            detail: "Routines, daemon health, and run logs",
        },
        Leaf {
            id: "leaf-living",
            space: Space::Data,
            source_id: "data.campaigns",
            entity_id: Some("report-content"),
            title: "Living",
            badge: "LIVE",
            detail: "Editorial dashboard over company signals",
        },
        Leaf {
            id: "leaf-ops",
            space: Space::Ops,
            source_id: "ops.services",
            entity_id: Some("svc-pi-cockpit"),
            title: "Services",
            badge: "OPS",
            detail: "Service health, logs, deploys, approvals",
        },
    ]
}

fn sources() -> Vec<Source> {
    vec![
        Source {
            id: "vault.files",
            space: Space::Vault,
            label: "Local Vault",
            detail: "Markdown, specs, files, frontmatter",
            status: "live",
        },
        Source {
            id: "vault.notes",
            space: Space::Vault,
            label: "Notes Index",
            detail: "Daily notes, briefs, SOPs, scratchpads",
            status: "indexed",
        },
        Source {
            id: "dev.git",
            space: Space::Dev,
            label: "Git + Worktrees",
            detail: "Repos, branches, PR context, dirty state",
            status: "connected",
        },
        Source {
            id: "dev.terminals",
            space: Space::Dev,
            label: "Terminals",
            detail: "PTY sessions, command runs, logs",
            status: "planned",
        },
        Source {
            id: "mesh.sessions",
            space: Space::Mesh,
            label: "Agent Sessions",
            detail: "Tides Mesh and ACP-compatible sessions",
            status: "live model",
        },
        Source {
            id: "mesh.models",
            space: Space::Mesh,
            label: "Models",
            detail: "Provider, model, thinking level, routing",
            status: "ready",
        },
        Source {
            id: "mesh.skills",
            space: Space::Mesh,
            label: "Skills + MCP",
            detail: "Callable capabilities and tool servers",
            status: "indexed",
        },
        Source {
            id: "mesh.routines",
            space: Space::Mesh,
            label: "Routines / Crons",
            detail: "Recurring agent jobs and run logs",
            status: "scheduler",
        },
        Source {
            id: "mesh.tickets",
            space: Space::Mesh,
            label: "Tickets",
            detail: "Markdown-backed issues and task state",
            status: "vault-native",
        },
        Source {
            id: "ops.services",
            space: Space::Ops,
            label: "Services",
            detail: "Railway, Cloudflare, launchd, daemons",
            status: "observed",
        },
        Source {
            id: "ops.approvals",
            space: Space::Ops,
            label: "Approvals",
            detail: "Deploys, publishing, data mutation gates",
            status: "required",
        },
        Source {
            id: "data.databases",
            space: Space::Data,
            label: "Databases",
            detail: "Postgres, Supabase, analytics stores",
            status: "connectors",
        },
        Source {
            id: "data.campaigns",
            space: Space::Data,
            label: "Campaign Analytics",
            detail: "Content, finance, campaign performance",
            status: "reports",
        },
    ]
}

fn entities() -> Vec<Entity> {
    vec![
        Entity {
            id: "note-prd",
            source_id: "vault.notes",
            kind: "Note",
            title: "Ocean GUI architecture spec",
            status: "draft",
            owner: "Smaths",
            updated: "now",
            summary: "Markdown source of truth for the Rust cockpit shape.",
            actions: &[
                "Open note",
                "Ask Charlotte to refine",
                "Create implementation tickets",
            ],
            links: &["docs/Ocean GUI.md", "AGENTS.md"],
        },
        Entity {
            id: "repo-ocean_gui",
            source_id: "dev.git",
            kind: "Repository",
            title: "Ocean GUI",
            status: "dirty",
            owner: "Rev",
            updated: "2m",
            summary: "Current Rust prototype with workspace and cockpit UI.",
            actions: &["Open terminal", "Show diff", "Ask Rev for review"],
            links: &["/Users/risingtidesdev/dev/Ocean GUI"],
        },
        Entity {
            id: "term-shell-1",
            source_id: "dev.terminals",
            kind: "PTY",
            title: "Ocean GUI build shell",
            status: "idle",
            owner: "Brick",
            updated: "5m",
            summary: "Future native terminal pane backed by portable-pty and terminal grid state.",
            actions: &["Attach", "Run cargo test", "Summarize session"],
            links: &["target/debug/ocean_gui"],
        },
        Entity {
            id: "session-henry",
            source_id: "mesh.sessions",
            kind: "Agent",
            title: "Henry / WritersRoom",
            status: "active",
            owner: "Tides Mesh",
            updated: "18s",
            summary: "Drafting and editorial synthesis agent connected to vault notes.",
            actions: &["Send message", "Open transcript", "Assign note"],
            links: &["~/.pi/agent/sessions", "WritersRoom"],
        },
        Entity {
            id: "session-brick",
            source_id: "mesh.sessions",
            kind: "Agent",
            title: "Brick / Backend",
            status: "available",
            owner: "Tides Mesh",
            updated: "1m",
            summary: "Backend, APIs, connectors, database work, and runtime tasks.",
            actions: &["Assign task", "Open terminal", "Request handoff"],
            links: &["TIDES-MESH BRICK"],
        },
        Entity {
            id: "model-gpt55",
            source_id: "mesh.models",
            kind: "Model",
            title: "openai-codex/gpt-5.5:xhigh",
            status: "judgment gate",
            owner: "Orchestrator",
            updated: "today",
            summary: "High-judgment model profile for orchestration, review, and risky decisions.",
            actions: &["Route Rev", "Route OWL", "Compare cost"],
            links: &["model profiles"],
        },
        Entity {
            id: "skill-cloudflare",
            source_id: "mesh.skills",
            kind: "Skill",
            title: "Cloudflare deploy",
            status: "available",
            owner: "Brick",
            updated: "today",
            summary: "Deploy Workers, Pages, R2, routes, and edge services with approval gates.",
            actions: &["Load skill", "Attach to task", "Open docs"],
            links: &["~/.codex/skills/cloudflare-deploy"],
        },
        Entity {
            id: "routine-vaultkeeper",
            source_id: "mesh.routines",
            kind: "Routine",
            title: "Vaultkeeper heartbeat",
            status: "scheduled",
            owner: "Thoth",
            updated: "hourly",
            summary: "Recurring vault health, orphan detection, and config validation job.",
            actions: &["Run now", "View log", "Pause routine"],
            links: &["~/.pi/agent/routines/vault-keeper"],
        },
        Entity {
            id: "THO-17",
            source_id: "mesh.tickets",
            kind: "Ticket",
            title: "Native Ocean GUI cockpit shell",
            status: "in_progress",
            owner: "Pixel + Brick",
            updated: "now",
            summary: "Build the first unified cockpit over vault, mesh, terminal, and data sources.",
            actions: &["Focus ticket", "Create branch", "Ask Rev to review"],
            links: &["6-Agent/tickets/THO-17.md"],
        },
        Entity {
            id: "svc-pi-cockpit",
            source_id: "ops.services",
            kind: "Service",
            title: "PI Cockpit Hub",
            status: "running",
            owner: "Ops",
            updated: "30s",
            summary: "Local hub pattern Ocean GUI will replace with native Rust state.",
            actions: &["View health", "Open logs", "Restart with approval"],
            links: &["http://localhost:3099/health"],
        },
        Entity {
            id: "approval-deploy",
            source_id: "ops.approvals",
            kind: "Approval",
            title: "Deploy dashboard worker",
            status: "waiting",
            owner: "Human",
            updated: "12m",
            summary: "Production-risk deploy requires explicit human approval before execution.",
            actions: &["Approve", "Reject", "Ask Rev for checklist"],
            links: &["Cloudflare Worker", "Rev review"],
        },
        Entity {
            id: "db-campaign",
            source_id: "data.databases",
            kind: "Database",
            title: "Campaign Postgres",
            status: "connected",
            owner: "Analytics",
            updated: "4m",
            summary: "Operational data source for campaign, content, finance, and company dashboards.",
            actions: &["Run query", "Save report", "Ask Charlotte to interpret"],
            links: &["DATABASE_URL", "campaign_hub"],
        },
        Entity {
            id: "report-content",
            source_id: "data.campaigns",
            kind: "Report",
            title: "Content pipeline health",
            status: "green",
            owner: "Rising Tides",
            updated: "9m",
            summary: "Campaign/content throughput, stuck jobs, upload stats, and revenue-adjacent signals.",
            actions: &["Refresh", "Export note", "Schedule morning digest"],
            links: &["TideDash", "Content Lab"],
        },
    ]
}

fn events() -> Vec<Event> {
    vec![
        Event {
            at: "now",
            actor: "Ocean GUI",
            action: "space.open",
            target: "Mesh",
            outcome: "agent cockpit ready",
        },
        Event {
            at: "1m",
            actor: "Henry",
            action: "note.create",
            target: "Ocean GUI architecture spec",
            outcome: "draft note available",
        },
        Event {
            at: "4m",
            actor: "Brick",
            action: "db.query",
            target: "Campaign Postgres",
            outcome: "12 rows returned",
        },
        Event {
            at: "7m",
            actor: "Rev",
            action: "review.request",
            target: "repo-ocean_gui",
            outcome: "pending",
        },
        Event {
            at: "12m",
            actor: "Ops",
            action: "approval.request",
            target: "Deploy dashboard worker",
            outcome: "waiting for human",
        },
    ]
}
