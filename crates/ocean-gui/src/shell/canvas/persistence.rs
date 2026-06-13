//! Local-first persistence for the native [`CanvasLedger`] (OCEAN-167,
//! GPUI Masterbuild §12 "Persistence And Sync").
//!
//! Before this slice the canvas was purely in-memory: every patch lived in the
//! shared `canvas_ledger` cell and died with the process. A restart lost all
//! canvas state. This module makes canvas state survive restart, local-first,
//! with no database and no CRDT (§12: "Do not add a database until query
//! patterns require it"; §17: keep patch ops deterministic so a future Loro
//! migration stays open, but defer it).
//!
//! # On-disk layout (§12)
//!
//! ```text
//! ~/.ocean/surfaces/<session_id>/canvas/<canvas_id>.json          (snapshot)
//! ~/.ocean/surfaces/<session_id>/canvas/<canvas_id>.patches.jsonl (append log)
//! ```
//!
//! - The **snapshot** is the full [`CanvasLedger`] serialized as pretty JSON. It
//!   is the fast-load baseline: loading it alone reconstructs the entire canvas.
//! - The **patch log** is append-only JSON-lines: one [`SurfacePatchEnvelope`]
//!   per line, in the order applied. It is the durable tail of mutations that
//!   landed *after* the last snapshot. Keeping the raw envelopes (not just the
//!   final state) preserves deterministic replay — the foundation a future CRDT
//!   needs (§17 decision test: "Can the patch log replay deterministically?").
//!
//! # Snapshot cadence and log rotation
//!
//! Writing a full snapshot on every single patch would be wasteful for a hot,
//! agent-driven canvas; never snapshotting would let the `.jsonl` grow forever
//! and make load slow. So the cadence is **bounded and deterministic**:
//!
//! - Every applied patch is appended to the `.jsonl` immediately (durability).
//! - Every [`SNAPSHOT_EVERY_N_PATCHES`] patches (a clean revision boundary,
//!   i.e. `revision % N == 0`) we rewrite the snapshot to the ledger's current
//!   state and **truncate** the `.jsonl` to empty. The snapshot now subsumes
//!   those patches, so the log restarts from zero — bounded growth, never more
//!   than `N - 1` un-snapshotted patches on disk at any time.
//!
//! On load we read the snapshot, then replay only the `.jsonl` envelopes whose
//! resulting revision is *past* the snapshot's `revision` (the snapshot may be
//! newer than the log if the process died between the snapshot write and the
//! log truncation, so we never double-apply).
//!
//! # Graceful degradation
//!
//! Persistence is best-effort and must never break the canvas. A missing,
//! unreadable, or corrupt snapshot/log degrades to "start empty" (for load) or
//! "skip this write" (for save), logging a warning to stderr. Nothing here ever
//! panics on bad I/O or bad JSON.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use super::ledger::CanvasLedger;
use super::patch::{CanvasId, LamportClock, SurfacePatchEnvelope};

/// Snapshot cadence: rewrite the full snapshot and truncate the patch log every
/// time the ledger revision crosses a multiple of this value. Chosen as a small
/// power-of-two-ish bound: frequent enough that the `.jsonl` tail stays tiny and
/// load stays fast, infrequent enough that a burst of patches doesn't rewrite
/// the whole snapshot on every keystroke.
pub const SNAPSHOT_EVERY_N_PATCHES: u64 = 16;

/// Filesystem layout resolver for one canvas's persisted state.
///
/// All paths derive from a `root` (normally `~/.ocean`), the session id, and the
/// canvas id. Session/canvas ids can contain wire characters that are illegal or
/// awkward in path components (`:` in `canvas:main`, `/`, …), so each segment is
/// sanitized before it becomes a directory/file name.
#[derive(Debug, Clone)]
pub struct CanvasStore {
    canvas_dir: PathBuf,
    canvas_id_file_stem: String,
}

