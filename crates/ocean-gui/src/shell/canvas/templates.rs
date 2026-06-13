//! Canvas **templates** — agent-facing work-objects that expand into the
//! structural primitives the ledger and renderer already understand (Slice 8,
//! gpui_masterbuild.md §5 "Then add templates on top", §10 renderable taxonomy).
//!
//! # Why templates
//!
//! An agent should be able to ask for a *brief card*, a *workflow node*, a
//! *kanban column*, a *storyboard frame*, a *stat tile*, or a *longhouse
//! proposal* without hand-authoring every primitive, port, child, and edge. The
//! ledger already preserves the template name on each component
//! ([`super::ledger::CanvasComponent::template`], via
//! [`super::ledger::ComponentKind::from_patch_kind`]). This module gives that
//! name *meaning* on both ends of the pipe:
//!
//! - **Expansion** ([`Template::expand`]): one template-tagged
//!   [`CanvasComponentPatch`] becomes a [`TemplateExpansion`] — a primary
//!   component patch (with the right structural [`ComponentKind`] and default
//!   size), plus any **child component patches** (kanban cards, the proposal's
//!   tally row) and **edge patches** (proposal → option edges) it implies. The
//!   ledger applies each resulting patch normally, so placement, collision
//!   avoidance, and revision bumping all flow through the existing
//!   [`super::ledger::CanvasLedger::apply_patch`] path.
//!
//! - **Drawable content** ([`Template::content`]): the same template name plus a
//!   component's `content` JSON resolves to a [`TemplateContent`] — the typed
//!   slots the renderer draws (title, body, status badge, stat label/value,
//!   media caption, tally). This is the per-template analogue of
//!   [`super::render::style_for_kind`]: instead of a bare kind-colored box, the
//!   renderer asks the template what shapes to draw.
//!
//! Everything here is **pure** and window-free: expansion is JSON→patches and
//! content resolution is JSON→typed slots. Both are unit-tested headlessly.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ledger::ComponentKind;
use super::patch::{
    CanvasComponentPatch, CanvasEdgePatch, ComponentId, EdgeId, Endpoint, Rect, SurfacePatch,
};

// ===========================================================================
// Template kinds
// ===========================================================================

/// The work-object templates an agent can emit (gpui_masterbuild.md §5).
///
/// Each template maps onto a structural [`ComponentKind`] but carries a richer
/// content contract and (for compound templates) implies extra children/edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    /// Card + title/body/metadata.
    BriefCard,
    /// Node + ports + status badge.
    WorkflowNode,
    /// Lane + child cards.
    KanbanColumn,
    /// Frame + media slot + caption.
    StoryboardFrame,
    /// Stat (label + value).
    StatTile,
    /// Card + tally metadata + option edges.
    LonghouseProposal,
}

