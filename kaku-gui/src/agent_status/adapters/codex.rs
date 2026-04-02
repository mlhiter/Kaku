use crate::agent_status::adapters::AgentAdapter;
use crate::agent_status::events::AgentEvent;
use std::collections::HashMap;

#[derive(Default)]
pub struct CodexAdapter {
    last_command_by_pane: HashMap<String, String>,
    command_generation_by_pane: HashMap<String, u64>,
    last_exit_signature_by_pane: HashMap<String, String>,
}

impl AgentAdapter for CodexAdapter {
    fn provider(&self) -> &'static str {
        "codex"
    }

    fn observe_user_var(
        &mut self,
        pane_key: &str,
        name: &str,
        value: &str,
        user_vars: &HashMap<String, String>,
    ) -> Vec<AgentEvent> {
        match name {
            "kaku_last_cmd" => self.observe_last_cmd(pane_key, value),
            "kaku_last_exit_code" => self.observe_last_exit_code(pane_key, value, user_vars),
            _ => Vec::new(),
        }
    }
}

impl CodexAdapter {
    fn observe_last_cmd(&mut self, pane_key: &str, command: &str) -> Vec<AgentEvent> {
        let normalized = command.trim().to_string();
        if normalized.is_empty() {
            return Vec::new();
        }

        let generation = self
            .command_generation_by_pane
            .entry(pane_key.to_string())
            .or_insert(0);
        *generation = generation.saturating_add(1);

        self.last_command_by_pane
            .insert(pane_key.to_string(), normalized.clone());

        if !is_codex_command(normalized.as_str()) {
            return Vec::new();
        }

        vec![
            AgentEvent::TaskStarted {
                provider: self.provider().to_string(),
            },
            AgentEvent::TaskOutput {
                provider: self.provider().to_string(),
            },
        ]
    }

    fn observe_last_exit_code(
        &mut self,
        pane_key: &str,
        exit_code_value: &str,
        user_vars: &HashMap<String, String>,
    ) -> Vec<AgentEvent> {
        let exit_code = match exit_code_value.trim().parse::<i32>() {
            Ok(code) => code,
            Err(_) => return Vec::new(),
        };

        let command = user_vars
            .get("kaku_last_cmd")
            .filter(|cmd| !cmd.trim().is_empty())
            .cloned()
            .or_else(|| self.last_command_by_pane.get(pane_key).cloned());

        let Some(command) = command else {
            return Vec::new();
        };
        if !is_codex_command(command.as_str()) {
            return Vec::new();
        }

        let generation = self
            .command_generation_by_pane
            .get(pane_key)
            .copied()
            .unwrap_or_default();
        let signature = format!("{}\0{}\0{}", generation, command, exit_code);
        if self
            .last_exit_signature_by_pane
            .get(pane_key)
            .is_some_and(|previous| previous == &signature)
        {
            return Vec::new();
        }
        self.last_exit_signature_by_pane
            .insert(pane_key.to_string(), signature);

        if exit_code == 0 {
            vec![AgentEvent::TaskCompleted {
                provider: self.provider().to_string(),
            }]
        } else {
            vec![AgentEvent::TaskFailed {
                provider: self.provider().to_string(),
                reason: Some(format!("codex exited with status {}", exit_code)),
            }]
        }
    }
}

fn is_codex_command(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    for token in trimmed.split_whitespace() {
        if token.contains('=') && !token.starts_with('/') && !token.starts_with("./") {
            continue;
        }
        let executable = token.rsplit('/').next().unwrap_or(token);
        return executable == "codex";
    }

    false
}

#[cfg(test)]
mod tests {
    use super::{is_codex_command, CodexAdapter};
    use crate::agent_status::adapters::AgentAdapter;
    use crate::agent_status::events::AgentEvent;
    use std::collections::HashMap;

    #[test]
    fn detects_codex_command_variants() {
        assert!(is_codex_command("codex run"));
        assert!(is_codex_command("/usr/local/bin/codex run"));
        assert!(is_codex_command("FOO=bar codex run"));
        assert!(!is_codex_command("claude --print"));
        assert!(!is_codex_command(""));
    }

    #[test]
    fn emits_started_and_completed_for_codex_command() {
        let mut adapter = CodexAdapter::default();
        let started = adapter.observe_user_var("1", "kaku_last_cmd", "codex --help", &HashMap::new());
        assert_eq!(
            started,
            vec![
                AgentEvent::TaskStarted {
                    provider: "codex".to_string()
                },
                AgentEvent::TaskOutput {
                    provider: "codex".to_string()
                }
            ]
        );

        let mut vars = HashMap::new();
        vars.insert("kaku_last_cmd".to_string(), "codex --help".to_string());
        let completed = adapter.observe_user_var("1", "kaku_last_exit_code", "0", &vars);
        assert_eq!(
            completed,
            vec![AgentEvent::TaskCompleted {
                provider: "codex".to_string()
            }]
        );
    }

    #[test]
    fn emits_failed_for_nonzero_codex_exit() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());
        let mut vars = HashMap::new();
        vars.insert("kaku_last_cmd".to_string(), "codex".to_string());
        let failed = adapter.observe_user_var("1", "kaku_last_exit_code", "130", &vars);
        assert_eq!(failed.len(), 1);
        match &failed[0] {
            AgentEvent::TaskFailed { reason, .. } => {
                assert_eq!(reason.as_deref(), Some("codex exited with status 130"));
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[test]
    fn ignores_duplicate_exit_signal() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());
        let mut vars = HashMap::new();
        vars.insert("kaku_last_cmd".to_string(), "codex".to_string());
        let first = adapter.observe_user_var("1", "kaku_last_exit_code", "0", &vars);
        let second = adapter.observe_user_var("1", "kaku_last_exit_code", "0", &vars);
        assert_eq!(first.len(), 1);
        assert!(second.is_empty());
    }

    #[test]
    fn repeated_same_command_still_emits_completion() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());
        let mut vars = HashMap::new();
        vars.insert("kaku_last_cmd".to_string(), "codex".to_string());
        let first = adapter.observe_user_var("1", "kaku_last_exit_code", "0", &vars);
        assert_eq!(first.len(), 1);

        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());
        let second = adapter.observe_user_var("1", "kaku_last_exit_code", "0", &vars);
        assert_eq!(second.len(), 1);
    }
}
