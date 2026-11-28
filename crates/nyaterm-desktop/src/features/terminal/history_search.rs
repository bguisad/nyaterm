//! Readline recovery owns only immutable observations and a pending gesture.
//! The session's ShellEditState remains the sole mutable command buffer.

use std::time::{Duration, Instant};

use gpui::Context;
use nyaterm_core::completion::types::ShellDialect;
use nyaterm_core::terminal::editing::{
    EDIT_CONFIRMATION_TIMEOUT, EditCapability, ShellLineEditing,
};
use nyaterm_core::terminal::history_search::HistorySearchPhase;
use nyaterm_core::terminal::input_tracker::TerminalInputState;
use nyaterm_core::terminal::shell_prompt::{ShellPrompt, classify_shell_prompt};
use nyaterm_terminal::editing::history_search::{HistoryInputBoundary, HistoryInputObservation};
use nyaterm_terminal::selection::TerminalSelectionMode;
use nyaterm_terminal::{TerminalLineId, TerminalSnapshot};
use zeroize::Zeroize;

use super::editing_state::SnapshotEvidence;
use super::terminal_surface::terminal_absolute_line_for_snapshot_row;
use crate::features::NyaTermApp;
use crate::models::{SessionLaunchConfig, TerminalSelection};

#[cfg(test)]
mod tests;

pub(super) struct HistorySearchRecovery {
    boundary: HistoryInputBoundary,
    pub(super) phase: HistorySearchPhase,
    saw_search: bool,
    evidence: SnapshotEvidence,
    backend: ShellPrompt,
    pending: Option<PendingGesture>,
}

pub(super) enum HistorySearchGesture {
    Move { line: TerminalLineId, col: usize },
    Select(TerminalSelection),
}

struct PendingGesture {
    gesture: HistorySearchGesture,
    observation: HistoryInputObservation,
    deadline: Instant,
    version: u64,
    sync_revision: u64,
    settled: Option<(SnapshotEvidence, Instant)>,
    settle_task: Option<gpui::Task<()>>,
}

// Readline can emit a run of individual cursor moves, including moves over
// invisible trailing spaces. Coalesce their frames before adopting the buffer.
const CALIBRATION_SETTLE: Duration = Duration::from_millis(50);

impl HistorySearchGesture {
    fn valid_for(
        &self,
        observation: &HistoryInputObservation,
        snapshot: &TerminalSnapshot,
    ) -> bool {
        match self {
            Self::Move { line, col } => snapshot
                .rows()
                .iter()
                .position(|row| row.line_id == Some(*line))
                .and_then(|row| observation.mapping.byte_at(row, *col))
                .is_some(),
            Self::Select(selection) => {
                if selection.mode != TerminalSelectionMode::Linear || selection.all_buffer {
                    return false;
                }
                let Some((start, end)) = selection.cell_range(snapshot.cols) else {
                    return false;
                };
                let row_for = |line| {
                    (0..snapshot.row_count()).find(|&row| {
                        terminal_absolute_line_for_snapshot_row(snapshot, row) == Some(line)
                    })
                };
                row_for(start.line)
                    .zip(row_for(end.line))
                    .is_some_and(|(start_row, end_row)| {
                        observation
                            .mapping
                            .selected_range((start_row, start.col), (end_row, end.col))
                            .is_some()
                    })
            }
        }
    }
}

impl NyaTermApp {
    pub(super) fn history_search_environment_allows_editing(&self) -> bool {
        let Some(id) = self.session.active_id() else {
            return false;
        };
        self.settings.summary().interaction_free_type_enabled
            && !self.security.screen_locked()
            && !self.session.is_disconnected(id)
            && !self.active_terminal_uses_alternate_screen()
            && !self.terminal_protocol_state_for_session(id).mouse_reporting
            && !self.is_credential_prompt_input_mode()
            && self.terminal.assist.credential_suggestions.is_none()
            && !self.sync_input.may_fan_out_from(id)
            && self.active_terminal_display_offset() == 0
            && !self.mcp.capabilities.terminal_executions.capturing(id)
            && self.session.metadata(id).is_some_and(|metadata| {
                matches!(
                    metadata.launch_config,
                    SessionLaunchConfig::Local(_) | SessionLaunchConfig::Ssh(_)
                )
            })
    }

