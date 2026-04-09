use crate::agent_status::adapters::claude::ClaudeAdapter;
use crate::agent_status::adapters::codex::CodexAdapter;
use crate::agent_status::events::AgentEvent;
use std::collections::HashMap;

pub mod claude;
pub mod codex;

#[derive(Debug, Clone, Default)]
pub struct AgentPaneOutputSample {
    pub tail_text: String,
    pub current_command: Option<String>,
    pub foreground_process_name: Option<String>,
}

pub trait AgentAdapter {
    fn provider(&self) -> &'static str;

    fn observe_user_var(
        &mut self,
        pane_key: &str,
        name: &str,
        value: &str,
        user_vars: &HashMap<String, String>,
    ) -> Vec<AgentEvent>;

    fn observe_pane_output(
        &mut self,
        _pane_key: &str,
        _sample: &AgentPaneOutputSample,
    ) -> Vec<AgentEvent> {
        Vec::new()
    }
}

#[derive(Default)]
pub struct AgentAdapterRegistry {
    adapters: Vec<Box<dyn AgentAdapter>>,
}

impl AgentAdapterRegistry {
    pub fn with_defaults() -> Self {
        let mut registry = Self::default();
        registry.register(Box::new(CodexAdapter::structured_only()));
        registry.register(Box::new(ClaudeAdapter::default()));
        registry
    }

    pub fn register(&mut self, adapter: Box<dyn AgentAdapter>) {
        self.adapters.push(adapter);
    }

    pub fn observe_user_var(
        &mut self,
        pane_key: &str,
        name: &str,
        value: &str,
        user_vars: &HashMap<String, String>,
    ) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for adapter in &mut self.adapters {
            let mut adapter_events = adapter.observe_user_var(pane_key, name, value, user_vars);
            events.append(&mut adapter_events);
        }
        events
    }

    pub fn observe_pane_output(
        &mut self,
        pane_key: &str,
        sample: &AgentPaneOutputSample,
    ) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for adapter in &mut self.adapters {
            let mut adapter_events = adapter.observe_pane_output(pane_key, sample);
            events.append(&mut adapter_events);
        }
        events
    }
}
