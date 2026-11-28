use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, TestAppContext};
use nyaterm_core::terminal::editing::EditPhase;
use nyaterm_core::terminal::history_search::HistorySearchPhase;
use nyaterm_terminal::protocol::TerminalProtocolState;
use nyaterm_transport::TelnetSessionConfig;

use super::HistorySearchGesture;
use crate::features::NyaTermApp;
use crate::features::test_support::app_with_visible_local_session;
use crate::models::{TerminalBufferCellPos, TerminalSelection};
use crate::test_support::{TestAppCleanup, TestConfigDir};

fn paint(
    app: &mut NyaTermApp,
    id: &str,
    transcript: &mut String,
    data: &str,
    cx: &mut Context<NyaTermApp>,
) {
    if transcript.is_empty() {
        app.terminal
            .seed_session_view(id.into(), String::new(), "UTF-8");
    }
    transcript.push_str(data);
    let view = app.terminal.view.views.get_mut(id).unwrap();
    view.output.push_str(data);
    view.screen.advance(data.as_bytes());
    view.protocol_state = TerminalProtocolState::from_screen(&view.screen);
    view.frame_snapshot = Some(std::sync::Arc::new(view.screen.snapshot()));
    app.reconcile_shell_editing(id, cx);
}

fn recall(
    app: &mut NyaTermApp,
    id: &str,
    marked: bool,
    exit: &[u8],
    cx: &mut Context<NyaTermApp>,
) -> String {
    let mut transcript = String::new();
    paint(
        app,
        id,
        &mut transcript,
        if marked { "$ \x1b]133;B\x07" } else { "$ " },
        cx,
    );
    app.note_shell_editing_input(b"\x12", cx);
    app.note_shell_editing_input(b"test", cx);
    paint(
        app,
        id,
        &mut transcript,
        "\r(reverse-i-search)`test': echo test tail  \x1b[K",
        cx,
    );
    assert!(!app.can_use_smart_cursor_selection());
    assert!(!app.history_search_ready_for_gesture());
    assert!(app.terminal.editing.predicted_input().value.is_empty());
    assert!(app.terminal.assist.commands.pending_history_entry.is_none());
    assert!(app.terminal.editing.state().unwrap().browsing_history);
    app.note_shell_editing_input(exit, cx);
    // A cached search snapshot is not evidence of exit.
    app.reconcile_shell_editing(id, cx);
    assert!(!app.history_search_ready_for_gesture());
    paint(
        app,
        id,
        &mut transcript,
        "\r$ echo test tail  \x1b[K\r\x1b[7C",
        cx,
    );
    assert!(app.history_search_ready_for_gesture());
    assert!(!app.can_use_smart_cursor_selection());
    transcript
}

fn read_exact(peer: &mut TcpStream, expected: &[u8]) {
    let mut actual = vec![0; expected.len()];
    peer.read_exact(&mut actual).unwrap();
    assert_eq!(actual, expected);
}

fn settle(app: &mut NyaTermApp, id: &str, cx: &mut Context<NyaTermApp>) {
    assert!(!app.can_use_smart_cursor_selection());
    let pending = app
        .terminal
        .editing
        .state_mut()
        .history_search
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
        .unwrap();
    pending.settled.as_mut().unwrap().1 = Instant::now() - super::CALIBRATION_SETTLE;
    app.reconcile_shell_editing(id, cx);
}

#[test]
fn recall_calibration_replays_selection_and_preserves_unselected_text_and_spaces() {
    for marked in [false, true] {
        let dir = TestConfigDir::new("nyaterm-search-selection");
        let mut cx = TestAppContext::single();
        let _cleanup = TestAppCleanup::new(&cx);
        let app = app_with_visible_local_session(&mut cx, dir.path(), "fixture");
        let manager = cx.update_entity(&app, |app, _| app.session.manager_handle());
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let info = manager
            .create_telnet_session(TelnetSessionConfig {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                raw_tcp: true,
                ..TelnetSessionConfig::default()
            })
            .unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        cx.update_entity(&app, |app, cx| {
            let metadata = app.session.metadata("fixture").unwrap().clone();
            app.session.register_session_metadata(&info.id, metadata);
            app.session.select_active_session(&info.id);
            let id = &info.id;
            let mut transcript = recall(app, id, marked, b"\x1b", cx);
            let selection = TerminalSelection::from_range(
                TerminalBufferCellPos::new(0, 7),
                TerminalBufferCellPos::new(0, 10),
            );
            app.terminal.selection.session_id = Some(id.clone());
            app.terminal.selection.selection = Some(selection);
            app.begin_smart_input_selection();
            app.commit_smart_input_selection(cx);
            read_exact(&mut peer, b"\x01");
            // Repeated commits retain the latest gesture without another probe.
            app.commit_smart_input_selection(cx);
            app.reconcile_shell_editing(id, cx);
            assert!(!app.can_use_smart_cursor_selection());
            paint(app, id, &mut transcript, "\r\x1b[2C", cx);
            read_exact(&mut peer, b"\x05");
            // Cursor in the visible suffix is still an incomplete acknowledgement.
            paint(app, id, &mut transcript, "\x1b[5C", cx);
            assert!(!app.can_use_smart_cursor_selection());
            paint(app, id, &mut transcript, "\x1b[9C", cx);
            assert!(!app.can_use_smart_cursor_selection());
            paint(app, id, &mut transcript, "\x1b[2C", cx);
            settle(app, id, cx);
            assert!(app.can_use_smart_cursor_selection());
            assert_eq!(
                app.terminal.editing.predicted_input().value,
                "echo test tail  "
            );
            assert_eq!(app.terminal.editing.predicted_input().cursor, 16);
            let range = app.smart_cursor_selected_input_range().unwrap();
            assert_eq!((range.start, range.end), (5, 9));
            assert!(app.replace_smart_input_selection(range, "X", cx));
            let expected = [b"\x1b[D".repeat(7), vec![0x7f; 4], b"X".to_vec()].concat();
            read_exact(&mut peer, &expected);
            assert_eq!(
                app.terminal.editing.predicted_input().value,
                "echo X tail  "
            );
            assert!(!expected.contains(&b'\r'));
            assert!(!expected.contains(&b'\n'));
        });
        manager.close(&info.id).unwrap();
    }
}