    pub(super) fn note_history_search_input(&mut self, text: &str, snapshot: &TerminalSnapshot) {
        let allowed = self.history_search_environment_allows_editing();
        let supported = self
            .session
            .active_id()
            .and_then(|id| self.commands.command_context(id))
            .is_none_or(|context| {
                matches!(context.dialect, ShellDialect::Bash | ShellDialect::Unknown)
            });
        let suppressed = self.terminal.assist.commands.suppression.is_suppressed();
        let state = self.terminal.editing.state_mut();
        if let Some(mut recovery) = state.history_search.take() {
            if allowed && recovery.phase.note_input(text) {
                recovery.evidence = SnapshotEvidence::from_snapshot(snapshot);
                state.history_search = Some(recovery);
            }
        } else if text == "\x12"
            && allowed
            && supported
            && (state.allows_editing() || (!suppressed && state.model.input.value.is_empty()))
            && let Some(boundary) =
                HistoryInputBoundary::capture(snapshot, state.line_editor_prompt.as_deref())
            && let Some(backend) = state
                .editor_evidence
                .or_else(|| classify_shell_prompt(boundary.prompt()))
            && backend.line_editing != ShellLineEditing::LegacyConsole
        {
            state.history_search = Some(HistorySearchRecovery {
                boundary,
                phase: HistorySearchPhase::Searching,
                saw_search: false,
                evidence: SnapshotEvidence::from_snapshot(snapshot),
                backend,
                pending: None,
            });
        }
        if state.history_search.is_some() {
            state.browsing_history = true;
        }
    }

