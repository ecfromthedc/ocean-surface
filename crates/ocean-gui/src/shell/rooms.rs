//! Native rooms state (OCEAN-109) — the GPUI counterpart to the web rooms UI
//! (OCEAN-108, `ocean-surface-ui`'s `rooms.rs`).
//!
//! This module owns the *state* of the rooms panel: the room list, the open
//! room + its transcript, a stable client identity, the composer/new-room
//! drafts, and a free-form status line. It is deliberately a plain, synchronous
//! reducer — the same shape as `agent.rs`'s `AgentState` — so it can be unit
//! tested without GPUI or the daemon. All HTTP I/O and the live-update plumbing
//! live in `view.rs`, which drives this state from background threads via the
//! shell's message-pump pattern (the same one the agent stream / permission
//! poll use).
//!
//! Daemon contract (matches OCEAN-108):
//!   GET    /v1/rooms/persistent                          → list rooms
//!   POST   /v1/rooms/persistent                          → create a room
//!   GET    /v1/rooms/persistent/{key}                    → room + transcript
//!   POST   /v1/rooms/persistent/{key}/participants       → join
//!   DELETE /v1/rooms/persistent/{key}/participants/{id}  → leave
//!   POST   /v1/rooms/persistent/{key}/messages           → post a message
//!   GET    /v1/rooms/persistent/{key}/transcript?after_seq=N → live tail
//!
//! Live updates: the daemon emits an unscoped `room_trigger` extension frame on
//! @-mention auto-convene (OCEAN-65). That frame is council-wide (no session
//! scope) so it never arrives on the GPUI shell's session-scoped agent stream
//! and is dropped by the control stream's session filter. So, exactly like the
//! web surface, the open room is kept fresh by polling its transcript on a short
//! interval with `after_seq` (cheap — each poll returns only the tail). The
//! `generation` counter retires a stale poll loop when the open room changes or
//! the panel closes, so a late poll can't write into the wrong room.

use std::fmt::Write as _;

use super::daemon::{Room, RoomMessage, RoomParticipantKind, RoomTriggerPolicy};

/// Which rooms-panel text input the shared key handler routes typed text to.
/// The GPUI shell uses a single focus handle for the whole panel, so this picks
/// the live draft (OCEAN-119). Defaults differ by view: the composer in a room,
/// the room-name input in the list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoomFocus {
    /// New-room name input (list view).
    NewRoomName,
    /// Cron-schedule input under the create toggles (list view).
    ScheduleCron,
    /// Message composer (room view) — the default in a room.
    Composer,
    /// Add-agent: the agent-id input (room view).
    AgentId,
    /// Add-agent: the optional display-name input (room view).
    AgentName,
}

/// This surface's stable identity as a room participant. Minted once per process
/// (the GPUI shell has no localStorage; a per-launch id is the native analogue,
/// and is enough for join/leave/author to all key on the same value within a
/// session). `display_name` defaults to the id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomIdentity {
    pub id: String,
    pub display_name: String,
}

impl RoomIdentity {
    /// Mint a fresh identity for this launch, e.g. `gpui-1a2b3c`.
    #[must_use]
    pub fn mint() -> Self {
        let id = format!("gpui-{}", mint_suffix());
        Self {
            display_name: id.clone(),
            id,
        }
    }
}

