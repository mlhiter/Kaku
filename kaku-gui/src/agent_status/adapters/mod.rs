use crate::agent_status::adapters::claude::ClaudeAdapter;
use crate::agent_status::adapters::codex::CodexAdapter;
use crate::agent_status::events::AgentEvent;
use std::collections::HashMap;

pub mod claude;
pub mod codex;

pub trait AgentAdapter {
    fn provider(&self) -> &'static str;

    fn observe_user_var(
        &mut self,
        pane_key: &str,
        name: &str,
        value: &str,
        user_vars: &HashMap<String, String>,
    ) -> Vec<AgentEvent>;
}

#[derive(Default)]
pub struct AgentAdapterRegistry {
    adapters: Vec<Box<dyn AgentAdapter>>,
}

impl AgentAdapterRegistry {
    pub fn with_defaults() -> Self {
        let mut registry = Self::default();
        registry.register(Box::new(CodexAdapter::default()));
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
}