impl CanvasStore {
    /// Build a store rooted at an explicit directory (used by tests with a temp
    /// dir). Production code uses [`CanvasStore::for_session`].
    pub fn with_root(root: impl AsRef<Path>, session_id: &str, canvas_id: &CanvasId) -> Self {
        let canvas_dir = root
            .as_ref()
            .join("surfaces")
            .join(sanitize_segment(session_id))
            .join("canvas");
        Self {
            canvas_dir,
            canvas_id_file_stem: sanitize_segment(canvas_id.as_str()),
        }
    }

    /// Build a store under the real home dir: `~/.ocean/surfaces/...`.
    ///
    /// Returns `None` if no home directory can be resolved (then persistence is
    /// silently disabled rather than writing to a surprising location). Follows
    /// the existing `HOME`-env convention used elsewhere in this crate.
    pub fn for_session(session_id: &str, canvas_id: &CanvasId) -> Option<Self> {
        let home = ocean_root()?;
        Some(Self::with_root(home, session_id, canvas_id))
    }

    /// `<canvas_id>.json` — the full-ledger snapshot.
    pub fn snapshot_path(&self) -> PathBuf {
        self.canvas_dir
            .join(format!("{}.json", self.canvas_id_file_stem))
    }

    /// `<canvas_id>.patches.jsonl` — the append-only patch log.
    pub fn patch_log_path(&self) -> PathBuf {
        self.canvas_dir
            .join(format!("{}.patches.jsonl", self.canvas_id_file_stem))
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.canvas_dir)
    }

    // -----------------------------------------------------------------------
    // Save
    // -----------------------------------------------------------------------

    /// Persist the result of applying `newly_applied` patches that produced the
    /// current `ledger` state.
    ///
    /// Contract (caller applies patches to the ledger first, then calls this):
    /// - Always append `newly_applied` to the `.jsonl` log (durability of the
    ///   tail past the last snapshot).
    /// - If the ledger has crossed a snapshot boundary
    ///   ([`SNAPSHOT_EVERY_N_PATCHES`]), rewrite the snapshot and truncate the
    ///   log (bounded growth).
    ///
    /// Best-effort: I/O or serialization errors are logged and swallowed so a
    /// failed write never disrupts the live canvas.
    pub fn persist(&self, ledger: &CanvasLedger, newly_applied: &[SurfacePatchEnvelope]) {
        if let Err(err) = self.persist_inner(ledger, newly_applied) {
            eprintln!(
                "[canvas-persist] warning: failed to persist canvas {} (session {}): {err}",
                ledger.canvas_id, ledger.session_id
            );
        }
    }

    fn persist_inner(
        &self,
        ledger: &CanvasLedger,
        newly_applied: &[SurfacePatchEnvelope],
    ) -> std::io::Result<()> {
        self.ensure_dir()?;

        // A boundary crossing snapshots + truncates; otherwise just append.
        // revision == 0 means nothing was applied yet — nothing to do.
        let at_boundary = ledger.revision > 0 && ledger.revision % SNAPSHOT_EVERY_N_PATCHES == 0;

        if at_boundary {
            // Crash-safety contract: the durable on-disk state (snapshot + log)
            // must reconstruct through the *current* applied revision at every
            // instant — there must be no window where the boundary patch exists
            // only in memory. So the order is strict:
            //
            //   1. append the boundary patch(es) to the log (+ flush), so the log
            //      already covers through the current revision;
            //   2. write the new snapshot atomically (temp + rename);
            //   3. only then truncate the log.
            //
            // If a crash happens after (1) but before/during (2), load recovers
            // the boundary patch from the log on top of the old snapshot. If a
            // crash happens after (2) but before (3), the boundary patch is in
            // both the snapshot and the un-truncated log tail — harmless, because
            // replay dedupes by revision (`<canvas_id>@<revision>`).
            self.append_patches(newly_applied)?;
            self.write_snapshot(ledger)?;
            // The snapshot now subsumes every prior patch; restart the log.
            self.truncate_log()?;
        } else {
            self.append_patches(newly_applied)?;
            // Ensure a baseline snapshot exists even before the first boundary,
            // so a load before patch #N still finds a snapshot to anchor on.
            if !self.snapshot_path().exists() {
                self.write_snapshot(ledger)?;
            }
        }
        Ok(())
    }

    /// Write the full snapshot atomically-ish (write temp, rename) so a crash
    /// mid-write can't leave a half-written, unparseable snapshot.
    fn write_snapshot(&self, ledger: &CanvasLedger) -> std::io::Result<()> {
        self.ensure_dir()?;
        let json = serde_json::to_string_pretty(ledger)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let final_path = self.snapshot_path();
        let tmp_path = final_path.with_extension("json.tmp");
        {
            let mut f = File::create(&tmp_path)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp_path, &final_path)
    }

    fn append_patches(&self, patches: &[SurfacePatchEnvelope]) -> std::io::Result<()> {
        if patches.is_empty() {
            return Ok(());
        }
        self.ensure_dir()?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.patch_log_path())?;
        for env in patches {
            let line = serde_json::to_string(env)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            f.write_all(line.as_bytes())?;
            f.write_all(b"\n")?;
        }
        f.flush()?;
        // fsync the appended lines to durable storage before any later snapshot
        // write/rename — the crash-safety contract relies on the log already
        // covering the current revision at the moment the snapshot is written.
        f.sync_all()
    }

    fn truncate_log(&self) -> std::io::Result<()> {
        let path = self.patch_log_path();
        if path.exists() {
            // Truncate to empty (keep the file so appends continue cheaply).
            File::create(path)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Load
    // -----------------------------------------------------------------------

    /// Load the persisted canvas: read the snapshot, then replay any patch-log
    /// entries newer than the snapshot revision.
    ///
    /// Returns `None` when there is nothing persisted (no snapshot and no log),
    /// or when everything on disk is unreadable/corrupt — in every degraded case
    /// the caller starts from an empty canvas. Never panics.
    pub fn load(&self) -> Option<CanvasLedger> {
        let snapshot = self.load_snapshot();
        let log = self.load_patch_log();

        let mut ledger = match (snapshot, log) {
            (Some(mut ledger), entries) => {
                replay_newer(&mut ledger, entries);
                ledger
            }
            // No snapshot but we have log entries (snapshot lost/corrupt): rebuild
            // from the log alone by replaying every entry onto an empty ledger.
            (None, entries) if !entries.is_empty() => {
                let first = &entries[0];
                let mut ledger = CanvasLedger::new(
                    first.canvas_id.clone(),
                    first.session_id.clone(),
                    super::ledger::CanvasMode::default(),
                );
                replay_newer(&mut ledger, entries);
                ledger
            }
            _ => return None,
        };

        // Seed the Lamport clock past every revision in the replayed history so a
        // fresh local (operator) edit after resume is strictly greater than
        // anything already on disk (OCEAN-270). Replaying versioned entries
        // already advances the clock via `observe`; this also covers a snapshot
        // with no log tail and legacy (pre-version) snapshots whose merge_state
        // was reconstructed from carried versions.
        let seed = ledger.merge_state.max_rev().max(ledger.clock.now());
        ledger.clock = LamportClock::at(seed);
        Some(ledger)
    }

    /// Read + parse the snapshot. Missing → `None` (normal first run). Present but
    /// corrupt → `None` + warning (degrade to empty, never panic).
    fn load_snapshot(&self) -> Option<CanvasLedger> {
        let path = self.snapshot_path();
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                eprintln!(
                    "[canvas-persist] warning: cannot read snapshot {}: {err}",
                    path.display()
                );
                return None;
            }
        };
        match serde_json::from_slice::<CanvasLedger>(&bytes) {
            Ok(ledger) => Some(ledger),
            Err(err) => {
                eprintln!(
                    "[canvas-persist] warning: corrupt snapshot {} ({err}); starting empty",
                    path.display()
                );
                None
            }
        }
    }

    /// Read + parse the patch log. Missing → empty vec. Individual corrupt lines
    /// are skipped (with a warning) rather than discarding the whole log.
    fn load_patch_log(&self) -> Vec<SurfacePatchEnvelope> {
        let path = self.patch_log_path();
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(err) => {
                eprintln!(
                    "[canvas-persist] warning: cannot read patch log {}: {err}",
                    path.display()
                );
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for (idx, line) in BufReader::new(file).lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(err) => {
                    eprintln!(
                        "[canvas-persist] warning: unreadable line {} in {}: {err}",
                        idx + 1,
                        path.display()
                    );
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<SurfacePatchEnvelope>(&line) {
                Ok(env) => out.push(env),
                Err(err) => {
                    eprintln!(
                        "[canvas-persist] warning: corrupt patch-log line {} in {} ({err}); skipping",
                        idx + 1,
                        path.display()
                    );
                }
            }
        }
        out
    }
}

