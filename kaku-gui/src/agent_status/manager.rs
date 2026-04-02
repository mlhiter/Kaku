use crate::agent_status::events::{
    AgentEvent, SessionStatus, SessionStatusConfidence, SessionStatusSource,
};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatusSnapshot {
    pub status: SessionStatus,
    pub source: SessionStatusSource,
    pub confidence: SessionStatusConfidence,
    pub reason: Option<String>,
}

impl SessionStatusSnapshot {
    pub fn new(
        status: SessionStatus,
        source: SessionStatusSource,
        confidence: SessionStatusConfidence,
        reason: Option<String>,
    ) -> Self {
        Self {
            status,
            source,
            confidence,
            reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatusTransition {
    pub session_id: String,
    pub previous: SessionStatusSnapshot,
    pub current: SessionStatusSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatusError {
    SessionNotRegistered { session_id: String },
    InvalidTransition {
        session_id: String,
        from: SessionStatus,
        to: SessionStatus,
    },
}

impl Display for SessionStatusError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionNotRegistered { session_id } => {
                write!(f, "session not registered: {session_id}")
            }
            Self::InvalidTransition {
                session_id,
                from,
                to,
            } => write!(
                f,
                "invalid session status transition for {session_id}: {} -> {}",
                from.as_storage_str(),
                to.as_storage_str()
            ),
        }
    }
}

impl std::error::Error for SessionStatusError {}

#[derive(Debug, Default)]
pub struct SessionStatusManager {
    sessions: HashMap<String, SessionStatusSnapshot>,
}

impl SessionStatusManager {
    pub fn register_session(
        &mut self,
        session_id: impl Into<String>,
        snapshot: SessionStatusSnapshot,
    ) -> Option<SessionStatusSnapshot> {
        self.sessions.insert(session_id.into(), snapshot)
    }

    pub fn remove_session(&mut self, session_id: &str) -> Option<SessionStatusSnapshot> {
        self.sessions.remove(session_id)
    }

    pub fn snapshot(&self, session_id: &str) -> Option<&SessionStatusSnapshot> {
        self.sessions.get(session_id)
    }

    pub fn update_status(
        &mut self,
        session_id: &str,
        next: SessionStatusSnapshot,
    ) -> Result<Option<SessionStatusTransition>, SessionStatusError> {
        let current = self
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionStatusError::SessionNotRegistered {
                session_id: session_id.to_string(),
            })?;

        if current == next {
            return Ok(None);
        }

        if !is_transition_allowed(current.status, next.status) {
            return Err(SessionStatusError::InvalidTransition {
                session_id: session_id.to_string(),
                from: current.status,
                to: next.status,
            });
        }

        self.sessions.insert(session_id.to_string(), next.clone());
        Ok(Some(SessionStatusTransition {
            session_id: session_id.to_string(),
            previous: current,
            current: next,
        }))
    }

    pub fn apply_agent_event(
        &mut self,
        session_id: &str,
        event: &AgentEvent,
        source: SessionStatusSource,
        confidence: SessionStatusConfidence,
    ) -> Result<Option<SessionStatusTransition>, SessionStatusError> {
        let (status, reason) = map_agent_event_to_status(event);
        self.update_status(
            session_id,
            SessionStatusSnapshot::new(status, source, confidence, reason),
        )
    }
}

pub fn map_agent_event_to_status(event: &AgentEvent) -> (SessionStatus, Option<String>) {
    match event {
        AgentEvent::TaskStarted { .. } => (SessionStatus::Loading, None),
        AgentEvent::TaskOutput { .. } => (SessionStatus::Running, None),
        AgentEvent::ApprovalRequired { detail, .. } => {
            (SessionStatus::NeedApprove, detail.clone())
        }
        AgentEvent::ApprovalResolved { approved, .. } => {
            if *approved {
                (SessionStatus::Running, None)
            } else {
                (
                    SessionStatus::Error,
                    Some("approval rejected or timed out".to_string()),
                )
            }
        }
        AgentEvent::TaskCompleted { .. } => (SessionStatus::Done, None),
        AgentEvent::TaskFailed { reason, .. } => (SessionStatus::Error, reason.clone()),
    }
}

