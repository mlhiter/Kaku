use super::{PendingSessionStatusWrite, TermWindow, TermWindowNotif};
use crate::agent_status::adapters::AgentPaneOutputSample;
use crate::agent_status::codex_context::{is_codex_like_command, is_codex_process_name};
use crate::agent_status::events::{
    AgentEvent, SessionStatus, SessionStatusConfidence, SessionStatusSource,
};
use crate::agent_status::manager::SessionStatusSnapshot;
use mux::pane::{CachePolicy, Pane, PaneId};
use mux::Mux;
use smol::Timer;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wezterm_term::StableRowIndex;
use window::WindowOps;

const MIN_PROBE_INTERVAL: Duration = Duration::from_millis(450);
const CONTEXT_LOSS_PROBE_GRACE: Duration = Duration::from_secs(8);
const SESSION_STATUS_FLUSH_DELAY: Duration = Duration::from_millis(300);

impl TermWindow {
    pub(super) fn process_agent_user_var_signal(
        &mut self,
        pane_id: PaneId,
        name: &str,
        value: &str,
        user_vars: &HashMap<String, String>,
    ) {
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
        let now = Instant::now();
        if self
            .agent_output_probe_at_by_pane
            .get(&pane_id)
            .is_some_and(|last_probe| now.duration_since(*last_probe) < MIN_PROBE_INTERVAL)
        {
            return;
        }
        self.agent_output_probe_at_by_pane.insert(pane_id, now);

        let Some((pane_key, sample)) = self.collect_agent_pane_output_sample(pane_id) else {
            return;
        };

        let events = self
            .agent_adapter_registry
            .observe_pane_output(pane_key.as_str(), &sample);
        if !events.is_empty() {
            log::info!(
                "agent pane-output pane={} command={:?} process={:?} events={} detail={:?}",
                pane_key,
                sample.current_command,
                sample.foreground_process_name,
                events.len(),
                events
            );
        }

        self.process_agent_events_for_pane(
            pane_id,
            events,
            SessionStatusSource::Heuristic,
            SessionStatusConfidence::Low,
        );
    }

    fn collect_agent_pane_output_sample(
        &mut self,
        pane_id: PaneId,
    ) -> Option<(String, AgentPaneOutputSample)> {
        let mux = Mux::get();
        let pane = mux.get_pane(pane_id)?;
        let pane_key = pane_id.as_usize().to_string();
        let user_vars = pane.copy_user_vars();
        let current_command = user_vars
            .get("kaku_last_cmd")
            .or_else(|| user_vars.get("WEZTERM_PROG"))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let foreground_process_name = pane.get_foreground_process_name(CachePolicy::FetchImmediate);
        let foreground_process_contains_codex = pane
            .get_foreground_process_info(CachePolicy::FetchImmediate)
            .is_some_and(|info| info.flatten_to_exe_names().contains("codex"));
        let is_codex_context = current_command
            .as_deref()
            .is_some_and(is_codex_like_command)
            || foreground_process_name
                .as_deref()
                .is_some_and(is_codex_process_name)
            || foreground_process_contains_codex;

        let now = Instant::now();
        if is_codex_context {
            self.agent_output_probe_grace_until_by_pane
                .insert(pane_id, now + CONTEXT_LOSS_PROBE_GRACE);
        } else {
            let within_grace = self
                .agent_output_probe_grace_until_by_pane
                .get(&pane_id)
                .is_some_and(|until| now <= *until);
            if !within_grace {
                self.agent_output_probe_grace_until_by_pane.remove(&pane_id);
                return None;
            }
        }

        Some((
            pane_key,
            AgentPaneOutputSample {
                tail_text: self.recent_pane_tail_text(&pane, 18, 1600),
                current_command,
                foreground_process_name,
            },
        ))
    }

    fn recent_pane_tail_text(
        &self,
        pane: &Arc<dyn Pane>,
        line_count: usize,
        max_chars: usize,
    ) -> String {
        let dimensions = pane.get_dimensions();
        if dimensions.viewport_rows == 0 {
            return String::new();
        }

        let viewport_end = dimensions.physical_top + dimensions.viewport_rows as StableRowIndex;
        let effective_lines = line_count.min(dimensions.viewport_rows);
        let viewport_start = viewport_end - effective_lines as StableRowIndex;
        let (_, lines) = pane.get_lines(viewport_start..viewport_end);

        let mut text = String::new();
        for line in lines {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(line.as_str().as_ref());
            if text.chars().count() > max_chars {
                break;
            }
        }

        if text.chars().count() <= max_chars {
            return text;
        }

        text.chars()
            .rev()
            .take(max_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
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