/// Replay the log entries that land *past* the ledger's current revision.
///
/// Every envelope `CanvasLedger::apply_patch` records carries a self-describing
/// patch id of the form `<canvas_id>@<revision>`, so each log line names the
/// exact revision it produced. That makes replay deterministic and idempotent
/// regardless of how the snapshot and log line up:
///
/// - Normal case: the log was truncated at the last snapshot, so every entry's
///   revision is `> snapshot_revision` and all of them get applied.
/// - Crash window (snapshot written, truncation not completed): the log still
///   holds the pre-snapshot tail. Those leading entries have revisions
///   `<= snapshot_revision` and are skipped — no double-apply.
///
/// Entries are applied strictly in revision order; their carried revision must
/// be exactly `ledger.revision + 1` at apply time (which holds because the log
/// is an ordered, gap-free suffix of history).
///
/// Replay goes through [`CanvasLedger::apply_remote_patch`] (OCEAN-270), not the
/// local-edit path: a logged envelope already carries the [`ComponentVersion`]
/// the merge stamped when it was first applied, so replaying it must **reuse**
/// that version (folding it back into the clock + merge state) rather than mint a
/// fresh one. Treating the persisted history as "remote" keeps resume
/// version-preserving and idempotent. After replay the caller seeds the clock
/// past the whole replayed history (see [`CanvasStore::load`]).
fn replay_newer(ledger: &mut CanvasLedger, entries: Vec<SurfacePatchEnvelope>) {
    for env in entries {
        // Skip anything the current ledger state already includes.
        if let Some(rev) = revision_from_patch_id(env.patch_id.as_str()) {
            if rev <= ledger.revision {
                continue;
            }
        }
        ledger.apply_remote_patch(env);
    }
}