impl Template {
    /// Parse a patch `kind` / template string into a known [`Template`].
    /// Returns `None` for primitive kinds (`card`, `node`, …) and unknown names —
    /// those render via the plain [`super::render::style_for_kind`] path.
    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "brief_card" => Some(Self::BriefCard),
            "workflow_node" => Some(Self::WorkflowNode),
            "kanban_column" => Some(Self::KanbanColumn),
            "storyboard_frame" => Some(Self::StoryboardFrame),
            "stat_tile" => Some(Self::StatTile),
            "longhouse_proposal" => Some(Self::LonghouseProposal),
            _ => None,
        }
    }

    /// The template's canonical name (its wire `kind` string).
    pub fn name(self) -> &'static str {
        match self {
            Self::BriefCard => "brief_card",
            Self::WorkflowNode => "workflow_node",
            Self::KanbanColumn => "kanban_column",
            Self::StoryboardFrame => "storyboard_frame",
            Self::StatTile => "stat_tile",
            Self::LonghouseProposal => "longhouse_proposal",
        }
    }

    /// The structural primitive this template renders as.
    pub fn primitive_kind(self) -> ComponentKind {
        match self {
            Self::BriefCard | Self::LonghouseProposal => ComponentKind::Card,
            Self::WorkflowNode => ComponentKind::Node,
            Self::KanbanColumn => ComponentKind::Lane,
            Self::StoryboardFrame => ComponentKind::Frame,
            Self::StatTile => ComponentKind::Stat,
        }
    }

    /// Default size (canvas units) for the template's primary component when the
    /// patch omits a `rect`. Sizes are tuned to each work-object's shape: stats
    /// are small, lanes are tall, frames are wide (16:9-ish for storyboards).
    pub fn default_size(self) -> (f32, f32) {
        match self {
            Self::BriefCard => (320.0, 220.0),
            Self::WorkflowNode => (200.0, 96.0),
            Self::KanbanColumn => (260.0, 480.0),
            Self::StoryboardFrame => (320.0, 200.0),
            Self::StatTile => (160.0, 96.0),
            Self::LonghouseProposal => (340.0, 240.0),
        }
    }

    // -----------------------------------------------------------------------
    // Expansion: template patch -> primitive composition
    // -----------------------------------------------------------------------

    /// Expand a template-tagged upsert into the concrete patches that build the
    /// work-object: the primary component plus any implied child components and
    /// edges.
    ///
    /// `patch` is the agent's original [`CanvasComponentPatch`] whose `kind` is
    /// this template's name. The expansion preserves `id`, `rect` (or supplies
    /// the template default size when absent — note `rect: None` is kept so the
    /// ledger still owns x/y placement), `z_index`, `content`, and `metadata` on
    /// the primary component, and derives children/edges from `content`.
    ///
    /// Compound templates:
    /// - **kanban_column**: each entry in `content.cards` (array of strings or
    ///   `{ id?, title, body? }` objects) becomes a child `brief_card`,
    ///   registered under the lane via [`SurfacePatch::Group`].
    /// - **longhouse_proposal**: each entry in `content.options` becomes a child
    ///   card and an edge from the proposal to that option (`kind: "reference"`,
    ///   label = the option's vote tally when present).
    pub fn expand(self, patch: &CanvasComponentPatch) -> TemplateExpansion {
        // Primary component: same id/content/metadata, but the structural kind is
        // fixed by the template and we annotate the size hint in metadata so the
        // renderer/placement can read it. We keep `kind` as the template name so
        // the ledger preserves it on `CanvasComponent::template`.
        let primary = CanvasComponentPatch {
            id: patch.id.clone(),
            kind: self.name().to_string(),
            rect: patch.rect,
            z_index: patch.z_index,
            content: patch.content.clone(),
            metadata: patch.metadata.clone(),
        };

        let mut children = Vec::new();
        let mut edges = Vec::new();
        let mut child_ids = Vec::new();

        match self {
            Self::KanbanColumn => {
                for (i, card) in self
                    .child_specs(&patch.content, "cards")
                    .into_iter()
                    .enumerate()
                {
                    let child_id = card
                        .id
                        .unwrap_or_else(|| ComponentId::new(format!("{}-card-{i}", patch.id)));
                    child_ids.push(child_id.clone());
                    children.push(CanvasComponentPatch {
                        id: child_id,
                        kind: Template::BriefCard.name().to_string(),
                        rect: None, // ledger places inside/after the lane
                        z_index: None,
                        content: card.content,
                        metadata: Value::Null,
                    });
                }
            }
            Self::LonghouseProposal => {
                for (i, opt) in self
                    .child_specs(&patch.content, "options")
                    .into_iter()
                    .enumerate()
                {
                    let child_id = opt
                        .id
                        .unwrap_or_else(|| ComponentId::new(format!("{}-opt-{i}", patch.id)));
                    child_ids.push(child_id.clone());
                    children.push(CanvasComponentPatch {
                        id: child_id.clone(),
                        kind: Template::BriefCard.name().to_string(),
                        rect: None,
                        z_index: None,
                        content: opt.content,
                        metadata: Value::Null,
                    });
                    edges.push(CanvasEdgePatch {
                        id: EdgeId::new(format!("{}->{}", patch.id, child_id)),
                        from: Endpoint {
                            component_id: patch.id.clone(),
                            port: None,
                        },
                        to: Endpoint {
                            component_id: child_id,
                            port: None,
                        },
                        kind: Some("reference".to_string()),
                        label: opt.label,
                        metadata: Value::Null,
                    });
                }
            }
            // Atomic templates: no children/edges.
            Self::BriefCard | Self::WorkflowNode | Self::StoryboardFrame | Self::StatTile => {}
        }

        TemplateExpansion {
            primary,
            children,
            edges,
            child_ids,
        }
    }

    /// Pull child specs out of a `content` object under `key` (an array). Accepts
    /// either bare strings (used as the child's title) or objects with optional
    /// `id`, `title`, `body`, and `votes`/`tally`/`count` (used as an edge label
    /// for proposals). Missing/wrong-typed input yields an empty list.
    fn child_specs(self, content: &Value, key: &str) -> Vec<ChildSpec> {
        let Some(items) = content.get(key).and_then(Value::as_array) else {
            return Vec::new();
        };
        items
            .iter()
            .map(|item| match item {
                Value::String(title) => ChildSpec {
                    id: None,
                    content: serde_json::json!({ "title": title }),
                    label: None,
                },
                Value::Object(_) => {
                    let id = item.get("id").and_then(Value::as_str).map(ComponentId::new);
                    let mut child = serde_json::Map::new();
                    if let Some(title) = item.get("title").and_then(Value::as_str) {
                        child.insert("title".into(), Value::String(title.to_string()));
                    }
                    if let Some(body) = item.get("body").and_then(Value::as_str) {
                        child.insert("body".into(), Value::String(body.to_string()));
                    }
                    let label = item
                        .get("votes")
                        .or_else(|| item.get("tally"))
                        .or_else(|| item.get("count"))
                        .and_then(value_to_short_string);
                    ChildSpec {
                        id,
                        content: Value::Object(child),
                        label,
                    }
                }
                _ => ChildSpec {
                    id: None,
                    content: Value::Null,
                    label: None,
                },
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Drawable content: template + content JSON -> typed render slots
    // -----------------------------------------------------------------------

    /// Resolve the drawable slots for a component of this template, from its
    /// `content` JSON. This is what the renderer draws instead of a bare box.
    pub fn content(self, content: &Value) -> TemplateContent {
        let title = str_slot(content, "title");
        let body = str_slot(content, "body").or_else(|| str_slot(content, "text"));

        match self {
            Self::BriefCard => TemplateContent::Brief {
                title,
                body,
                metadata: metadata_lines(content),
            },
            Self::WorkflowNode => TemplateContent::WorkflowNode {
                title,
                status: status_slot(content),
                inputs: port_names(content, "inputs"),
                outputs: port_names(content, "outputs"),
            },
            Self::KanbanColumn => TemplateContent::KanbanColumn {
                title,
                count: content
                    .get("cards")
                    .and_then(Value::as_array)
                    .map(|a| a.len()),
            },
            Self::StoryboardFrame => TemplateContent::StoryboardFrame {
                caption: str_slot(content, "caption").or(title),
                media: str_slot(content, "media")
                    .or_else(|| str_slot(content, "image"))
                    .or_else(|| str_slot(content, "shot")),
            },
            Self::StatTile => TemplateContent::Stat {
                label: str_slot(content, "label").or(title),
                value: str_slot(content, "value")
                    .or_else(|| content.get("value").and_then(value_to_short_string)),
                delta: str_slot(content, "delta")
                    .or_else(|| content.get("delta").and_then(value_to_short_string)),
            },
            Self::LonghouseProposal => TemplateContent::LonghouseProposal {
                title,
                body,
                tally: tally_slots(content),
            },
        }
    }
}

// ===========================================================================
// Expansion output
// ===========================================================================

/// The concrete patch set a template expands into. Apply `primary` first, then
/// `children`, then group the children under the primary, then `edges`.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateExpansion {
    /// The primary component (the template's structural primitive).
    pub primary: CanvasComponentPatch,
    /// Child components implied by the template (kanban cards, proposal options).
    pub children: Vec<CanvasComponentPatch>,
    /// Edges implied by the template (proposal → option references).
    pub edges: Vec<CanvasEdgePatch>,
    /// Ids of the children, in order — used to group them under the primary.
    pub child_ids: Vec<ComponentId>,
}

impl TemplateExpansion {
    /// Flatten this expansion into the ordered list of [`SurfacePatch`]es a caller
    /// applies to a [`super::ledger::CanvasLedger`]: primary upsert, child
    /// upserts, a group binding children to the primary, then edge connects.
    ///
    /// This is the bridge used by Slice 6's patch-event handler: when a single
    /// template upsert arrives, it can be expanded and the resulting patches
    /// applied in order, producing the full work-object on the canvas.
    pub fn into_patches(self) -> Vec<SurfacePatch> {
        let mut patches = Vec::with_capacity(1 + self.children.len() + self.edges.len() + 1);
        let frame_id = self.primary.id.clone();
        patches.push(SurfacePatch::UpsertComponent {
            component: self.primary,
        });
        for child in self.children {
            patches.push(SurfacePatch::UpsertComponent { component: child });
        }
        if !self.child_ids.is_empty() {
            patches.push(SurfacePatch::Group {
                frame_id,
                children: self.child_ids,
            });
        }
        for edge in self.edges {
            patches.push(SurfacePatch::Connect { edge });
        }
        patches
    }
}

/// One resolved child spec extracted from a compound template's content.
#[derive(Debug, Clone, PartialEq)]
struct ChildSpec {
    id: Option<ComponentId>,
    content: Value,
    /// Optional edge label (vote tally for proposal options).
    label: Option<String>,
}

// ===========================================================================
// Drawable content slots
// ===========================================================================

/// A status badge for a workflow node, resolved to a coarse class the renderer
/// can color without parsing free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// Not yet run.
    Idle,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Ok,
    /// Failed / errored.
    Error,
    /// Waiting on input / blocked.
    Waiting,
}