/// Reactive-free state for the rooms panel. Lives inside `OceanGuiShell` and is
/// mutated on the main thread from message-pump handlers.
#[derive(Clone, Debug)]
pub struct RoomsState {
    /// Whether the rooms panel is showing (toggled from the agent toolbar).
    pub panel_open: bool,
    /// All persistent rooms (from `GET /v1/rooms/persistent`).
    pub list: Vec<Room>,
    /// The currently selected room key, if any.
    pub open_key: Option<String>,
    /// The open room's full record (roster + metadata).
    pub open_room: Option<Room>,
    /// The open room's transcript, ascending by `seq`.
    pub transcript: Vec<RoomMessage>,
    /// Draft text for the "new room" name input.
    pub new_room_draft: String,
    /// Draft text for the message composer.
    pub composer_draft: String,
    /// Draft text for the add-agent control's agent-id input (room view).
    pub agent_id_draft: String,
    /// Draft text for the add-agent control's display-name input (optional).
    pub agent_name_draft: String,
    /// Which panel input the shared key handler routes typed text to.
    pub focus: RoomFocus,
    /// Trigger-policy toggles applied at room creation (OCEAN-119). `on_mention`
    /// defaults on (the common auto-convene case); the rest default off.
    /// `on_schedule_draft` is a free-form cron string (empty = no schedule).
    pub policy_on_mention: bool,
    pub policy_on_thread_reply: bool,
    pub policy_on_component_event: bool,
    pub policy_on_schedule_draft: String,
    /// Free-form status line (errors, in-flight notices).
    pub status: String,
    /// Monotonic generation: bumped when the open room changes so a stale
    /// transcript poll retires instead of writing into the wrong room.
    pub generation: u64,
    /// This surface's stable participant id / name (used for join/leave/post).
    pub identity: RoomIdentity,
}

impl Default for RoomsState {
    fn default() -> Self {
        Self {
            panel_open: false,
            list: Vec::new(),
            open_key: None,
            open_room: None,
            transcript: Vec::new(),
            new_room_draft: String::new(),
            composer_draft: String::new(),
            agent_id_draft: String::new(),
            agent_name_draft: String::new(),
            focus: RoomFocus::Composer,
            policy_on_mention: true,
            policy_on_thread_reply: false,
            policy_on_component_event: false,
            policy_on_schedule_draft: String::new(),
            status: String::new(),
            generation: 0,
            identity: RoomIdentity::mint(),
        }
    }
}

impl RoomsState {
    /// Whether the current identity is in the open room's roster.
    #[must_use]
    pub fn joined_open(&self) -> bool {
        let me = self.identity.id.as_str();
        self.open_room
            .as_ref()
            .map(|room| room.participants.iter().any(|p| p.id == me))
            .unwrap_or(false)
    }

    /// The highest transcript seq currently held (0 if empty) — the `after_seq`
    /// the next tail poll should request.
    #[must_use]
    pub fn highest_seq(&self) -> u64 {
        self.transcript.last().map(|m| m.seq).unwrap_or(0)
    }

    /// Whether the composer has a non-blank draft ready to send.
    #[must_use]
    pub fn can_send(&self) -> bool {
        self.open_key.is_some() && !self.composer_draft.trim().is_empty()
    }

    /// Replace the room list (after `fetch_rooms`).
    pub fn set_list(&mut self, rooms: Vec<Room>) {
        self.list = rooms;
    }