    /// Returns true while normal reconciliation must not consume search text or
    /// partially calibrated input. A completed recovery resumes ordinary checks.
    pub(super) fn reconcile_history_search(
        &mut self,
        snapshot: &TerminalSnapshot,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut recovery) = self.terminal.editing.state_mut().history_search.take() else {
            return false;
        };
        if !self.history_search_environment_allows_editing()
            || !recovery.boundary.same_geometry(snapshot)
        {
            return true;
        }
        let state = self.terminal.editing.state_mut();
        if state.awaiting_snapshot {
            state.history_search = Some(recovery);
            return true;
        }
        if recovery.pending.as_ref().is_some_and(|pending| {
            Instant::now() >= pending.deadline
                || pending.version != state.model.version
                || pending.sync_revision != self.sync_input.revision()
        }) {
            return true;
        }
        let evidence = SnapshotEvidence::from_snapshot(snapshot);
        if evidence == recovery.evidence {
            state.history_search = Some(recovery);
            return true;
        }
        if recovery.boundary.is_search_display(snapshot) {
            recovery.saw_search = true;
            // The search echo may arrive after an exit key has already been sent.
            if recovery.phase == HistorySearchPhase::Ready {
                recovery.phase = HistorySearchPhase::Searching;
            }
            if recovery.pending.is_some() {
                return true;
            }
            recovery.evidence = evidence;
            state.history_search = Some(recovery);
            return true;
        }
        match recovery.phase {
            HistorySearchPhase::Searching | HistorySearchPhase::AwaitingPrompt => {
                if recovery.saw_search && recovery.boundary.observe(snapshot, false).is_some() {
                    recovery.phase = HistorySearchPhase::Ready;
                    recovery.evidence = evidence;
                }
                state.history_search = Some(recovery);
                true
            }
            HistorySearchPhase::Ready => {
                // Cursor-only navigation after exit does not change the recalled
                // buffer. Revalidate the prompt before accepting a later gesture.
                if recovery.evidence.same_content(&evidence)
                    && recovery.boundary.observe(snapshot, false).is_some()
                {
                    recovery.evidence = evidence;
                    state.history_search = Some(recovery);
                }
                true
            }
            HistorySearchPhase::MovingToStart | HistorySearchPhase::MovingToEnd => {
                let Some(observation) = recovery
                    .boundary
                    .observe(snapshot, recovery.phase == HistorySearchPhase::MovingToEnd)
                else {
                    if let Some(pending) = recovery.pending.as_mut() {
                        pending.settled = None;
                        pending.settle_task = None;
                    }
                    state.history_search = Some(recovery);
                    return true;
                };
                let pending = recovery
                    .pending
                    .as_ref()
                    .expect("calibration owns a gesture");
                if observation.visible.trim_end_matches(' ')
                    != pending.observation.visible.trim_end_matches(' ')
                    || !observation
                        .mapping
                        .cells
                        .starts_with(&pending.observation.mapping.cells)
                {
                    // Moving to the start can hide trailing spaces that were
                    // visible up to the original cursor. Compare the common cells.
                    if recovery.phase != HistorySearchPhase::MovingToStart
                        || observation.visible.trim_end_matches(' ')
                            != pending.observation.visible.trim_end_matches(' ')
                        || !pending
                            .observation
                            .mapping
                            .cells
                            .starts_with(&observation.mapping.cells)
                    {
                        return true;
                    }
                }
                if recovery.phase == HistorySearchPhase::MovingToStart {
                    if observation.cursor != 0 {
                        state.history_search = Some(recovery);
                        return true;
                    }
                    recovery.phase = HistorySearchPhase::MovingToEnd;
                    recovery.evidence = evidence;
                    state.history_search = Some(recovery);
                    self.send_history_calibration_key(b"\x05", cx);
                    return true;
                }
                let pending = recovery.pending.as_mut().unwrap();
                if !pending.settled.as_ref().is_some_and(|(previous, since)| {
                    *previous == evidence && since.elapsed() >= CALIBRATION_SETTLE
                }) {
                    if pending
                        .settled
                        .as_ref()
                        .is_none_or(|(previous, _)| *previous != evidence)
                    {
                        pending.settled = Some((evidence, Instant::now()));
                        let id = self.session.active_id_owned().unwrap();
                        let confirmation_id = state.confirmation_id;
                        pending.settle_task = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(CALIBRATION_SETTLE).await;
                            let _ =
                                this.update(cx, |this, cx| {
                                    if this.terminal.editing.sessions.get(&id).is_some_and(
                                        |state| state.confirmation_id == confirmation_id,
                                    ) {
                                        this.reconcile_shell_editing(&id, cx);
                                    }
                                });
                        }));
                    }
                    state.history_search = Some(recovery);
                    return true;
                }
                let pending = recovery.pending.take().unwrap();
                if !pending.gesture.valid_for(&observation, snapshot) {
                    return true;
                }
                state.model.input.value.zeroize();
                state.model.input = TerminalInputState {
                    value: observation.visible.to_string(),
                    cursor: observation.cursor,
                    ..TerminalInputState::default()
                };
                state.model.version = state.model.version.wrapping_add(1);
                state.model.confirm();
                state.mapping = Some(observation.mapping);
                state.capability = if snapshot.shell_input_anchor.is_some() {
                    EditCapability::MarkedShell
                } else {
                    EditCapability::EchoMatched
                };
                state.echo_line_editor = true;
                state.line_editor_prompt = Some(recovery.boundary.prompt().to_owned());
                state.editor_evidence = Some(recovery.backend);
                state.pending_evidence = None;
                state.confirmation_task = None;
                match pending.gesture {
                    HistorySearchGesture::Move { line, col } => {
                        if let Some(row) = snapshot
                            .rows()
                            .iter()
                            .position(|row| row.line_id == Some(line))
                            && let Some(target) = state
                                .mapping
                                .as_ref()
                                .and_then(|mapping| mapping.byte_at(row, col))
                        {
                            self.move_smart_input_cursor(target, cx);
                        }
                    }
                    HistorySearchGesture::Select(selection) => {
                        state.selection_origin_version = Some(state.model.version);
                        state.selection = Some((selection, state.model.version));
                    }
                }
                cx.notify();
                // A cursor move may now be Pending. Do not reconcile it against
                // the snapshot from before that move in this same callback.
                true
            }
        }
    }

    pub(super) fn history_search_ready_for_gesture(&self) -> bool {
        self.history_search_environment_allows_editing()
            && self.terminal.editing.state().is_some_and(|state| {
                !state.awaiting_snapshot
                    && state.history_search.as_ref().is_some_and(|recovery| {
                        matches!(
                            recovery.phase,
                            HistorySearchPhase::Ready
                                | HistorySearchPhase::MovingToStart
                                | HistorySearchPhase::MovingToEnd
                        )
                    })
            })
    }

    pub(super) fn start_history_search_gesture(
        &mut self,
        gesture: HistorySearchGesture,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.history_search_ready_for_gesture() {
            return false;
        }
        let Some(id) = self.session.active_id_owned() else {
            return false;
        };
        let snapshot = self.terminal_snapshot_for_session(Some(&id), 0);
        let sync_revision = self.sync_input.revision();
        let state = self.terminal.editing.state_mut();
        let recovery = state.history_search.as_mut().unwrap();
        if let Some(pending) = recovery.pending.as_mut() {
            if gesture.valid_for(&pending.observation, &snapshot) {
                pending.gesture = gesture;
            }
            return true;
        }
        if recovery.evidence != SnapshotEvidence::from_snapshot(&snapshot) {
            return false;
        }
        let Some(observation) = recovery.boundary.observe(&snapshot, false) else {
            return false;
        };
        if observation.visible.is_empty() || !gesture.valid_for(&observation, &snapshot) {
            return false;
        }
        let key = if observation.cursor == 0 {
            recovery.phase = HistorySearchPhase::MovingToEnd;
            b"\x05"
        } else {
            recovery.phase = HistorySearchPhase::MovingToStart;
            b"\x01"
        };
        recovery.pending = Some(PendingGesture {
            gesture,
            observation,
            deadline: Instant::now() + EDIT_CONFIRMATION_TIMEOUT,
            version: state.model.version,
            sync_revision,
            settled: None,
            settle_task: None,
        });
        if !self.send_history_calibration_key(key, cx) {
            return true;
        }
        let confirmation_id = self.terminal.editing.allocate_confirmation_id();
        self.terminal.editing.state_mut().confirmation_id = confirmation_id;
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(EDIT_CONFIRMATION_TIMEOUT)
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(state) = this.terminal.editing.sessions.get_mut(&id)
                    && state.confirmation_id == confirmation_id
                    && state
                        .history_search
                        .as_ref()
                        .is_some_and(|recovery| recovery.pending.is_some())
                {
                    state.invalidate();
                    cx.notify();
                }
            });
        });
        self.terminal.editing.state_mut().confirmation_task = Some(task);
        true
    }

    fn send_history_calibration_key(&mut self, key: &[u8], cx: &mut Context<Self>) -> bool {
        let Some(id) = self.session.active_id_owned() else {
            return false;
        };
        self.terminal.view.frame_pipeline.arm_output_event_wake();
        if let Err(error) = self.write_session_wire_input_recorded_as(&id, key, key) {
            self.terminal.editing.state_mut().invalidate();
            self.set_terminal_status_if_changed(format!("input failed: {error}"));
            cx.notify();
            return false;
        }
        self.dismiss_command_suggestions(cx);
        self.arm_terminal_input_wake(cx);
        true
    }
}
