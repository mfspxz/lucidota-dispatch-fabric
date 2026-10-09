//! T005: TaskState with validated transitions.
//!
//! Every allowed transition is listed in [`TaskState::ALLOWED`]; anything not
//! listed is rejected with [`FabricError::IllegalTransition`]. Terminal states
//! have no outgoing transitions.

use crate::error::{FabricError, Result};
use serde::{Deserialize, Serialize};

/// T005: lifecycle state of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskState {
    Proposed,
    Ready,
    Blocked,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskState {
    /// Every legal (from, to) transition. Anything else is rejected.
    pub const ALLOWED: [(Self, Self); 13] = [
        (Self::Proposed, Self::Ready),
        (Self::Proposed, Self::Cancelled),
        (Self::Ready, Self::Running),
        (Self::Ready, Self::Blocked),
        (Self::Ready, Self::Cancelled),
        (Self::Blocked, Self::Ready),
        (Self::Blocked, Self::Cancelled),
        (Self::Running, Self::Succeeded),
        (Self::Running, Self::Failed),
        (Self::Running, Self::Blocked),
        (Self::Running, Self::Cancelled),
        (Self::Failed, Self::Ready),
        (Self::Failed, Self::Cancelled),
    ];

    /// True when no transition may leave this state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
            && !Self::ALLOWED.iter().any(|(from, _)| *from == self)
    }

    /// True when `self -> to` is a legal transition.
    pub fn can_transition_to(self, to: Self) -> bool {
        Self::ALLOWED.contains(&(self, to))
    }

    /// Move to the next state, rejecting illegal transitions.
    pub fn transition(self, to: Self) -> Result<Self> {
        if self.can_transition_to(to) {
            Ok(to)
        } else {
            Err(FabricError::IllegalTransition {
                from: self.name(),
                to: to.name(),
            })
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Proposed => "PROPOSED",
            Self::Ready => "READY",
            Self::Blocked => "BLOCKED",
            Self::Running => "RUNNING",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_transitions_pass() {
        for (from, to) in TaskState::ALLOWED {
            assert!(from.can_transition_to(to), "{from:?} -> {to:?}");
            assert_eq!(from.transition(to).unwrap(), to);
        }
    }

    #[test]
    fn illegal_transitions_rejected() {
        // skipped states
        assert!(TaskState::Proposed.transition(TaskState::Running).is_err());
        // backwards out of terminal
        assert!(TaskState::Succeeded.transition(TaskState::Ready).is_err());
        assert!(TaskState::Cancelled.transition(TaskState::Proposed).is_err());
        // self transitions are not listed
        assert!(TaskState::Ready.transition(TaskState::Ready).is_err());
    }

    #[test]
    fn terminal_states_have_no_outgoing() {
        assert!(TaskState::Succeeded.is_terminal());
        assert!(!TaskState::Running.is_terminal());
        assert!(!TaskState::Failed.is_terminal()); // Failed -> Ready/Cancelled allowed
    }

    #[test]
    fn serializes_as_name() {
        let j = serde_json::to_string(&TaskState::Running).unwrap();
        assert_eq!(j, "\"RUNNING\"");
    }
}