#[test]
fn recall_click_uses_latest_target_only_after_calibration() {
    let dir = TestConfigDir::new("nyaterm-search-click");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let app = app_with_visible_local_session(&mut cx, dir.path(), "fixture");
    let manager = cx.update_entity(&app, |app, _| app.session.manager_handle());
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let info = manager
        .create_telnet_session(TelnetSessionConfig {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            raw_tcp: true,
            ..TelnetSessionConfig::default()
        })
        .unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    cx.update_entity(&app, |app, cx| {
        let metadata = app.session.metadata("fixture").unwrap().clone();
        app.session.register_session_metadata(&info.id, metadata);
        app.session.select_active_session(&info.id);
        let id = &info.id;
        let mut transcript = recall(app, id, false, b"\x1b[D", cx);
        let line = app
            .terminal_snapshot_for_session(Some(id), 0)
            .row(0)
            .unwrap()
            .line_id
            .unwrap();
        assert!(app.start_history_search_gesture(HistorySearchGesture::Move { line, col: 5 }, cx));
        assert!(app.start_history_search_gesture(HistorySearchGesture::Move { line, col: 7 }, cx));
        read_exact(&mut peer, b"\x01");
        paint(app, id, &mut transcript, "\r\x1b[2C", cx);
        read_exact(&mut peer, b"\x05");
        paint(app, id, &mut transcript, "\x1b[16C", cx);
        settle(app, id, cx);
        read_exact(&mut peer, &b"\x1b[D".repeat(11));
        assert_eq!(app.terminal.editing.predicted_input().cursor, 5);
        assert_eq!(
            app.terminal.editing.state().unwrap().model.phase,
            EditPhase::Pending
        );
        paint(app, id, &mut transcript, "\x1b[11D", cx);
        assert_eq!(
            app.terminal.editing.state().unwrap().model.phase,
            EditPhase::Ready
        );
    });
    manager.close(&info.id).unwrap();
}

#[test]
fn recovery_revokes_on_new_input_session_switch_and_write_failure() {
    for action in [
        "type",
        "enter",
        "interrupt",
        "switch",
        "write_failure",
        "mouse",
        "password",
        "broadcast",
        "disabled",
    ] {
        let dir = TestConfigDir::new("nyaterm-search-cancel");
        let mut cx = TestAppContext::single();
        let _cleanup = TestAppCleanup::new(&cx);
        let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
        cx.update_entity(&app, |app, cx| {
            let mut transcript = recall(app, "s1", false, b"\x1b", cx);
            match action {
                "type" => app.note_shell_editing_input(b"x", cx),
                "enter" => app.note_shell_editing_input(b"\r", cx),
                "interrupt" => app.note_shell_editing_input(b"\x03", cx),
                "switch" => {
                    app.terminal.editing.activate("other");
                    app.terminal.editing.activate("s1");
                }
                "write_failure" => {
                    let line = app
                        .terminal_snapshot_for_session(Some("s1"), 0)
                        .row(0)
                        .unwrap()
                        .line_id
                        .unwrap();
                    assert!(app.start_history_search_gesture(
                        HistorySearchGesture::Move { line, col: 7 },
                        cx
                    ));
                }
                "mouse" => paint(app, "s1", &mut transcript, "\x1b[?1000h", cx),
                "password" => {
                    app.terminal.assist.credential_prompt_input_until_ms = u64::MAX;
                    app.reconcile_shell_editing("s1", cx);
                }
                "broadcast" => {
                    app.sync_input.toggle_broadcast_to_all();
                    app.reconcile_shell_editing("s1", cx);
                }
                "disabled" => app.terminal.clear_free_type_interactions(),
                _ => unreachable!(),
            }
            assert!(
                app.terminal
                    .editing
                    .state()
                    .unwrap()
                    .history_search
                    .is_none(),
                "{action}"
            );
            assert!(!app.can_use_smart_cursor_selection(), "{action}");
        });
    }
}

