use super::{PendingSessionStatusWrite, TermWindow, TermWindowNotif};
use crate::agent_status::events::{
    AgentEvent, SessionStatus, SessionStatusConfidence, SessionStatusSource,
};
use crate::agent_status::manager::SessionStatusSnapshot;
use mux::pane::PaneId;
use smol::Timer;
use std::collections::HashMap;
use std::time::Duration;
use window::WindowOps;

const SESSION_STATUS_FLUSH_DELAY: Duration = Duration::from_millis(300);

impl TermWindow {
    pub(super) fn process_agent_user_var_signal(
        &mut self,
        pane_id: PaneId,
        name: &str,
        value: &str,
        user_vars: &HashMap<String, String>,
    ) {
        if self.maybe_handle_codex_managed_start(pane_id, name, value, user_vars) {
            return;
        }

        let codex_channel_active = self
            .codex_app_server_channels_by_pane
            .contains_key(&pane_id);
        if codex_channel_active {
            return;
        }

        let pane_key = pane_id.as_usize().to_string();
        let events = self
            .agent_adapter_registry
            .observe_user_var(&pane_key, name, value, user_vars);

        if !events.is_empty()
            && matches!(
                name,
                "kaku_last_cmd" | "kaku_last_exit_code" | "WEZTERM_PROG"
            )
        {
            let value_sample = value.trim().chars().take(120).collect::<String>();
            log::debug!(
                "agent user-var pane={} name={} value='{}' events={}",
                pane_key,
                name,
                value_sample,
                events.len()
            );
        }

        self.process_agent_events_for_pane(
            pane_id,
            events,
            SessionStatusSource::Heuristic,
            SessionStatusConfidence::Low,
        );
    }

    pub(super) fn process_agent_pane_output_signal(&mut self, pane_id: PaneId) {
        let _ = pane_id;
    }

    fn process_agent_events_for_pane(
        &mut self,
        pane_id: PaneId,
        events: Vec<AgentEvent>,
        source: SessionStatusSource,
        confidence: SessionStatusConfidence,
    ) {
        if events.is_empty() {
            return;
        }

        let Some((project_id, session_id)) = self.sidebar_session_binding_for_pane(pane_id) else {
            log::warn!(
                "agent events dropped: no sidebar binding for pane={} event_count={}",
                pane_id,
                events.len()
            );
            return;
        };
        let session_key = format!("{project_id}/{session_id}");
        if self.agent_status_manager.snapshot(&session_key).is_none() {
            let initial = self
                .sidebar_session_status_snapshot(project_id.as_str(), session_id.as_str())
                .unwrap_or_else(|| {
                    SessionStatusSnapshot::new(
                        SessionStatus::Idle,
                        SessionStatusSource::Heuristic,
                        SessionStatusConfidence::Low,
                        None,
                    )
                });
            self.agent_status_manager
                .register_session(session_key.clone(), initial);
        }

        for event in events {
            let transition = match self.agent_status_manager.apply_agent_event(
                session_key.as_str(),
                &event,
                source,
                confidence,
            ) {
                Ok(transition) => transition,
                Err(err) => {
                    log::warn!(
                        "agent status transition rejected for {}: {:#}",
                        session_key,
                        err
                    );
                    continue;
                }
            };
            let Some(transition) = transition else {
                continue;
            };
            log::info!(
                "agent status transition pane={} session={} event={:?} {} -> {}",
                pane_id,
                session_key,
                event,
                transition.previous.status.as_storage_str(),
                transition.current.status.as_storage_str()
            );
            self.enqueue_session_status_write(
                project_id.as_str(),
                session_id.as_str(),
                transition.current,
            );
        }
    }

    pub(super) fn process_agent_events_for_session(
        &mut self,
        pane_id: PaneId,
        project_id: &str,
        session_id: &str,
        events: Vec<AgentEvent>,
        source: SessionStatusSource,
        confidence: SessionStatusConfidence,
    ) {
        if events.is_empty() {
            return;
        }
        let session_key = format!("{project_id}/{session_id}");
        if self.agent_status_manager.snapshot(&session_key).is_none() {
            let initial = self
                .sidebar_session_status_snapshot(project_id, session_id)
                .unwrap_or_else(|| {
                    SessionStatusSnapshot::new(
                        SessionStatus::Idle,
                        SessionStatusSource::Structured,
                        SessionStatusConfidence::High,
                        None,
                    )
                });
            self.agent_status_manager
                .register_session(session_key.clone(), initial);
        }

        for event in events {
            let transition = match self.agent_status_manager.apply_agent_event(
                session_key.as_str(),
                &event,
                source,
                confidence,
            ) {
                Ok(transition) => transition,
                Err(err) => {
                    log::warn!(
                        "agent status transition rejected for {}: {:#}",
                        session_key,
                        err
                    );
                    continue;
                }
            };
            let Some(transition) = transition else {
                continue;
            };
            log::info!(
                "agent status transition pane={} session={} event={:?} {} -> {}",
                pane_id,
                session_key,
                event,
                transition.previous.status.as_storage_str(),
                transition.current.status.as_storage_str()
            );
            self.enqueue_session_status_write(project_id, session_id, transition.current);
        }
    }

    fn enqueue_session_status_write(
        &mut self,
        project_id: &str,
        session_id: &str,
        snapshot: SessionStatusSnapshot,
    ) {
        let session_key = format!("{project_id}/{session_id}");
        self.pending_session_status_writes.insert(
            session_key,
            PendingSessionStatusWrite {
                project_id: project_id.to_string(),
                session_id: session_id.to_string(),
                snapshot,
            },
        );

        if self.session_status_flush_scheduled {
            return;
        }
        self.session_status_flush_scheduled = true;

        let Some(window) = self.window.clone() else {
            self.flush_pending_session_status_writes();
            return;
        };
        promise::spawn::spawn(async move {
            Timer::after(SESSION_STATUS_FLUSH_DELAY).await;
            window.notify(TermWindowNotif::Apply(Box::new(|term_window| {
                term_window.flush_pending_session_status_writes();
            })));
        })
        .detach();
    }

    pub(super) fn flush_pending_session_status_writes(&mut self) {
        self.session_status_flush_scheduled = false;
        let writes = std::mem::take(&mut self.pending_session_status_writes);
        for (_, write) in writes {
            if let Err(err) = self.sidebar_set_session_status_snapshot(
                write.project_id.as_str(),
                write.session_id.as_str(),
                &write.snapshot,
            ) {
                log::warn!(
                    "failed to persist session status for {}/{}: {:#}",
                    write.project_id,
                    write.session_id,
                    err
                );
            }
        }
    }
}
