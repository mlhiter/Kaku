use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Loading,
    NeedApprove,
    Running,
    Done,
    Error,
}

impl SessionStatus {
    pub const fn as_storage_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::NeedApprove => "need_approve",
            Self::Running => "running",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    pub fn parse_storage(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "idle" => Some(Self::Idle),
            "loading" => Some(Self::Loading),
            "need_approve" | "need-approve" | "needapprove" => Some(Self::NeedApprove),
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusSource {
    Structured,
    Heuristic,
}

impl SessionStatusSource {
    pub const fn as_storage_str(self) -> &'static str {
        match self {
            Self::Structured => "structured",
            Self::Heuristic => "heuristic",
        }
    }

    pub fn parse_storage(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "structured" => Some(Self::Structured),
            "heuristic" => Some(Self::Heuristic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusConfidence {
    High,
    Low,
}

impl SessionStatusConfidence {
    pub const fn as_storage_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Low => "low",
        }
    }

    pub fn parse_storage(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "high" => Some(Self::High),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    TaskStarted {
        provider: String,
    },
    TaskOutput {
        provider: String,
    },
    ApprovalRequired {
        provider: String,
        detail: Option<String>,
    },
    ApprovalResolved {
        provider: String,
        approved: bool,
    },
    TaskCompleted {
        provider: String,
    },
    TaskFailed {
        provider: String,
        reason: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::{SessionStatus, SessionStatusConfidence, SessionStatusSource};

    #[test]
    fn parse_storage_status_aliases() {
        assert_eq!(SessionStatus::parse_storage("idle"), Some(SessionStatus::Idle));
        assert_eq!(
            SessionStatus::parse_storage("need-approve"),
            Some(SessionStatus::NeedApprove)
        );
        assert_eq!(
            SessionStatus::parse_storage("needapprove"),
            Some(SessionStatus::NeedApprove)
        );
        assert_eq!(SessionStatus::parse_storage("unknown"), None);
    }

    #[test]
    fn parse_storage_source_and_confidence() {
        assert_eq!(
            SessionStatusSource::parse_storage("structured"),
            Some(SessionStatusSource::Structured)
        );
        assert_eq!(
            SessionStatusSource::parse_storage("heuristic"),
            Some(SessionStatusSource::Heuristic)
        );
        assert_eq!(
            SessionStatusConfidence::parse_storage("high"),
            Some(SessionStatusConfidence::High)
        );
        assert_eq!(
            SessionStatusConfidence::parse_storage("low"),
            Some(SessionStatusConfidence::Low)
        );
        assert_eq!(SessionStatusSource::parse_storage("x"), None);
        assert_eq!(SessionStatusConfidence::parse_storage("x"), None);
    }
}