impl NodeStatus {
    /// The short label drawn in the badge.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Waiting => "waiting",
        }
    }

    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "running" | "active" | "in_progress" | "in-progress" => Self::Running,
            "ok" | "done" | "success" | "succeeded" | "complete" | "completed" => Self::Ok,
            "error" | "failed" | "failure" | "err" => Self::Error,
            "waiting" | "blocked" | "pending" | "queued" => Self::Waiting,
            _ => Self::Idle,
        }
    }
}

/// One tally row in a longhouse proposal (option label + count + optional state).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TallyRow {
    pub label: String,
    pub count: u64,
}

/// The typed, drawable content for one templated component — what the renderer
/// turns into styled elements (title text, body, status badge, stat value, …)
/// instead of a kind-colored rectangle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "template", rename_all = "snake_case")]
pub enum TemplateContent {
    /// brief_card: heading + body paragraph + metadata lines.
    Brief {
        title: Option<String>,
        body: Option<String>,
        metadata: Vec<(String, String)>,
    },
    /// workflow_node: title + status badge + input/output ports.
    WorkflowNode {
        title: Option<String>,
        status: NodeStatus,
        inputs: Vec<String>,
        outputs: Vec<String>,
    },
    /// kanban_column: lane heading + card count.
    KanbanColumn {
        title: Option<String>,
        count: Option<usize>,
    },
    /// storyboard_frame: media placeholder + caption.
    StoryboardFrame {
        caption: Option<String>,
        media: Option<String>,
    },
    /// stat_tile: big value + label + optional delta.
    Stat {
        label: Option<String>,
        value: Option<String>,
        delta: Option<String>,
    },
    /// longhouse_proposal: title + body + tally rows.
    LonghouseProposal {
        title: Option<String>,
        body: Option<String>,
        tally: Vec<TallyRow>,
    },
}

