use crate::agent_status::adapters::AgentAdapter;
use crate::agent_status::events::AgentEvent;
use std::collections::HashMap;

#[derive(Default)]
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn provider(&self) -> &'static str {
        "claude"
    }

    fn observe_user_var(
        &mut self,
        _pane_key: &str,
        _name: &str,
        _value: &str,
        _user_vars: &HashMap<String, String>,
    ) -> Vec<AgentEvent> {
        // M2-1B keeps Claude as a no-op placeholder.
        // M2-1C will wire real Claude signals into AgentEvent.
        Vec::new()
    }
}