#[test]
fn unchanged_snapshot_and_deadline_cannot_acknowledge_a_probe() {
    let dir = TestConfigDir::new("nyaterm-search-timeout");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
    cx.update_entity(&app, |app, cx| {
        recall(app, "s1", false, b"\x1b", cx);
        let snapshot = app.terminal_snapshot_for_session(Some("s1"), 0);
        let state = app.terminal.editing.state_mut();
        let recovery = state.history_search.as_mut().unwrap();
        recovery.phase = HistorySearchPhase::MovingToEnd;
        recovery.pending = Some(super::PendingGesture {
            gesture: HistorySearchGesture::Move {
                line: snapshot.row(0).unwrap().line_id.unwrap(),
                col: 7,
            },
            observation: recovery.boundary.observe(&snapshot, false).unwrap(),
            deadline: Instant::now() + Duration::from_secs(2),
            version: state.model.version,
            sync_revision: app.sync_input.revision(),
            settled: None,
            settle_task: None,
        });
        app.reconcile_shell_editing("s1", cx);
        assert!(!app.can_use_smart_cursor_selection());
        app.terminal
            .editing
            .state_mut()
            .history_search
            .as_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .deadline = Instant::now();
        app.reconcile_shell_editing("s1", cx);
        assert!(
            app.terminal
                .editing
                .state()
                .unwrap()
                .history_search
                .is_none()
        );
        assert!(!app.can_use_smart_cursor_selection());
    });
}

#[test]
fn failed_search_cancel_and_submission_never_learn_search_queries() {
    for exit in [b"\x07".as_slice(), b"\r".as_slice(), b"\x03".as_slice()] {
        let dir = TestConfigDir::new("nyaterm-search-failed");
        let mut cx = TestAppContext::single();
        let _cleanup = TestAppCleanup::new(&cx);
        let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
        cx.update_entity(&app, |app, cx| {
            let mut transcript = String::new();
            paint(app, "s1", &mut transcript, "$ \x1b]133;B\x07", cx);
            app.note_shell_editing_input(b"\x12", cx);
            app.note_shell_editing_input(b"missing", cx);
            paint(
                app,
                "s1",
                &mut transcript,
                "\r(failed reverse-i-search)`missing': \x1b[K",
                cx,
            );
            app.note_shell_editing_input(b"\x12", cx);
            assert!(!app.can_use_smart_cursor_selection());
            app.note_shell_editing_input(exit, cx);
            paint(app, "s1", &mut transcript, "\r$ \x1b[K", cx);
            assert!(app.terminal.editing.predicted_input().value.is_empty());
            assert!(app.terminal.assist.commands.pending_history_entry.is_none());
        });
    }
}

#[test]
fn ordinary_input_during_calibration_cancels_the_gesture_and_waits_for_fresh_echo() {
    let dir = TestConfigDir::new("nyaterm-search-interleaved");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
    cx.update_entity(&app, |app, cx| {
        recall(app, "s1", true, b"\x1b", cx);
        let state = app.terminal.editing.state_mut();
        state.history_search.as_mut().unwrap().phase = HistorySearchPhase::MovingToStart;
        app.note_shell_editing_input(b"x", cx);
        app.reconcile_shell_editing("s1", cx);
        assert!(
            app.terminal
                .editing
                .state()
                .unwrap()
                .history_search
                .is_none()
        );
        assert!(!app.can_use_smart_cursor_selection());
        assert!(app.terminal.editing.predicted_input().desynced);
    });
}

#[test]
fn navigation_after_search_keeps_recovery_but_changed_text_revokes_it() {
    let dir = TestConfigDir::new("nyaterm-search-navigation");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
    cx.update_entity(&app, |app, cx| {
        let mut transcript = recall(app, "s1", false, b"\x1b", cx);
        for (key, echo) in [
            (b"\x05".as_slice(), "\x1b[11C"),
            (b"\x1b[D".as_slice(), "\x1b[D"),
        ] {
            app.note_shell_editing_input(key, cx);
            paint(app, "s1", &mut transcript, echo, cx);
            assert!(app.history_search_ready_for_gesture());
        }
        paint(app, "s1", &mut transcript, "\r$ changed command\x1b[K", cx);
        assert!(!app.history_search_ready_for_gesture());
        assert!(!app.can_use_smart_cursor_selection());
    });
}