/// Extract the trailing `<revision>` from a `CanvasLedger`-minted patch id of the
/// form `<canvas_id>@<revision>` (see [`CanvasLedger::apply_patch`]). Returns
/// `None` for any id that doesn't carry a parseable `@<number>` suffix.
fn revision_from_patch_id(patch_id: &str) -> Option<u64> {
    patch_id.rsplit_once('@')?.1.parse::<u64>().ok()
}

/// Resolve `~/.ocean`, following the crate's existing `HOME`-env convention.
fn ocean_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".ocean"))
}

/// Make an id safe to use as a single path segment: replace anything that isn't
/// alphanumeric, `-`, `_`, or `.` with `_`. Deterministic and collision-resistant
/// enough for our id space (`canvas:main` → `canvas_main`, `sess/1` → `sess_1`).
fn sanitize_segment(s: &str) -> String {
    if s.is_empty() {
        return "_".to_string();
    }
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests (temp-dir only — never touch the real ~/.ocean)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::canvas::ledger::CanvasMode;
    use crate::shell::canvas::patch::{
        ActorRef, CanvasComponentPatch, ComponentId, PatchId, Rect, SurfaceId, SurfacePatch,
    };
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU64, Ordering};

    // Unique temp dir per test (no external tempfile dep).
    fn temp_root(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ocean-canvas-persist-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn store(root: &Path) -> CanvasStore {
        CanvasStore::with_root(root, "sess:1", &CanvasId::new("canvas:main"))
    }

    fn upsert_env(ledger: &CanvasLedger, id: &str, rev: u64) -> SurfacePatchEnvelope {
        // Build an envelope mirroring what apply_patch records, so the log lines
        // we hand back to load() match real on-disk lines.
        SurfacePatchEnvelope {
            patch_id: PatchId::new(format!("p@{rev}")),
            session_id: ledger.session_id.clone(),
            surface_id: SurfaceId::new("gpui:local"),
            canvas_id: ledger.canvas_id.clone(),
            actor: ActorRef::system(),
            created_at_ms: rev as i64,
            patch: SurfacePatch::UpsertComponent {
                component: CanvasComponentPatch {
                    id: ComponentId::new(id),
                    kind: "card".to_string(),
                    rect: Some(Rect::new(0.0, 0.0, 100.0, 100.0)),
                    z_index: None,
                    content: json!({ "title": id }),
                    metadata: Value::Null,
                },
            },
            version: None,
        }
    }

    /// Apply one upsert to the ledger and persist the produced envelope.
    fn apply_and_persist(s: &CanvasStore, ledger: &mut CanvasLedger, id: &str) {
        let patch = SurfacePatch::UpsertComponent {
            component: CanvasComponentPatch {
                id: ComponentId::new(id),
                kind: "card".to_string(),
                rect: Some(Rect::new(0.0, 0.0, 100.0, 100.0)),
                z_index: None,
                content: json!({ "title": id }),
                metadata: Value::Null,
            },
        };
        ledger.apply_patch(patch, ActorRef::system(), ledger.revision as i64);
        // The envelope apply_patch just pushed is the newly-applied one.
        let env = ledger.patch_log.last().cloned().unwrap();
        s.persist(ledger, std::slice::from_ref(&env));
    }

    #[test]
    fn mutating_writes_snapshot_and_log() {
        let root = temp_root("write");
        let s = store(&root);
        let mut ledger = CanvasLedger::new("canvas:main", "sess:1", CanvasMode::Freeform);

        apply_and_persist(&s, &mut ledger, "a");

        assert!(s.snapshot_path().exists(), "snapshot should be written");
        assert!(s.patch_log_path().exists(), "patch log should be written");

        // The log holds exactly the one applied patch.
        let log = s.load_patch_log();
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn load_reconstructs_identical_ledger() {
        let root = temp_root("load");
        let s = store(&root);
        let mut ledger = CanvasLedger::new("canvas:main", "sess:1", CanvasMode::Freeform);
        apply_and_persist(&s, &mut ledger, "a");
        apply_and_persist(&s, &mut ledger, "b");
        apply_and_persist(&s, &mut ledger, "c");

        let loaded = s.load().expect("should load a persisted ledger");
        assert_eq!(loaded.revision, ledger.revision);
        assert_eq!(loaded.components, ledger.components);
        assert_eq!(loaded.edges, ledger.edges);
        assert_eq!(loaded, ledger, "round-trip must be identical");
    }

    #[test]
    fn snapshot_plus_newer_patches_replay_correctly() {
        let root = temp_root("replay");
        let s = store(&root);
        s.ensure_dir().unwrap();
        let mut ledger = CanvasLedger::new("canvas:main", "sess:1", CanvasMode::Freeform);

        // Hand-craft the scenario: a snapshot at revision 1, plus a log that also
        // contains the rev-1 patch AND a newer rev-2 patch. replay_newer must skip
        // the already-captured rev-1 and apply only rev-2 — no double-apply.
        ledger.apply_patch(upsert_env(&ledger, "a", 1).patch, ActorRef::system(), 1);
        s.write_snapshot(&ledger).unwrap(); // snapshot @ revision 1

        // Now a log on disk with BOTH patches (rev1 already in snapshot, rev2 new).
        let env1 = upsert_env(&ledger, "a", 1);
        // Build env2 from a ledger view at the right revision.
        let mut probe = ledger.clone();
        probe.apply_patch(
            SurfacePatch::UpsertComponent {
                component: CanvasComponentPatch {
                    id: ComponentId::new("b"),
                    kind: "card".to_string(),
                    rect: Some(Rect::new(0.0, 0.0, 100.0, 100.0)),
                    z_index: None,
                    content: json!({ "title": "b" }),
                    metadata: Value::Null,
                },
            },
            ActorRef::system(),
            2,
        );
        let env2 = probe.patch_log.last().cloned().unwrap();
        s.append_patches(&[env1, env2]).unwrap();

        let loaded = s.load().expect("load");
        // rev-1 (a) from snapshot, rev-2 (b) replayed from log, no duplicate a.
        assert_eq!(loaded.revision, 2, "should end at revision 2, not 3");
        assert!(loaded.component(&ComponentId::new("a")).is_some());
        assert!(loaded.component(&ComponentId::new("b")).is_some());
        assert_eq!(loaded.components.len(), 2);
    }

    #[test]
    fn missing_files_load_to_none_no_panic() {
        let root = temp_root("missing");
        let s = store(&root);
        assert!(s.load().is_none(), "nothing persisted → None, no panic");
    }

    #[test]
    fn corrupt_snapshot_degrades_without_panic() {
        let root = temp_root("corrupt-snap");
        let s = store(&root);
        s.ensure_dir().unwrap();
        fs::write(s.snapshot_path(), b"{ this is not valid json ][").unwrap();

        // Corrupt snapshot + no log → empty (None), never panic.
        assert!(s.load().is_none());
    }

    #[test]
    fn corrupt_log_line_is_skipped_not_fatal() {
        let root = temp_root("corrupt-log");
        let s = store(&root);
        let mut ledger = CanvasLedger::new("canvas:main", "sess:1", CanvasMode::Freeform);
        apply_and_persist(&s, &mut ledger, "a");

        // Append a garbage line after the valid one.
        let mut f = OpenOptions::new()
            .append(true)
            .open(s.patch_log_path())
            .unwrap();
        f.write_all(b"{ not json\n").unwrap();
        drop(f);

        let log = s.load_patch_log();
        assert_eq!(log.len(), 1, "garbage line skipped, valid line kept");

        let loaded = s.load().expect("still loads from snapshot + good lines");
        assert!(loaded.component(&ComponentId::new("a")).is_some());
    }

    #[test]
    fn log_rotates_after_snapshot_boundary() {
        let root = temp_root("rotate");
        let s = store(&root);
        let mut ledger = CanvasLedger::new("canvas:main", "sess:1", CanvasMode::Freeform);

        // Apply exactly SNAPSHOT_EVERY_N_PATCHES patches → crosses one boundary.
        for i in 0..SNAPSHOT_EVERY_N_PATCHES {
            apply_and_persist(&s, &mut ledger, &format!("c{i}"));
        }
        assert_eq!(ledger.revision, SNAPSHOT_EVERY_N_PATCHES);

        // At the boundary the log was truncated to empty and the snapshot rewritten.
        let log_after = s.load_patch_log();
        assert!(
            log_after.is_empty(),
            "log should be truncated at snapshot boundary, got {} lines",
            log_after.len()
        );

        // Loading still reconstructs the full ledger from the fresh snapshot alone.
        let loaded = s.load().expect("load after rotation");
        assert_eq!(loaded.revision, SNAPSHOT_EVERY_N_PATCHES);
        assert_eq!(loaded.components.len() as u64, SNAPSHOT_EVERY_N_PATCHES);
        assert_eq!(loaded, ledger);

        // One more patch past the boundary appends to the now-empty log.
        apply_and_persist(&s, &mut ledger, "after");
        let log_next = s.load_patch_log();
        assert_eq!(
            log_next.len(),
            1,
            "post-boundary patch appended to fresh log"
        );
        assert_eq!(s.load().unwrap(), ledger);
    }

    /// Crash-safety regression (P1 from PR #41 review): at a snapshot boundary the
    /// boundary patch must be appended to the log BEFORE the snapshot is written,
    /// so a crash after the append but before/during the snapshot write still
    /// recovers the boundary patch from the log on top of the prior snapshot —
    /// the patch is never lost to "applied in memory only".
    #[test]
    fn boundary_patch_recovered_when_snapshot_write_crashes() {
        let root = temp_root("crash-boundary");
        let s = store(&root);
        let mut ledger = CanvasLedger::new("canvas:main", "sess:1", CanvasMode::Freeform);

        // Apply and persist the first N-1 patches normally. These leave the
        // baseline snapshot at revision 1 and the log holding revisions 1..N-1.
        for i in 0..(SNAPSHOT_EVERY_N_PATCHES - 1) {
            apply_and_persist(&s, &mut ledger, &format!("c{i}"));
        }
        assert_eq!(ledger.revision, SNAPSHOT_EVERY_N_PATCHES - 1);

        // The Nth patch (the boundary patch) is applied in memory...
        let patch = SurfacePatch::UpsertComponent {
            component: CanvasComponentPatch {
                id: ComponentId::new("boundary"),
                kind: "card".to_string(),
                rect: Some(Rect::new(0.0, 0.0, 100.0, 100.0)),
                z_index: None,
                content: json!({ "title": "boundary" }),
                metadata: Value::Null,
            },
        };
        ledger.apply_patch(patch, ActorRef::system(), ledger.revision as i64);
        assert_eq!(ledger.revision, SNAPSHOT_EVERY_N_PATCHES);
        let boundary_env = ledger.patch_log.last().cloned().unwrap();

        // ...and the durable boundary write begins: APPEND happens (step 1 of the
        // contract), then the process "crashes" — write_snapshot + truncate never
        // run. We reproduce exactly that partial state by calling append_patches
        // alone, the same first step persist_inner performs at a boundary.
        s.append_patches(std::slice::from_ref(&boundary_env))
            .unwrap();

        // The snapshot on disk is still the pre-boundary baseline (revision 1),
        // proving we did not reach the snapshot write.
        let on_disk_snapshot = s.load_snapshot().expect("baseline snapshot present");
        assert!(
            on_disk_snapshot.revision < SNAPSHOT_EVERY_N_PATCHES,
            "snapshot must still be pre-boundary (crash happened before its write)"
        );

        // Recovery: load (snapshot + log replay) MUST reconstruct through the
        // boundary revision — the boundary patch is recovered, not lost.
        let recovered = s.load().expect("load after simulated boundary crash");
        assert_eq!(
            recovered.revision, SNAPSHOT_EVERY_N_PATCHES,
            "recovered ledger must reach the boundary revision, not N-1"
        );
        assert!(
            recovered.component(&ComponentId::new("boundary")).is_some(),
            "the boundary patch must survive a crash before the snapshot write"
        );
        assert_eq!(recovered, ledger, "full state recovered identically");
    }

    #[test]
    fn sanitizes_ids_into_safe_path_segments() {
        let s = CanvasStore::with_root(
            "/tmp/root",
            "sess/with:colon",
            &CanvasId::new("canvas:main"),
        );
        let snap = s.snapshot_path();
        let snap_str = snap.to_string_lossy();
        assert!(snap_str.contains("sess_with_colon"));
        assert!(snap_str.ends_with("canvas_main.json"));
        assert!(!snap_str.contains(':'), "colons must be sanitized out");
    }
}