pub fn is_transition_allowed(from: SessionStatus, to: SessionStatus) -> bool {
    if from == to {
        return true;
    }

    match from {
        SessionStatus::Idle => matches!(to, SessionStatus::Loading | SessionStatus::Running),
        SessionStatus::Loading => matches!(
            to,
            SessionStatus::Idle
                | SessionStatus::Running
                | SessionStatus::Done
                | SessionStatus::Error
        ),
        SessionStatus::Running => matches!(
            to,
            SessionStatus::Idle
                | SessionStatus::NeedApprove
                | SessionStatus::Done
                | SessionStatus::Error
        ),
        SessionStatus::NeedApprove => {
            matches!(
                to,
                SessionStatus::Idle | SessionStatus::Running | SessionStatus::Error
            )
        }
        SessionStatus::Done => matches!(to, SessionStatus::Idle | SessionStatus::Loading),
        SessionStatus::Error => matches!(to, SessionStatus::Idle | SessionStatus::Loading),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_transition_allowed, map_agent_event_to_status, AgentEvent, SessionStatus,
        SessionStatusManager, SessionStatusSnapshot,
    };
    use crate::agent_status::events::{SessionStatusConfidence, SessionStatusSource};

    fn default_snapshot(status: SessionStatus) -> SessionStatusSnapshot {
        SessionStatusSnapshot::new(
            status,
            SessionStatusSource::Heuristic,
            SessionStatusConfidence::Low,
            None,
        )
    }

    #[test]
    fn validates_transition_rules() {
        assert!(is_transition_allowed(
            SessionStatus::Idle,
            SessionStatus::Loading
        ));
        assert!(is_transition_allowed(
            SessionStatus::Running,
            SessionStatus::NeedApprove
        ));
        assert!(!is_transition_allowed(
            SessionStatus::Done,
            SessionStatus::NeedApprove
        ));
        assert!(!is_transition_allowed(
            SessionStatus::Error,
            SessionStatus::Done
        ));
    }

    #[test]
    fn applies_valid_transition() {
        let mut manager = SessionStatusManager::default();
        manager.register_session("sess-1", default_snapshot(SessionStatus::Idle));
        let transition = manager
            .update_status("sess-1", default_snapshot(SessionStatus::Loading))
            .expect("update status")
            .expect("transition should exist");
        assert_eq!(transition.previous.status, SessionStatus::Idle);
        assert_eq!(transition.current.status, SessionStatus::Loading);
        assert_eq!(
            manager.snapshot("sess-1").expect("snapshot").status,
            SessionStatus::Loading
        );
    }

    #[test]
    fn rejects_invalid_transition() {
        let mut manager = SessionStatusManager::default();
        manager.register_session("sess-1", default_snapshot(SessionStatus::Done));
        let err = manager
            .update_status("sess-1", default_snapshot(SessionStatus::NeedApprove))
            .expect_err("must reject invalid transition");
        let message = err.to_string();
        assert!(message.contains("invalid session status transition"));
    }

    #[test]
    fn maps_agent_events_to_status() {
        let event = AgentEvent::ApprovalRequired {
            provider: "codex".to_string(),
            detail: Some("approval needed".to_string()),
        };
        let (status, reason) = map_agent_event_to_status(&event);
        assert_eq!(status, SessionStatus::NeedApprove);
        assert_eq!(reason.as_deref(), Some("approval needed"));

        let failed = AgentEvent::TaskFailed {
            provider: "codex".to_string(),
            reason: Some("exit 1".to_string()),
        };
        let (status, reason) = map_agent_event_to_status(&failed);
        assert_eq!(status, SessionStatus::Error);
        assert_eq!(reason.as_deref(), Some("exit 1"));
    }

    #[test]
    fn applies_agent_event_updates_state() {
        let mut manager = SessionStatusManager::default();
        manager.register_session("sess-1", default_snapshot(SessionStatus::Idle));

        manager
            .apply_agent_event(
                "sess-1",
                &AgentEvent::TaskStarted {
                    provider: "codex".to_string(),
                },
                SessionStatusSource::Structured,
                SessionStatusConfidence::High,
            )
            .expect("started should apply");
        assert_eq!(
            manager.snapshot("sess-1").expect("snapshot").status,
            SessionStatus::Loading
        );

        manager
            .apply_agent_event(
                "sess-1",
                &AgentEvent::TaskOutput {
                    provider: "codex".to_string(),
                },
                SessionStatusSource::Structured,
                SessionStatusConfidence::High,
            )
            .expect("output should apply");
        assert_eq!(
            manager.snapshot("sess-1").expect("snapshot").status,
            SessionStatus::Running
        );

        manager
            .apply_agent_event(
                "sess-1",
                &AgentEvent::TaskCompleted {
                    provider: "codex".to_string(),
                },
                SessionStatusSource::Structured,
                SessionStatusConfidence::High,
            )
            .expect("completed should apply");
        assert_eq!(
            manager.snapshot("sess-1").expect("snapshot").status,
            SessionStatus::Done
        );
    }
}
