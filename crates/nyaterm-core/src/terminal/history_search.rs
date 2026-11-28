//! Readline search lifecycle. Only terminal observations can acknowledge exit.

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum HistorySearchPhase {
    #[default]
    Searching,
    AwaitingPrompt,
    Ready,
    MovingToStart,
    MovingToEnd,
}

impl HistorySearchPhase {
    /// False means ordinary input cancels the recovery request. Search text is
    /// never ordinary command typing, including after a failed match.
    pub fn note_input(&mut self, text: &str) -> bool {
        match self {
            Self::Searching | Self::AwaitingPrompt => {
                if matches!(text, "\x12" | "\x13") {
                    *self = Self::Searching;
                    true
                } else if matches!(
                    text,
                    "\x1b"
                        | "\x07"
                        | "\x01"
                        | "\x05"
                        | "\x1b[D"
                        | "\x1bOD"
                        | "\x1b[C"
                        | "\x1bOC"
                        | "\x1b[H"
                        | "\x1bOH"
                        | "\x1b[F"
                        | "\x1bOF"
                        | "\x1b[A"
                        | "\x1bOA"
                        | "\x1b[B"
                        | "\x1bOB"
                ) {
                    *self = Self::AwaitingPrompt;
                    true
                } else {
                    *self == Self::Searching
                        && (text.chars().all(|ch| !ch.is_control())
                            || matches!(text, "\x7f" | "\x08" | "\x17"))
                }
            }
            Self::Ready => matches!(
                text,
                "\x01"
                    | "\x05"
                    | "\x1b[D"
                    | "\x1bOD"
                    | "\x1b[C"
                    | "\x1bOC"
                    | "\x1b[H"
                    | "\x1bOH"
                    | "\x1b[F"
                    | "\x1bOF"
            ),
            Self::MovingToStart | Self::MovingToEnd => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HistorySearchPhase;

    #[test]
    fn search_queries_and_repeated_search_do_not_become_command_typing() {
        let mut phase = HistorySearchPhase::Searching;
        for key in ["echo", "\x12", "\x7f", "界", "\x17"] {
            assert!(phase.note_input(key));
            assert_eq!(phase, HistorySearchPhase::Searching);
        }
        for key in ["\x1b", "\x1b[D", "\x1bOC", "\x07", "\x05"] {
            phase = HistorySearchPhase::Searching;
            assert!(phase.note_input(key));
            assert_eq!(phase, HistorySearchPhase::AwaitingPrompt);
        }
    }

    #[test]
    fn submission_and_new_typing_cancel_recovery_without_replaying_input() {
        for key in ["\r", "\n", "\x03", "\x04", "\x1b[200~paste\x1b[201~"] {
            assert!(!HistorySearchPhase::Searching.note_input(key));
        }
        for mut phase in [
            HistorySearchPhase::AwaitingPrompt,
            HistorySearchPhase::Ready,
            HistorySearchPhase::MovingToStart,
            HistorySearchPhase::MovingToEnd,
        ] {
            assert!(!phase.note_input("x"));
        }
    }
}