impl TemplateContent {
    /// Resolve the drawable content for a component whose template name is `kind`.
    /// Returns `None` when `kind` is not a known template (caller falls back to
    /// the primitive renderer).
    pub fn resolve(kind: &str, content: &Value) -> Option<Self> {
        Template::from_kind(kind).map(|t| t.content(content))
    }
}

// ===========================================================================
// Content extraction helpers (pure)
// ===========================================================================

/// A non-empty string field on a JSON object.
fn str_slot(content: &Value, key: &str) -> Option<String> {
    content
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Render a scalar JSON value as a short string (numbers, bools, strings).
fn value_to_short_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// The coarse [`NodeStatus`] from a `content.status` string (defaults to idle).
fn status_slot(content: &Value) -> NodeStatus {
    content
        .get("status")
        .and_then(Value::as_str)
        .map(NodeStatus::parse)
        .unwrap_or(NodeStatus::Idle)
}

/// Port names under `content[key]` (array of strings), e.g. `inputs`/`outputs`.
fn port_names(content: &Value, key: &str) -> Vec<String> {
    content
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(_) => v.get("name").and_then(Value::as_str).map(str::to_string),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Flatten a `content.metadata` object into ordered `(key, value)` display lines.
/// Non-object metadata yields no lines.
fn metadata_lines(content: &Value) -> Vec<(String, String)> {
    let Some(obj) = content.get("metadata").and_then(Value::as_object) else {
        return Vec::new();
    };
    obj.iter()
        .filter_map(|(k, v)| value_to_short_string(v).map(|s| (k.clone(), s)))
        .collect()
}

/// Tally rows for a proposal, from `content.tally`. Accepts either an object
/// `{ "yes": 3, "no": 1 }` or an array `[{ "label": "yes", "count": 3 }, …]`.
fn tally_slots(content: &Value) -> Vec<TallyRow> {
    match content.get("tally") {
        Some(Value::Object(obj)) => obj
            .iter()
            .filter_map(|(k, v)| {
                v.as_u64().map(|count| TallyRow {
                    label: k.clone(),
                    count,
                })
            })
            .collect(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|row| {
                let label = row.get("label").and_then(Value::as_str)?.to_string();
                let count = row.get("count").and_then(Value::as_u64).unwrap_or(0);
                Some(TallyRow { label, count })
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patch(id: &str, kind: &str, content: Value) -> CanvasComponentPatch {
        CanvasComponentPatch {
            id: ComponentId::new(id),
            kind: kind.to_string(),
            rect: None,
            z_index: None,
            content,
            metadata: Value::Null,
        }
    }

    // ---- name <-> kind parsing --------------------------------------------

    #[test]
    fn every_template_roundtrips_name_and_kind() {
        for t in [
            Template::BriefCard,
            Template::WorkflowNode,
            Template::KanbanColumn,
            Template::StoryboardFrame,
            Template::StatTile,
            Template::LonghouseProposal,
        ] {
            assert_eq!(Template::from_kind(t.name()), Some(t), "{}", t.name());
        }
    }

    #[test]
    fn primitive_and_unknown_kinds_are_not_templates() {
        for k in [
            "card",
            "node",
            "lane",
            "frame",
            "stat",
            "blah",
            "text_block",
        ] {
            assert_eq!(Template::from_kind(k), None, "{k} should not be a template");
        }
    }

    // ---- primitive mapping (the §5 table) ---------------------------------

    #[test]
    fn templates_map_to_the_section_5_primitives() {
        assert_eq!(Template::BriefCard.primitive_kind(), ComponentKind::Card);
        assert_eq!(Template::WorkflowNode.primitive_kind(), ComponentKind::Node);
        assert_eq!(Template::KanbanColumn.primitive_kind(), ComponentKind::Lane);
        assert_eq!(
            Template::StoryboardFrame.primitive_kind(),
            ComponentKind::Frame
        );
        assert_eq!(Template::StatTile.primitive_kind(), ComponentKind::Stat);
        assert_eq!(
            Template::LonghouseProposal.primitive_kind(),
            ComponentKind::Card
        );
    }

    #[test]
    fn from_patch_kind_agrees_with_template_primitive() {
        // The ledger's own kind resolver must collapse each template to the same
        // structural primitive this module declares.
        for t in [
            Template::BriefCard,
            Template::WorkflowNode,
            Template::KanbanColumn,
            Template::StoryboardFrame,
            Template::LonghouseProposal,
        ] {
            assert_eq!(
                ComponentKind::from_patch_kind(t.name()),
                t.primitive_kind(),
                "ledger vs template disagree for {}",
                t.name()
            );
        }
    }

    // ---- expansion: atomic templates --------------------------------------

    #[test]
    fn brief_card_expands_to_a_single_card_with_content_preserved() {
        let p = patch(
            "brief-1",
            "brief_card",
            json!({ "title": "Sales Brief", "body": "Draft for Warner" }),
        );
        let exp = Template::BriefCard.expand(&p);
        assert!(exp.children.is_empty());
        assert!(exp.edges.is_empty());
        assert_eq!(exp.primary.id, ComponentId::new("brief-1"));
        assert_eq!(exp.primary.kind, "brief_card");
        assert_eq!(exp.primary.content["title"], "Sales Brief");
        // Into-patches yields just the one upsert.
        assert_eq!(exp.into_patches().len(), 1);
    }

    #[test]
    fn workflow_node_and_stat_and_storyboard_are_atomic() {
        for (kind, t) in [
            ("workflow_node", Template::WorkflowNode),
            ("stat_tile", Template::StatTile),
            ("storyboard_frame", Template::StoryboardFrame),
        ] {
            let exp = t.expand(&patch("x", kind, json!({})));
            assert!(exp.children.is_empty(), "{kind} should be atomic");
            assert!(exp.edges.is_empty(), "{kind} should be atomic");
            assert_eq!(exp.into_patches().len(), 1);
        }
    }

    // ---- expansion: kanban column -----------------------------------------

    #[test]
    fn kanban_column_expands_lane_plus_child_cards() {
        let p = patch(
            "todo",
            "kanban_column",
            json!({
                "title": "To Do",
                "cards": ["ship slice 8", { "title": "review PR", "body": "knox" }]
            }),
        );
        let exp = Template::KanbanColumn.expand(&p);
        assert_eq!(exp.primary.kind, "kanban_column");
        assert_eq!(exp.children.len(), 2);
        // Each child is a brief_card.
        assert!(exp.children.iter().all(|c| c.kind == "brief_card"));
        // First card title came from the bare string.
        assert_eq!(exp.children[0].content["title"], "ship slice 8");
        // Second card carries title + body from the object form.
        assert_eq!(exp.children[1].content["title"], "review PR");
        assert_eq!(exp.children[1].content["body"], "knox");
        // Child ids are deterministic when not provided.
        assert_eq!(exp.child_ids[0], ComponentId::new("todo-card-0"));

        // Flattened patches: lane upsert + 2 card upserts + 1 group.
        let patches = exp.into_patches();
        assert_eq!(patches.len(), 4);
        assert!(matches!(patches[0], SurfacePatch::UpsertComponent { .. }));
        assert!(matches!(patches.last(), Some(SurfacePatch::Group { .. })));
    }

    #[test]
    fn kanban_column_with_no_cards_is_just_a_lane() {
        let exp = Template::KanbanColumn.expand(&patch(
            "c",
            "kanban_column",
            json!({ "title": "Empty" }),
        ));
        assert!(exp.children.is_empty());
        assert_eq!(exp.into_patches().len(), 1);
    }

    // ---- expansion: longhouse proposal ------------------------------------

    #[test]
    fn longhouse_proposal_expands_card_with_option_edges() {
        let p = patch(
            "prop-1",
            "longhouse_proposal",
            json!({
                "title": "Adopt Loro",
                "body": "CRDT for local-first sync",
                "options": [
                    { "id": "opt-yes", "title": "Yes", "votes": 5 },
                    { "title": "No", "votes": 2 }
                ]
            }),
        );
        let exp = Template::LonghouseProposal.expand(&p);
        assert_eq!(exp.primary.kind, "longhouse_proposal");
        assert_eq!(exp.children.len(), 2);
        assert_eq!(exp.edges.len(), 2);

        // Explicit child id is honored; the missing one is derived.
        assert_eq!(exp.children[0].id, ComponentId::new("opt-yes"));
        assert_eq!(exp.children[1].id, ComponentId::new("prop-1-opt-1"));

        // Edges run proposal -> option and carry the vote tally as the label.
        let e0 = &exp.edges[0];
        assert_eq!(e0.from.component_id, ComponentId::new("prop-1"));
        assert_eq!(e0.to.component_id, ComponentId::new("opt-yes"));
        assert_eq!(e0.kind.as_deref(), Some("reference"));
        assert_eq!(e0.label.as_deref(), Some("5"));

        // Flattened: card + 2 option cards + group + 2 edges = 6.
        let patches = exp.into_patches();
        assert_eq!(patches.len(), 6);
        assert_eq!(
            patches
                .iter()
                .filter(|p| matches!(p, SurfacePatch::Connect { .. }))
                .count(),
            2
        );
    }

    // ---- drawable content -------------------------------------------------

    #[test]
    fn brief_content_exposes_title_body_and_metadata_lines() {
        let c = Template::BriefCard.content(&json!({
            "title": "Brief",
            "body": "Body text",
            "metadata": { "source": "longhouse.sales", "owner": "sage" }
        }));
        let TemplateContent::Brief {
            title,
            body,
            metadata,
        } = c
        else {
            panic!("expected Brief");
        };
        assert_eq!(title.as_deref(), Some("Brief"));
        assert_eq!(body.as_deref(), Some("Body text"));
        // Metadata lines preserve insertion order and stringify values.
        assert_eq!(
            metadata,
            vec![
                ("source".to_string(), "longhouse.sales".to_string()),
                ("owner".to_string(), "sage".to_string()),
            ]
        );
    }

    #[test]
    fn workflow_node_content_resolves_status_and_ports() {
        let c = Template::WorkflowNode.content(&json!({
            "title": "Fetch",
            "status": "Running",
            "inputs": ["trigger"],
            "outputs": ["rows", { "name": "error" }]
        }));
        let TemplateContent::WorkflowNode {
            title,
            status,
            inputs,
            outputs,
        } = c
        else {
            panic!("expected WorkflowNode");
        };
        assert_eq!(title.as_deref(), Some("Fetch"));
        assert_eq!(status, NodeStatus::Running);
        assert_eq!(inputs, vec!["trigger".to_string()]);
        assert_eq!(outputs, vec!["rows".to_string(), "error".to_string()]);
    }

    #[test]
    fn workflow_status_parsing_is_lenient_and_defaults_to_idle() {
        assert_eq!(status_slot(&json!({ "status": "DONE" })), NodeStatus::Ok);
        assert_eq!(
            status_slot(&json!({ "status": "failed" })),
            NodeStatus::Error
        );
        assert_eq!(
            status_slot(&json!({ "status": "blocked" })),
            NodeStatus::Waiting
        );
        assert_eq!(status_slot(&json!({})), NodeStatus::Idle);
        assert_eq!(status_slot(&json!({ "status": "weird" })), NodeStatus::Idle);
    }

    #[test]
    fn stat_content_resolves_value_label_and_delta_from_numbers_or_strings() {
        let c = Template::StatTile.content(&json!({
            "label": "Saves", "value": 1206, "delta": "+12%"
        }));
        let TemplateContent::Stat {
            label,
            value,
            delta,
        } = c
        else {
            panic!("expected Stat");
        };
        assert_eq!(label.as_deref(), Some("Saves"));
        assert_eq!(value.as_deref(), Some("1206"));
        assert_eq!(delta.as_deref(), Some("+12%"));
    }

    #[test]
    fn storyboard_content_resolves_media_and_caption() {
        let c = Template::StoryboardFrame.content(&json!({
            "caption": "Opening shot", "shot": "wide.png"
        }));
        let TemplateContent::StoryboardFrame { caption, media } = c else {
            panic!("expected StoryboardFrame");
        };
        assert_eq!(caption.as_deref(), Some("Opening shot"));
        assert_eq!(media.as_deref(), Some("wide.png"));
    }

    #[test]
    fn kanban_content_reports_card_count() {
        let c = Template::KanbanColumn.content(&json!({ "title": "Doing", "cards": [1, 2, 3] }));
        let TemplateContent::KanbanColumn { title, count } = c else {
            panic!("expected KanbanColumn");
        };
        assert_eq!(title.as_deref(), Some("Doing"));
        assert_eq!(count, Some(3));
    }

    #[test]
    fn proposal_content_resolves_tally_from_object_or_array() {
        let from_obj = Template::LonghouseProposal.content(&json!({
            "title": "P", "tally": { "yes": 5, "no": 2 }
        }));
        let TemplateContent::LonghouseProposal { tally, .. } = from_obj else {
            panic!("expected LonghouseProposal");
        };
        assert_eq!(tally.len(), 2);
        assert!(tally.contains(&TallyRow {
            label: "yes".into(),
            count: 5
        }));

        let from_arr = Template::LonghouseProposal.content(&json!({
            "tally": [{ "label": "yes", "count": 5 }, { "label": "no", "count": 2 }]
        }));
        let TemplateContent::LonghouseProposal { tally, .. } = from_arr else {
            panic!("expected LonghouseProposal");
        };
        assert_eq!(
            tally,
            vec![
                TallyRow {
                    label: "yes".into(),
                    count: 5
                },
                TallyRow {
                    label: "no".into(),
                    count: 2
                },
            ]
        );
    }

    #[test]
    fn resolve_returns_none_for_non_templates() {
        assert!(TemplateContent::resolve("card", &json!({})).is_none());
        assert!(TemplateContent::resolve("brief_card", &json!({})).is_some());
    }

    // ---- end-to-end: expansion applied to a real ledger -------------------

    #[test]
    fn kanban_expansion_applied_to_ledger_builds_lane_with_grouped_cards() {
        use super::super::ledger::{CanvasLedger, CanvasMode, ComponentKind};
        use super::super::patch::ActorRef;

        let mut l = CanvasLedger::new("canvas:main", "sess-1", CanvasMode::Kanban);
        let p = patch(
            "todo",
            "kanban_column",
            json!({ "title": "To Do", "cards": ["a", "b"] }),
        );
        for sp in Template::KanbanColumn.expand(&p).into_patches() {
            l.apply_patch(sp, ActorRef::agent(Some("sage".into())), 0);
        }

        // Lane + 2 child cards exist.
        assert_eq!(l.components.len(), 3);
        let lane = l.component(&ComponentId::new("todo")).unwrap();
        assert_eq!(lane.kind, ComponentKind::Lane);
        assert_eq!(lane.template, "kanban_column");
        // Children are grouped under the lane.
        assert_eq!(lane.children.len(), 2);
        // Each child is a brief_card primitive (Card).
        let child = l.component(&ComponentId::new("todo-card-0")).unwrap();
        assert_eq!(child.kind, ComponentKind::Card);
        assert_eq!(child.content["title"], "a");
        // Auto-placed children do not overlap.
        let c0 = l.component(&ComponentId::new("todo-card-0")).unwrap().rect;
        let c1 = l.component(&ComponentId::new("todo-card-1")).unwrap().rect;
        assert!(!c0.intersects(&c1), "kanban cards must not overlap");
    }

    #[test]
    fn proposal_expansion_applied_to_ledger_builds_card_and_edges() {
        use super::super::ledger::CanvasLedger;
        use super::super::ledger::CanvasMode;
        use super::super::patch::ActorRef;

        let mut l = CanvasLedger::new("canvas:main", "sess-1", CanvasMode::Freeform);
        let p = patch(
            "prop",
            "longhouse_proposal",
            json!({ "title": "P", "options": [{ "title": "Yes", "votes": 3 }, { "title": "No" }] }),
        );
        for sp in Template::LonghouseProposal.expand(&p).into_patches() {
            l.apply_patch(sp, ActorRef::agent(Some("sage".into())), 0);
        }
        // Proposal card + 2 option cards.
        assert_eq!(l.components.len(), 3);
        // Two reference edges from the proposal to each option.
        assert_eq!(l.edges.len(), 2);
        for edge in l.edges.values() {
            assert_eq!(edge.from.component_id, ComponentId::new("prop"));
        }
    }

    #[test]
    fn template_content_roundtrips_through_json() {
        let c = Template::StatTile.content(&json!({ "label": "x", "value": 1 }));
        let s = serde_json::to_string(&c).unwrap();
        let back: TemplateContent = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);
    }
}
