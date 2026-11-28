//! Real Bash/readline echoes, isolated from user rc files and history.
#![cfg(unix)]

use std::time::{Duration, Instant};

use nyaterm_core::terminal::editing::{EditIntent, plan_edit};
use nyaterm_core::terminal::input_tracker::TerminalInputState;
use nyaterm_terminal::TerminalScreen;
use nyaterm_terminal::editing::history_search::HistoryInputBoundary;
use nyaterm_terminal::editing::resolve_input;
use nyaterm_transport::{LocalSessionConfig, SessionEvent, SessionManager};

struct Bash {
    manager: SessionManager,
    id: String,
    screen: TerminalScreen,
}

impl Drop for Bash {
    fn drop(&mut self) {
        let _ = self.manager.close(&self.id);
    }
}

impl Bash {
    fn send(&self, data: &[u8]) {
        self.manager.write(&self.id, data).unwrap();
    }

    fn wait_for(&mut self, predicate: impl Fn(&TerminalScreen) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "synthetic readline echo timed out"
            );
            for event in self
                .manager
                .drain_events_blocking_with_output_budget(128, 65536, Duration::from_millis(20))
                .unwrap()
                .events
            {
                if let SessionEvent::Output { session_id, data } = event
                    && session_id == self.id
                {
                    self.screen.advance(&data);
                }
            }
            for reply in self.screen.take_effects().pty_write {
                self.send(&reply);
            }
            if predicate(&self.screen) {
                return;
            }
        }
    }
}

#[test]
fn bash_search_exit_calibration_and_replacement_preserve_trailing_spaces_without_submission() {
    for exit in [b"\x1b".as_slice(), b"\x1b[D".as_slice()] {
        let manager = SessionManager::new();
        let info = manager
            .create_local_session(LocalSessionConfig {
                // env supplies process-local values without changing the test
                // runner's environment or allowing shell integration to load rc files.
                shell_path: Some("/usr/bin/env".into()),
                shell_args: [
                    "HISTFILE=/dev/null",
                    "HISTSIZE=100",
                    "HISTCONTROL=",
                    "HISTIGNORE=",
                    "INPUTRC=/dev/null",
                    "PS1=$ ",
                    "PROMPT_COMMAND=",
                    "TERM=xterm-256color",
                    "/bin/bash",
                    "--noprofile",
                    "--norc",
                    "-i",
                ]
                .map(str::to_owned)
                .to_vec(),
                cols: 80,
                rows: 12,
                ..LocalSessionConfig::default()
            })
            .unwrap();
        let mut bash = Bash {
            manager,
            id: info.id,
            screen: TerminalScreen::new(80, 12),
        };
        bash.wait_for(|screen| {
            screen
                .snapshot()
                .line(screen.snapshot().cursor.row)
                .is_some_and(|line| line.trim() == "$")
        });
        bash.send(b"set -o emacs; history -s 'echo NYA_RECALL tail  '\r");
        bash.wait_for(|screen| {
            let snapshot = screen.snapshot();
            snapshot.cursor.row > 0
                && snapshot
                    .line(snapshot.cursor.row)
                    .is_some_and(|line| line.trim() == "$")
        });
        let boundary = HistoryInputBoundary::capture(&bash.screen.snapshot(), None).unwrap();
        bash.send(b"\x12NYA_RECALL");
        bash.wait_for(|screen| boundary.is_search_display(&screen.snapshot()));
        bash.send(exit);
        bash.wait_for(|screen| boundary.observe(&screen.snapshot(), false).is_some());
        let observed = boundary.observe(&bash.screen.snapshot(), false).unwrap();
        assert!(observed.cursor < observed.visible.len());
        bash.send(b"\x01");
        bash.wait_for(|screen| {
            boundary
                .observe(&screen.snapshot(), false)
                .is_some_and(|input| input.cursor == 0)
        });
        bash.send(b"\x05");
        bash.wait_for(|screen| {
            boundary
                .observe(&screen.snapshot(), true)
                .is_some_and(|input| input.visible.as_str() == "echo NYA_RECALL tail  ")
        });
        let recovered = boundary.observe(&bash.screen.snapshot(), true).unwrap();
        let input = TerminalInputState {
            value: recovered.visible.to_string(),
            cursor: recovered.cursor,
            ..TerminalInputState::default()
        };
        let plan = plan_edit(
            &input,
            0,
            EditIntent::Replace {
                range: 5..15,
                text: "X",
            },
        )
        .unwrap();
        assert!(!plan.move_right);
        let mut wire = b"\x1b[D".repeat(plan.move_steps);
        wire.extend(std::iter::repeat_n(0x7f, plan.delete_steps));
        wire.extend_from_slice(plan.insert_text.as_bytes());
        assert!(!wire.contains(&b'\r'));
        bash.send(&wire);
        bash.wait_for(|screen| {
            resolve_input(
                &screen.snapshot(),
                &plan.predicted.value,
                plan.predicted.cursor,
            )
            .is_some()
        });
        assert_eq!(plan.predicted.value, "echo X tail  ");
        // Cancel instead of executing the synthetic command.
        bash.send(b"\x03");
    }
}