    /// Begin opening a room: bump the generation (retiring any prior poll loop),
    /// select the key, and clear the prior room's record/transcript so the panel
    /// shows a clean loading state. Returns the new generation for the caller to
    /// stamp on its poll loop.
    pub fn begin_open(&mut self, key: String) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.open_key = Some(key);
        self.open_room = None;
        self.transcript.clear();
        self.status = "loading room...".to_string();
        self.generation
    }

    /// Land a loaded room record + full transcript, if `generation` is still the
    /// active one (guards against a fast re-select). Returns whether it landed.
    pub fn apply_loaded(
        &mut self,
        generation: u64,
        room: Option<Room>,
        transcript: Vec<RoomMessage>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.open_room = room;
        self.transcript = transcript;
        self.status.clear();
        true
    }

    /// Close the open room and retire its poll loop.
    pub fn close_room(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.open_key = None;
        self.open_room = None;
        self.transcript.clear();
    }

    /// Append only genuinely-new transcript entries (seq strictly above what we
    /// hold), for `key` while it is still the open room. Returns whether
    /// anything was appended. Used by our own writes and the live poll.
    pub fn append_transcript_tail(&mut self, key: &str, tail: Vec<RoomMessage>) -> bool {
        if self.open_key.as_deref() != Some(key) {
            return false;
        }
        let mut appended = false;
        for message in tail {
            if self.highest_seq() < message.seq {
                self.transcript.push(message);
                appended = true;
            }
        }
        appended
    }

    /// Replace the open room's record (after a join/leave mutation response).
    pub fn set_open_room(&mut self, room: Option<Room>) {
        self.open_room = room;
    }

    /// Title shown in the panel header — the open room's name, else "Rooms".
    #[must_use]
    pub fn header_title(&self) -> String {
        self.open_room
            .as_ref()
            .map(|room| room.name.clone())
            .unwrap_or_else(|| "Rooms".to_string())
    }

    /// Assemble the create-time trigger policy from the toggles, or `None` if
    /// nothing is set (so the daemon stores no policy rather than an all-off one).
    /// Mirrors the web surface's `collect_policy` (OCEAN-117 / OCEAN-119).
    #[must_use]
    pub fn collect_trigger_policy(&self) -> Option<RoomTriggerPolicy> {
        let cron = self.policy_on_schedule_draft.trim().to_string();
        let on_schedule = if cron.is_empty() { None } else { Some(cron) };
        let policy = RoomTriggerPolicy {
            on_mention: self.policy_on_mention,
            on_thread_reply: self.policy_on_thread_reply,
            on_component_event: self.policy_on_component_event,
            on_schedule,
        };
        if policy == RoomTriggerPolicy::default() {
            None
        } else {
            Some(policy)
        }
    }

    /// Ids of the open room's **agent** participants — the actors a human can
    /// `@mention` to auto-convene. Used to render the composer's discoverability
    /// hint (OCEAN-119, matching the web surface's `agent_ids`).
    #[must_use]
    pub fn agent_ids(&self) -> Vec<String> {
        self.open_room
            .as_ref()
            .map(|room| {
                room.participants
                    .iter()
                    .filter(|p| p.kind == RoomParticipantKind::Agent)
                    .map(|p| p.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the add-agent control has a non-blank agent id ready to submit.
    #[must_use]
    pub fn can_add_agent(&self) -> bool {
        self.open_key.is_some() && !self.agent_id_draft.trim().is_empty()
    }

    /// A read-only summary of the open room's auto-convene triggers, e.g.
    /// "Auto-convene: @mention, schedule". `None` when the room carries no policy.
    /// Mirrors the web surface's policy summary (OCEAN-117).
    #[must_use]
    pub fn trigger_policy_summary(&self) -> Option<String> {
        let policy = self.open_room.as_ref()?.trigger_policy.as_ref()?;
        let mut on: Vec<&str> = Vec::new();
        if policy.on_mention {
            on.push("@mention");
        }
        if policy.on_thread_reply {
            on.push("thread-reply");
        }
        if policy.on_component_event {
            on.push("component-event");
        }
        if policy.on_schedule.is_some() {
            on.push("schedule");
        }
        let triggers = if on.is_empty() {
            "none".to_string()
        } else {
            on.join(", ")
        };
        Some(format!("Auto-convene: {triggers}"))
    }

    /// Insert `@id ` into the composer draft (from a mention-hint chip click),
    /// adding a leading space when the draft is non-empty and doesn't end in one.
    /// Mirrors the web surface's mention-chip insert (OCEAN-117).
    pub fn insert_mention(&mut self, agent_id: &str) {
        if !self.composer_draft.is_empty() && !self.composer_draft.ends_with(' ') {
            self.composer_draft.push(' ');
        }
        self.composer_draft.push('@');
        self.composer_draft.push_str(agent_id);
        self.composer_draft.push(' ');
    }

    /// The draft the focused input currently edits. Used by the panel's shared
    /// key handler to route typed text / backspace to the right place. When no
    /// room is open, the room-view targets fall back to the new-room name.
    fn focused_draft_mut(&mut self) -> &mut String {
        let in_room = self.open_key.is_some();
        match self.focus {
            RoomFocus::NewRoomName => &mut self.new_room_draft,
            RoomFocus::ScheduleCron => &mut self.policy_on_schedule_draft,
            RoomFocus::Composer if in_room => &mut self.composer_draft,
            RoomFocus::AgentId if in_room => &mut self.agent_id_draft,
            RoomFocus::AgentName if in_room => &mut self.agent_name_draft,
            // Room-view targets when no room is open → the name draft.
            _ => &mut self.new_room_draft,
        }
    }

    /// Append typed text to the focused draft (key handler).
    pub fn push_typed(&mut self, text: &str) {
        self.focused_draft_mut().push_str(text);
    }

    /// Backspace the focused draft (key handler).
    pub fn pop_typed(&mut self) {
        self.focused_draft_mut().pop();
    }
}

// ---- Helpers ----------------------------------------------------------------

/// A short, reasonably-unique suffix for a minted identity. No UUID crate in
/// this bundle, so derive one from the wall clock XOR a hash of the thread id.
fn mint_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut out = String::new();
    let _ = write!(&mut out, "{:x}", nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 24);
    out
}

/// Derive a url/key-safe slug from a room name (lowercase alnum + `-`). Matches
/// the web surface's `slugify` so a room created from either surface keys the
/// same way.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// A compact "last activity" label from an ISO-8601 timestamp — date+time to the
/// minute. Empty input → empty string. Matches the web surface's `short_time`.
#[must_use]
pub fn short_time(ts: &str) -> String {
    if ts.is_empty() {
        return String::new();
    }
    let trimmed = ts.split('.').next().unwrap_or(ts).replace('T', " ");
    trimmed.chars().take(16).collect()
}

/// A short participant-count label, e.g. "1 participant" / "3 participants".
#[must_use]
pub fn participant_count_label(count: usize) -> String {
    if count == 1 {
        "1 participant".to_string()
    } else {
        format!("{count} participants")
    }
}

/// The author chip label for a transcript entry: a kind glyph + the author id.
#[must_use]
pub fn author_label(author_id: &str, kind: RoomParticipantKind) -> String {
    format!("{} {}", kind.glyph(), author_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::daemon::{RoomMessageKind, RoomParticipant};

    fn participant(id: &str, kind: RoomParticipantKind) -> RoomParticipant {
        RoomParticipant {
            id: id.to_string(),
            kind,
            display_name: id.to_string(),
        }
    }

    fn message(seq: u64, body: &str) -> RoomMessage {
        RoomMessage {
            seq,
            author_id: "gpui-1".to_string(),
            author_kind: RoomParticipantKind::Human,
            kind: RoomMessageKind::Message,
            body: body.to_string(),
            created_at: String::new(),
        }
    }

    fn room(key: &str, participants: Vec<RoomParticipant>) -> Room {
        Room {
            id: key.to_string(),
            name: key.to_string(),
            participants,
            created_at: String::new(),
            updated_at: String::new(),
            trigger_policy: None,
        }
    }

    #[test]
    fn slugify_lowercases_and_dashes_like_web_surface() {
        assert_eq!(slugify("Map Fix"), "map-fix");
        assert_eq!(slugify("  Ocean   Surface!! "), "ocean-surface");
        assert_eq!(slugify("already-ok_123"), "already-ok-123");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn short_time_trims_iso_to_minute() {
        assert_eq!(short_time("2026-06-05T12:34:56.789Z"), "2026-06-05 12:34");
        assert_eq!(short_time(""), "");
    }

    #[test]
    fn participant_count_label_singular_and_plural() {
        assert_eq!(participant_count_label(0), "0 participants");
        assert_eq!(participant_count_label(1), "1 participant");
        assert_eq!(participant_count_label(4), "4 participants");
    }

    #[test]
    fn joined_open_reflects_identity_in_roster() {
        let mut state = RoomsState::default();
        let me = state.identity.id.clone();
        assert!(!state.joined_open());

        state.open_key = Some("map-fix".to_string());
        state.open_room = Some(room(
            "map-fix",
            vec![participant("someone-else", RoomParticipantKind::Agent)],
        ));
        assert!(!state.joined_open());

        state.open_room = Some(room(
            "map-fix",
            vec![participant(&me, RoomParticipantKind::Human)],
        ));
        assert!(state.joined_open());
    }

    #[test]
    fn begin_open_bumps_generation_and_clears_prior_room() {
        let mut state = RoomsState::default();
        state.open_room = Some(room("old", vec![]));
        state.transcript = vec![message(1, "stale")];

        let gen_id = state.begin_open("new".to_string());

        assert_eq!(state.open_key.as_deref(), Some("new"));
        assert!(state.open_room.is_none());
        assert!(state.transcript.is_empty());
        assert_eq!(gen_id, state.generation);
        assert!(state.status.contains("loading"));
    }

    #[test]
    fn apply_loaded_is_dropped_when_generation_is_stale() {
        let mut state = RoomsState::default();
        let gen_id = state.begin_open("map-fix".to_string());

        // A fast re-select bumps the generation; the older load must not land.
        let _newer = state.begin_open("other".to_string());
        let landed = state.apply_loaded(gen_id, Some(room("map-fix", vec![])), vec![message(1, "hi")]);

        assert!(!landed);
        assert!(state.transcript.is_empty());
        assert_eq!(state.open_key.as_deref(), Some("other"));
    }

    #[test]
    fn apply_loaded_lands_for_active_generation() {
        let mut state = RoomsState::default();
        let gen_id = state.begin_open("map-fix".to_string());

        let landed = state.apply_loaded(
            gen_id,
            Some(room("map-fix", vec![])),
            vec![message(1, "hi"), message(2, "there")],
        );

        assert!(landed);
        assert_eq!(state.transcript.len(), 2);
        assert!(state.status.is_empty());
    }

    #[test]
    fn append_transcript_tail_appends_only_new_seqs_for_open_room() {
        let mut state = RoomsState::default();
        state.begin_open("map-fix".to_string());
        state.apply_loaded(state.generation, Some(room("map-fix", vec![])), vec![message(1, "a")]);
        assert_eq!(state.highest_seq(), 1);

        // A tail that includes an already-held seq plus new ones.
        let appended =
            state.append_transcript_tail("map-fix", vec![message(1, "dup"), message(2, "b"), message(3, "c")]);
        assert!(appended);
        assert_eq!(state.transcript.len(), 3);
        assert_eq!(state.highest_seq(), 3);

        // A tail for a different (not-open) room is ignored.
        let appended_other = state.append_transcript_tail("other", vec![message(4, "x")]);
        assert!(!appended_other);
        assert_eq!(state.transcript.len(), 3);
    }

    #[test]
    fn close_room_retires_poll_and_clears_state() {
        let mut state = RoomsState::default();
        let gen_id = state.begin_open("map-fix".to_string());
        state.apply_loaded(gen_id, Some(room("map-fix", vec![])), vec![message(1, "a")]);

        state.close_room();

        assert!(state.open_key.is_none());
        assert!(state.open_room.is_none());
        assert!(state.transcript.is_empty());
        assert_ne!(state.generation, gen_id); // poll loop retired
    }

    #[test]
    fn can_send_requires_open_room_and_nonblank_draft() {
        let mut state = RoomsState::default();
        state.composer_draft = "hello".to_string();
        assert!(!state.can_send()); // no open room

        state.open_key = Some("map-fix".to_string());
        assert!(state.can_send());

        state.composer_draft = "   ".to_string();
        assert!(!state.can_send());
    }

    #[test]
    fn header_title_uses_open_room_name_then_falls_back() {
        let mut state = RoomsState::default();
        assert_eq!(state.header_title(), "Rooms");

        state.open_room = Some(Room {
            id: "map-fix".to_string(),
            name: "Map Fix".to_string(),
            participants: vec![],
            created_at: String::new(),
            updated_at: String::new(),
            trigger_policy: None,
        });
        assert_eq!(state.header_title(), "Map Fix");
    }

    fn room_with_policy(
        key: &str,
        participants: Vec<RoomParticipant>,
        policy: Option<RoomTriggerPolicy>,
    ) -> Room {
        Room {
            id: key.to_string(),
            name: key.to_string(),
            participants,
            created_at: String::new(),
            updated_at: String::new(),
            trigger_policy: policy,
        }
    }

    #[test]
    fn collect_trigger_policy_none_when_all_off_and_no_cron() {
        let mut state = RoomsState::default();
        // Default has on_mention=true; turn everything off → None.
        state.policy_on_mention = false;
        state.policy_on_thread_reply = false;
        state.policy_on_component_event = false;
        state.policy_on_schedule_draft = "   ".to_string();
        assert_eq!(state.collect_trigger_policy(), None);
    }

    #[test]
    fn collect_trigger_policy_builds_from_toggles_and_cron() {
        let mut state = RoomsState::default();
        state.policy_on_mention = true;
        state.policy_on_thread_reply = true;
        state.policy_on_component_event = false;
        state.policy_on_schedule_draft = "  0 9 * * *  ".to_string();
        let policy = state.collect_trigger_policy().expect("policy expected");
        assert!(policy.on_mention);
        assert!(policy.on_thread_reply);
        assert!(!policy.on_component_event);
        assert_eq!(policy.on_schedule.as_deref(), Some("0 9 * * *"));
    }

    #[test]
    fn agent_ids_lists_only_agent_participants() {
        let mut state = RoomsState::default();
        assert!(state.agent_ids().is_empty());

        state.open_key = Some("map-fix".to_string());
        state.open_room = Some(room(
            "map-fix",
            vec![
                participant("alice", RoomParticipantKind::Human),
                participant("flux", RoomParticipantKind::Agent),
                participant("knox", RoomParticipantKind::Agent),
                participant("relay", RoomParticipantKind::Bot),
            ],
        ));
        assert_eq!(state.agent_ids(), vec!["flux".to_string(), "knox".to_string()]);
    }

    #[test]
    fn can_add_agent_requires_open_room_and_nonblank_id() {
        let mut state = RoomsState::default();
        state.agent_id_draft = "flux".to_string();
        assert!(!state.can_add_agent()); // no open room

        state.open_key = Some("map-fix".to_string());
        assert!(state.can_add_agent());

        state.agent_id_draft = "   ".to_string();
        assert!(!state.can_add_agent());
    }

    #[test]
    fn insert_mention_spaces_correctly() {
        let mut state = RoomsState::default();
        state.insert_mention("flux");
        assert_eq!(state.composer_draft, "@flux ");

        // Already ends in a space → no double space.
        state.composer_draft = "hi ".to_string();
        state.insert_mention("knox");
        assert_eq!(state.composer_draft, "hi @knox ");

        // Non-empty without trailing space → a separator is added.
        state.composer_draft = "hi".to_string();
        state.insert_mention("sage");
        assert_eq!(state.composer_draft, "hi @sage ");
    }

    #[test]
    fn trigger_policy_summary_reads_open_room_policy() {
        let mut state = RoomsState::default();
        assert_eq!(state.trigger_policy_summary(), None);

        state.open_key = Some("standup".to_string());
        state.open_room = Some(room_with_policy(
            "standup",
            vec![],
            Some(RoomTriggerPolicy {
                on_mention: true,
                on_thread_reply: false,
                on_component_event: false,
                on_schedule: Some("0 9 * * *".to_string()),
            }),
        ));
        assert_eq!(
            state.trigger_policy_summary().as_deref(),
            Some("Auto-convene: @mention, schedule")
        );
    }
}
