use crate::agent_status::adapters::{AgentAdapter, AgentPaneOutputSample};
use crate::agent_status::codex_context::{
    is_codex_like_command as shared_is_codex_like_command,
    is_codex_process_name as shared_is_codex_process_name,
};
use crate::agent_status::events::AgentEvent;
use serde_json::Value;
use std::collections::HashMap;

const CODEX_PROVIDER: &str = "codex";

#[derive(Debug, Default, Clone)]
struct PaneRuntimeState {
    last_command: Option<String>,
    command_generation: u64,
    last_exit_signature: Option<String>,
    fallback_active: bool,
    fallback_hard_context: bool,
    approval_prompt_visible: bool,
    approval_prompt_detail: Option<String>,
    last_app_server_signature: Option<String>,
    app_server_turn_active: bool,
    app_server_signal_seen: bool,
    fallback_last_tail_signature: Option<String>,
}

pub struct CodexAdapter {
    pane_state_by_key: HashMap<String, PaneRuntimeState>,
    heuristic_enabled: bool,
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self {
            pane_state_by_key: HashMap::new(),
            // Keep historical behavior for unit tests; production wiring can
            // opt into structured-only mode explicitly.
            heuristic_enabled: true,
        }
    }
}

impl AgentAdapter for CodexAdapter {
    fn provider(&self) -> &'static str {
        CODEX_PROVIDER
    }

    fn observe_user_var(
        &mut self,
        pane_key: &str,
        name: &str,
        value: &str,
        user_vars: &HashMap<String, String>,
    ) -> Vec<AgentEvent> {
        if !self.heuristic_enabled {
            return Vec::new();
        }
        let state = self.state_mut(pane_key);
        match name {
            "kaku_last_cmd" => Self::observe_last_cmd(state, value),
            // WEZTERM_PROG is emitted by default shell integration and is available
            // even when kaku-specific hooks are not installed.
            "WEZTERM_PROG" => Self::observe_wezterm_prog(state, value, user_vars),
            "kaku_last_exit_code" => Self::observe_last_exit_code(state, value, user_vars),
            _ => Vec::new(),
        }
    }

    fn observe_pane_output(
        &mut self,
        pane_key: &str,
        sample: &AgentPaneOutputSample,
    ) -> Vec<AgentEvent> {
        let heuristic_enabled = self.heuristic_enabled;
        let state = self.state_mut(pane_key);
        let codex_context = is_codex_context(sample, state.last_command.clone());
        if !heuristic_enabled {
            let should_read_structured = codex_context
                || state.app_server_turn_active
                || looks_like_app_server_notification_payload(sample.tail_text.as_str());
            if should_read_structured {
                if let Some(events) = Self::observe_app_server_notification(state, sample) {
                    if !events.is_empty() {
                        return events;
                    }
                }
            }
            return Vec::new();
        }

        let should_read_structured =
            codex_context || state.app_server_turn_active || state.approval_prompt_visible;
        if should_read_structured {
            if let Some(events) = Self::observe_app_server_notification(state, sample) {
                if !events.is_empty() {
                    return events;
                }
            }
        }
        if state.app_server_signal_seen {
            return Self::observe_app_server_fallback(state, sample, codex_context);
        }
        Self::observe_approval_prompt(state, sample)
    }
}

#[derive(Debug, Clone)]
struct AppServerNotification {
    signature: String,
    method: String,
    params: Value,
    is_request: bool,
}

impl CodexAdapter {
    pub fn structured_only() -> Self {
        Self {
            pane_state_by_key: HashMap::new(),
            heuristic_enabled: false,
        }
    }

    fn state_mut(&mut self, pane_key: &str) -> &mut PaneRuntimeState {
        self.pane_state_by_key
            .entry(pane_key.to_string())
            .or_default()
    }

    fn observe_app_server_notification(
        state: &mut PaneRuntimeState,
        sample: &AgentPaneOutputSample,
    ) -> Option<Vec<AgentEvent>> {
        let notification = extract_app_server_notification(sample.tail_text.as_str())?;
        let mapped = Self::map_app_server_notification(state, &notification)?;

        state.app_server_signal_seen = true;
        if state
            .last_app_server_signature
            .as_deref()
            .is_some_and(|previous| previous == notification.signature.as_str())
        {
            return Some(Vec::new());
        }
        state.last_app_server_signature = Some(notification.signature);

        Some(mapped)
    }

    fn map_app_server_notification(
        state: &mut PaneRuntimeState,
        notification: &AppServerNotification,
    ) -> Option<Vec<AgentEvent>> {
        if notification.is_request {
            return Self::map_app_server_request(state, notification);
        }

        match notification.method.as_str() {
            "turn/started" => {
                state.app_server_turn_active = true;
                Some(vec![task_started_event()])
            }
            "item/started" | "item/completed" => Some(vec![task_output_event()]),
            "thread/status/changed" => Self::map_thread_status_changed(state, &notification.params),
            "item/autoApprovalReview/started" => {
                let detail = notification
                    .params
                    .get("review")
                    .and_then(|value| value.get("rationale"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| Some("autoApprovalReview".to_string()));
                Some(mark_approval_required(state, detail))
            }
            "item/autoApprovalReview/completed" => {
                let status = notification
                    .params
                    .get("review")
                    .and_then(|value| value.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let approved = matches!(status, "approved" | "inProgress");
                Some(
                    mark_approval_resolved_if_visible(state, approved)
                        .into_iter()
                        .collect(),
                )
            }
            "turn/completed" => Some(Self::map_turn_completed(state, &notification.params)),
            "error" => {
                let will_retry = notification
                    .params
                    .get("willRetry")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if will_retry {
                    return Some(Vec::new());
                }
                let reason = notification
                    .params
                    .get("error")
                    .and_then(|value| value.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| Some("codex app-server error".to_string()));
                Some(vec![task_failed_event(reason)])
            }
            _ => None,
        }
    }

    fn map_thread_status_changed(
        state: &mut PaneRuntimeState,
        params: &Value,
    ) -> Option<Vec<AgentEvent>> {
        let status = params.get("status");
        let status_type = status
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        match status_type {
            "active" => {
                state.app_server_turn_active = true;
                if is_waiting_on_approval_status(status) {
                    return Some(mark_approval_required(
                        state,
                        Some("waitingOnApproval".to_string()),
                    ));
                }
                if is_waiting_on_user_input_status(status) {
                    state.app_server_turn_active = false;
                    state.fallback_active = false;
                    state.fallback_hard_context = false;
                    state.last_exit_signature = None;

                    let mut events = Vec::new();
                    if let Some(event) = mark_approval_resolved_if_visible(state, true) {
                        events.push(event);
                    }
                    events.push(task_completed_event());
                    return Some(events);
                }

                let mut events = Vec::new();
                if let Some(event) = mark_approval_resolved_if_visible(state, true) {
                    events.push(event);
                }
                events.push(task_output_event());
                Some(events)
            }
            "idle" => {
                let was_active = state.app_server_turn_active;
                state.app_server_turn_active = false;

                let mut events = Vec::new();
                if let Some(event) = mark_approval_resolved_if_visible(state, true) {
                    events.push(event);
                }
                if was_active {
                    events.push(task_completed_event());
                }
                Some(events)
            }
            "systemError" => Some(vec![task_failed_event(Some(
                "codex app-server reported systemError".to_string(),
            ))]),
            _ => None,
        }
    }

    fn map_turn_completed(state: &mut PaneRuntimeState, params: &Value) -> Vec<AgentEvent> {
        state.app_server_turn_active = false;

        let turn = params.get("turn");
        let turn_status = turn
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        let mut events = Vec::new();
        match turn_status {
            "completed" => {
                if let Some(event) = mark_approval_resolved_if_visible(state, true) {
                    events.push(event);
                }
                events.push(task_completed_event());
            }
            "failed" => {
                let reason = turn
                    .and_then(|value| value.get("error"))
                    .and_then(|value| value.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| Some("codex turn failed".to_string()));
                if let Some(event) = mark_approval_resolved_if_visible(state, false) {
                    events.push(event);
                }
                events.push(task_failed_event(reason));
            }
            "interrupted" => {
                if let Some(event) = mark_approval_resolved_if_visible(state, false) {
                    events.push(event);
                }
                events.push(task_failed_event(Some(
                    "codex turn interrupted".to_string(),
                )));
            }
            _ => {}
        }
        events
    }

    fn map_app_server_request(
        state: &mut PaneRuntimeState,
        notification: &AppServerNotification,
    ) -> Option<Vec<AgentEvent>> {
        let detail = match notification.method.as_str() {
            "item/commandExecution/requestApproval" => {
                extract_string_field(&notification.params, &["command", "reason"])
                    .or_else(|| Some("commandExecution/requestApproval".to_string()))
            }
            "item/fileChange/requestApproval" => {
                extract_string_field(&notification.params, &["reason", "grantRoot"])
                    .or_else(|| Some("fileChange/requestApproval".to_string()))
            }
            "item/permissions/requestApproval" => {
                extract_string_field(&notification.params, &["reason"])
                    .or_else(|| Some("permissions/requestApproval".to_string()))
            }
            "execCommandApproval" => extract_exec_command_approval_detail(&notification.params)
                .or_else(|| Some("execCommandApproval".to_string())),
            "applyPatchApproval" => {
                extract_string_field(&notification.params, &["reason", "grantRoot"])
                    .or_else(|| Some("applyPatchApproval".to_string()))
            }
            _ => {
                let lower = notification.method.to_ascii_lowercase();
                if !lower.contains("approval") {
                    return None;
                }
                extract_string_field(
                    &notification.params,
                    &["reason", "command", "message", "title", "grantRoot"],
                )
                .or_else(|| Some(notification.method.clone()))
            }
        };

        let mut events = Vec::new();
        if !state.app_server_turn_active {
            state.app_server_turn_active = true;
            events.push(task_started_event());
        }
        events.extend(mark_approval_required(state, detail));
        Some(events)
    }

    fn observe_approval_prompt(
        state: &mut PaneRuntimeState,
        sample: &AgentPaneOutputSample,
    ) -> Vec<AgentEvent> {
        let was_active = state.fallback_active;
        let approval_was_visible = state.approval_prompt_visible;
        let fallback_hard_context = state.fallback_hard_context;
        let approval_detail = extract_approval_prompt(sample.tail_text.as_str());
        let codex_context = is_codex_context(sample, state.last_command.clone());
        let tail_changed = track_fallback_tail_activity(state, sample.tail_text.as_str());
        let waiting_input = is_waiting_for_user_input_prompt(sample.tail_text.as_str());

        if approval_detail.is_none() && !was_active && !approval_was_visible {
            // While idle, don't start a new fallback turn solely because we still have
            // codex context. Require fresh output and exclude stable input prompts.
            if !(codex_context && tail_changed && !waiting_input) {
                return Vec::new();
            }
        }

        let has_explicit_non_codex_context = sample
            .current_command
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty() && !is_codex_command(value))
            || sample
                .foreground_process_name
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty() && !is_codex_process_name(value));

        if was_active
            && fallback_hard_context
            && approval_detail.is_none()
            && !codex_context
            && has_explicit_non_codex_context
        {
            state.fallback_active = false;
            state.fallback_hard_context = false;

            let mut events = Vec::new();
            if let Some(event) = mark_approval_resolved_if_visible(state, true) {
                events.push(event);
            }
            events.push(task_completed_event());
            return events;
        }

        let mut events = Vec::new();
        if !was_active {
            state.fallback_active = true;
            state.fallback_hard_context = codex_context;
            events.push(task_started_event());
            if approval_detail.is_none() {
                events.push(task_output_event());
            }
        } else if codex_context {
            state.fallback_hard_context = true;
        }

        match approval_detail {
            Some(detail) => {
                events.extend(mark_approval_required(state, Some(detail)));
            }
            None => {
                if let Some(event) = mark_approval_resolved_if_visible(state, true) {
                    events.push(event);
                } else if was_active {
                    if waiting_input {
                        state.fallback_active = false;
                        state.fallback_hard_context = false;
                        state.fallback_last_tail_signature = None;
                        events.push(task_completed_event());
                    } else {
                        events.push(task_output_event());
                    }
                }
            }
        }
        events
    }

    fn observe_app_server_approval_fallback(
        state: &mut PaneRuntimeState,
        sample: &AgentPaneOutputSample,
    ) -> Vec<AgentEvent> {
        let approval_detail = extract_approval_prompt(sample.tail_text.as_str());
        match approval_detail {
            Some(detail) => mark_approval_required(state, Some(detail)),
            None => mark_approval_resolved_if_visible(state, true)
                .into_iter()
                .collect(),
        }
    }

    fn observe_app_server_fallback(
        state: &mut PaneRuntimeState,
        sample: &AgentPaneOutputSample,
        codex_context: bool,
    ) -> Vec<AgentEvent> {
        // Keep approval fallback available even after structured transport is seen.
        // Some app-server versions surface approval only in rendered terminal text.
        let mut events = Self::observe_app_server_approval_fallback(state, sample);
        if !events.is_empty() {
            return events;
        }

        if state.app_server_turn_active {
            if codex_context {
                // Structured transport can be sparse in some versions. Emit heartbeat
                // running signals from pane output so we don't get stuck in loading.
                if !state.fallback_active {
                    state.fallback_active = true;
                    return vec![task_output_event()];
                }
                events.push(task_output_event());
                return events;
            }

            // We lost codex context while turn was considered active.
            // Treat as completed and release structured mode.
            state.app_server_turn_active = false;
            state.app_server_signal_seen = false;
            state.fallback_active = false;
            state.fallback_hard_context = false;
            events.push(task_completed_event());
            return events;
        }

        if !codex_context {
            state.app_server_signal_seen = false;
        }
        events
    }

    fn observe_last_cmd(state: &mut PaneRuntimeState, command: &str) -> Vec<AgentEvent> {
        let normalized = command.trim().to_string();
        if normalized.is_empty() {
            return Vec::new();
        }

        state.command_generation = state.command_generation.saturating_add(1);
        state.last_command = Some(normalized.clone());
        state.last_app_server_signature = None;
        state.app_server_turn_active = false;
        state.app_server_signal_seen = false;
        state.fallback_active = false;
        state.fallback_hard_context = false;
        state.fallback_last_tail_signature = None;

        if !is_codex_command(normalized.as_str()) {
            return Vec::new();
        }

        state.fallback_active = true;
        state.fallback_hard_context = true;
        vec![task_started_event()]
    }

    fn observe_wezterm_prog(
        state: &mut PaneRuntimeState,
        command: &str,
        user_vars: &HashMap<String, String>,
    ) -> Vec<AgentEvent> {
        let normalized = command.trim();
        if normalized.is_empty() {
            if !state.fallback_active {
                return Vec::new();
            }

            state.fallback_active = false;
            state.app_server_turn_active = false;
            state.fallback_hard_context = false;
            state.fallback_last_tail_signature = None;

            let mut events = Vec::new();
            if let Some(event) = mark_approval_resolved_if_visible(state, true) {
                events.push(event);
            }
            events.push(task_completed_event());
            return events;
        }

        // Prefer kaku_last_cmd when present to avoid double-start events.
        let has_kaku_last_cmd = user_vars
            .get("kaku_last_cmd")
            .is_some_and(|value| !value.trim().is_empty());
        if has_kaku_last_cmd {
            state.last_command = Some(normalized.to_string());
            return Vec::new();
        }

        Self::observe_last_cmd(state, normalized)
    }

    fn observe_last_exit_code(
        state: &mut PaneRuntimeState,
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
            .or_else(|| state.last_command.clone());

        let Some(command) = command else {
            return Vec::new();
        };

        if !is_codex_command(command.as_str()) && !state.fallback_active {
            return Vec::new();
        }

        let signature = format!("{}\0{}\0{}", state.command_generation, command, exit_code);
        if state
            .last_exit_signature
            .as_deref()
            .is_some_and(|previous| previous == signature.as_str())
        {
            return Vec::new();
        }

        state.last_exit_signature = Some(signature);
        state.app_server_turn_active = false;
        state.fallback_active = false;
        state.fallback_hard_context = false;
        state.fallback_last_tail_signature = None;

        if exit_code == 0 {
            vec![task_completed_event()]
        } else {
            vec![task_failed_event(Some(format!(
                "codex exited with status {}",
                exit_code
            )))]
        }
    }
}

fn mark_approval_required(state: &mut PaneRuntimeState, detail: Option<String>) -> Vec<AgentEvent> {
    let detail = detail
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(160).collect::<String>());

    let was_visible = state.approval_prompt_visible;
    state.approval_prompt_visible = true;

    let prev_detail = if let Some(value) = detail.clone() {
        state.approval_prompt_detail.replace(value)
    } else {
        state.approval_prompt_detail.take()
    };

    if !was_visible || prev_detail.as_deref() != detail.as_deref() {
        vec![approval_required_event(detail)]
    } else {
        Vec::new()
    }
}

fn mark_approval_resolved_if_visible(
    state: &mut PaneRuntimeState,
    approved: bool,
) -> Option<AgentEvent> {
    if !state.approval_prompt_visible {
        return None;
    }

    state.approval_prompt_visible = false;
    state.approval_prompt_detail = None;
    Some(approval_resolved_event(approved))
}

fn is_waiting_on_approval_status(status: Option<&Value>) -> bool {
    let Some(status) = status else {
        return false;
    };

    let status_type = status
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if status_type.contains("approval") {
        return true;
    }

    if status
        .get("activeFlags")
        .and_then(Value::as_array)
        .is_some_and(|flags| {
            flags.iter().filter_map(Value::as_str).any(|flag| {
                flag.eq_ignore_ascii_case("waitingOnApproval")
                    || flag.to_ascii_lowercase().contains("approval")
            })
        })
    {
        return true;
    }

    status
        .get("waitReason")
        .and_then(Value::as_str)
        .is_some_and(|value| value.to_ascii_lowercase().contains("approval"))
}

fn is_waiting_on_user_input_status(status: Option<&Value>) -> bool {
    let Some(status) = status else {
        return false;
    };

    let status_type = status
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if status_type.contains("waitingoninput")
        || status_type.contains("waitingonuserinput")
        || status_type.contains("waiting_for_input")
        || status_type.contains("waiting_for_user_input")
        || status_type.contains("awaitinginput")
        || status_type.contains("awaitinguserinput")
        || status_type.contains("userinput")
    {
        return true;
    }

    if status
        .get("activeFlags")
        .and_then(Value::as_array)
        .is_some_and(|flags| {
            flags.iter().filter_map(Value::as_str).any(|flag| {
                let flag_lower = flag.to_ascii_lowercase();
                if flag_lower.contains("approval") {
                    return false;
                }
                flag_lower.contains("waitingoninput")
                    || flag_lower.contains("waitingonuserinput")
                    || flag_lower.contains("waiting_for_input")
                    || flag_lower.contains("waiting_for_user_input")
                    || flag_lower.contains("awaitinginput")
                    || flag_lower.contains("awaitinguserinput")
                    || flag_lower.contains("userinput")
                    || flag_lower.contains("awaitinguser")
                    || flag_lower.contains("waitingforuser")
            })
        })
    {
        return true;
    }

    status
        .get("waitReason")
        .and_then(Value::as_str)
        .is_some_and(|value| {
            let lower = value.to_ascii_lowercase();
            if lower.contains("approval") {
                return false;
            }
            lower.contains("input")
                || lower.contains("user")
                || lower.contains("respond")
                || lower.contains("reply")
        })
}

fn task_started_event() -> AgentEvent {
    AgentEvent::TaskStarted {
        provider: CODEX_PROVIDER.to_string(),
    }
}

fn task_output_event() -> AgentEvent {
    AgentEvent::TaskOutput {
        provider: CODEX_PROVIDER.to_string(),
    }
}

fn task_completed_event() -> AgentEvent {
    AgentEvent::TaskCompleted {
        provider: CODEX_PROVIDER.to_string(),
    }
}

fn task_failed_event(reason: Option<String>) -> AgentEvent {
    AgentEvent::TaskFailed {
        provider: CODEX_PROVIDER.to_string(),
        reason,
    }
}

fn approval_required_event(detail: Option<String>) -> AgentEvent {
    AgentEvent::ApprovalRequired {
        provider: CODEX_PROVIDER.to_string(),
        detail,
    }
}

fn approval_resolved_event(approved: bool) -> AgentEvent {
    AgentEvent::ApprovalResolved {
        provider: CODEX_PROVIDER.to_string(),
        approved,
    }
}

fn is_codex_context(sample: &AgentPaneOutputSample, known_command: Option<String>) -> bool {
    let current_command = sample
        .current_command
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .or_else(|| known_command.as_deref());

    current_command.is_some_and(is_codex_command)
        || sample
            .foreground_process_name
            .as_deref()
            .is_some_and(is_codex_process_name)
}

fn is_codex_command(command: &str) -> bool {
    shared_is_codex_like_command(command)
}

fn is_codex_process_name(process_name: &str) -> bool {
    shared_is_codex_process_name(process_name)
}

fn extract_approval_prompt(tail_text: &str) -> Option<String> {
    for raw_line in tail_text.lines().rev().take(18) {
        let stripped = strip_ansi_control(raw_line);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }

        let lower = line.to_ascii_lowercase();
        let has_approve = lower.contains("approve")
            || lower.contains("approval")
            || line.contains("批准")
            || line.contains("审批");
        let has_confirm = lower.contains("confirm")
            || lower.contains("allow")
            || lower.contains("permission")
            || lower.contains("proceed")
            || lower.contains("continue")
            || line.contains("允许")
            || line.contains("确认")
            || line.contains("继续");
        let has_prompt_hint = lower.contains("[y/n]")
            || lower.contains("(y/n)")
            || lower.contains("[y/n")
            || lower.contains("(y/n")
            || lower.contains("yes/no")
            || lower.contains("press enter")
            || lower.contains("approve and continue")
            || lower.contains("approve once")
            || lower.contains("always allow")
            || lower.contains("deny")
            || lower.contains("reject")
            || line.contains("允许一次")
            || line.contains("总是允许")
            || line.contains("拒绝");
        let has_question_mark = line.contains('?') || line.contains('？');
        let has_action_word =
            lower.contains("command") || lower.contains("action") || lower.contains("tool");
        let has_strong_phrase = lower.contains("approve and continue")
            || lower.contains("always allow")
            || lower.contains("approve once")
            || line.contains("允许一次")
            || line.contains("总是允许");

        if (has_approve || has_confirm || (has_question_mark && has_action_word))
            && (has_prompt_hint || has_question_mark || has_strong_phrase)
        {
            return Some(line.chars().take(160).collect());
        }
    }
    None
}

fn track_fallback_tail_activity(state: &mut PaneRuntimeState, tail_text: &str) -> bool {
    let signature = strip_ansi_control(tail_text)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if signature.is_empty() {
        return false;
    }

    if state
        .fallback_last_tail_signature
        .as_deref()
        .is_some_and(|previous| previous == signature.as_str())
    {
        return false;
    }

    state.fallback_last_tail_signature = Some(signature);
    true
}

fn is_waiting_for_user_input_prompt(tail_text: &str) -> bool {
    for raw_line in tail_text.lines().rev().take(20) {
        let stripped = strip_ansi_control(raw_line);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }

        let lower = line.to_ascii_lowercase();
        if lower.contains("press enter to send")
            || lower.contains("enter to send")
            || lower.contains("shift+enter")
            || lower.contains("ctrl+j")
            || lower.contains("type a message")
            || lower.contains("ask anything")
            || line.contains("按回车发送")
            || line.contains("输入消息")
        {
            return true;
        }

        let trimmed = line.trim_start();
        if matches!(
            trimmed.chars().next(),
            Some('>') | Some('›') | Some('❯') | Some('→') | Some('▌') | Some('█')
        ) {
            return true;
        }
    }

    false
}

fn extract_app_server_notification(tail_text: &str) -> Option<AppServerNotification> {
    for raw_line in tail_text.lines().rev().take(24) {
        let stripped = strip_ansi_control(raw_line);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }

        let payload_json = extract_json_object_segment(line)?;
        let payload: Value = match serde_json::from_str(payload_json) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let method = match payload.get("method").and_then(Value::as_str) {
            Some(value) if !value.trim().is_empty() => value.to_string(),
            _ => continue,
        };

        let is_request = payload
            .get("id")
            .map(|value| !value.is_null())
            .unwrap_or(false);
        let params = payload.get("params").cloned().unwrap_or(Value::Null);

        return Some(AppServerNotification {
            signature: payload_json.to_string(),
            method,
            params,
            is_request,
        });
    }
    None
}

fn looks_like_app_server_notification_payload(tail_text: &str) -> bool {
    let lower = tail_text.to_ascii_lowercase();
    lower.contains("\"method\":\"turn/")
        || lower.contains("\"method\": \"turn/")
        || lower.contains("\"method\":\"thread/")
        || lower.contains("\"method\": \"thread/")
        || lower.contains("\"method\":\"item/")
        || lower.contains("\"method\": \"item/")
        || lower.contains("\"method\":\"serverrequest/")
        || lower.contains("\"method\": \"serverrequest/")
        || lower.contains("\"method\":\"error\"")
        || lower.contains("\"method\": \"error\"")
}

fn extract_string_field(params: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| params.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(|value| value.chars().take(160).collect::<String>())
}

fn extract_exec_command_approval_detail(params: &Value) -> Option<String> {
    if let Some(command) = params
        .get("command")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(command.chars().take(160).collect());
    }

    extract_string_field(params, &["reason"])
}

fn extract_json_object_segment(line: &str) -> Option<&str> {
    let start = line.find('{')?;
    let end = line.rfind('}')?;
    if start >= end {
        return None;
    }
    line.get(start..=end)
}

fn strip_ansi_control(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0usize;

    while idx < bytes.len() {
        if bytes[idx] != 0x1b {
            out.push(bytes[idx]);
            idx += 1;
            continue;
        }

        idx += 1;
        if idx >= bytes.len() {
            break;
        }

        match bytes[idx] {
            b'[' => {
                idx += 1;
                while idx < bytes.len() {
                    let byte = bytes[idx];
                    idx += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            b']' => {
                idx += 1;
                while idx < bytes.len() {
                    if bytes[idx] == 0x07 {
                        idx += 1;
                        break;
                    }
                    if bytes[idx] == 0x1b && idx + 1 < bytes.len() && bytes[idx + 1] == b'\\' {
                        idx += 2;
                        break;
                    }
                    idx += 1;
                }
            }
            _ => {
                idx += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}
#[cfg(test)]
mod tests {
    use super::{
        extract_app_server_notification, extract_approval_prompt, is_codex_command,
        is_waiting_on_user_input_status, looks_like_app_server_notification_payload, CodexAdapter,
    };
    use crate::agent_status::adapters::{AgentAdapter, AgentPaneOutputSample};
    use crate::agent_status::events::AgentEvent;
    use serde_json::json;
    use std::collections::HashMap;

    fn codex_sample(tail_text: &str) -> AgentPaneOutputSample {
        AgentPaneOutputSample {
            tail_text: tail_text.to_string(),
            current_command: Some("codex".to_string()),
            foreground_process_name: Some("codex".to_string()),
        }
    }

    fn non_codex_sample(tail_text: &str) -> AgentPaneOutputSample {
        AgentPaneOutputSample {
            tail_text: tail_text.to_string(),
            current_command: Some("c".to_string()),
            foreground_process_name: Some("tmux".to_string()),
        }
    }

    #[test]
    fn detects_codex_command_variants() {
        assert!(is_codex_command("codex run"));
        assert!(is_codex_command("/usr/local/bin/codex run"));
        assert!(is_codex_command("FOO=bar codex run"));
        assert!(is_codex_command("command codex run"));
        assert!(is_codex_command("builtin codex run"));
        assert!(is_codex_command("nocorrect codex run"));
        assert!(is_codex_command("time codex run"));
        assert!(is_codex_command("env FOO=bar codex run"));
        assert!(!is_codex_command("claude --print"));
        assert!(!is_codex_command("echo codex"));
        assert!(!is_codex_command(""));
    }

    #[test]
    fn emits_started_and_completed_for_codex_command() {
        let mut adapter = CodexAdapter::default();
        let started =
            adapter.observe_user_var("1", "kaku_last_cmd", "codex --help", &HashMap::new());
        assert_eq!(
            started,
            vec![AgentEvent::TaskStarted {
                provider: "codex".to_string()
            }]
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
    fn wezterm_prog_drives_started_when_kaku_last_cmd_is_missing() {
        let mut adapter = CodexAdapter::default();
        let started =
            adapter.observe_user_var("1", "WEZTERM_PROG", "codex --help", &HashMap::new());
        assert_eq!(
            started,
            vec![AgentEvent::TaskStarted {
                provider: "codex".to_string()
            }]
        );

        let completed = adapter.observe_user_var("1", "kaku_last_exit_code", "0", &HashMap::new());
        assert_eq!(
            completed,
            vec![AgentEvent::TaskCompleted {
                provider: "codex".to_string()
            }]
        );
    }

    #[test]
    fn wezterm_prog_empty_completes_active_codex_turn() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "WEZTERM_PROG", "codex run", &HashMap::new());

        let completed = adapter.observe_user_var("1", "WEZTERM_PROG", "", &HashMap::new());
        assert_eq!(
            completed,
            vec![AgentEvent::TaskCompleted {
                provider: "codex".to_string(),
            }]
        );
    }

    #[test]
    fn wezterm_prog_does_not_duplicate_when_kaku_last_cmd_exists() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex --help", &HashMap::new());
        let mut vars = HashMap::new();
        vars.insert("kaku_last_cmd".to_string(), "codex --help".to_string());

        let wezterm = adapter.observe_user_var("1", "WEZTERM_PROG", "codex --help", &vars);
        assert!(wezterm.is_empty());
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

    #[test]
    fn extracts_approval_prompt_line() {
        let text = "step 1\nApprove this action? [y/N]\n";
        assert_eq!(
            extract_approval_prompt(text).as_deref(),
            Some("Approve this action? [y/N]")
        );
    }

    #[test]
    fn emits_approval_required_and_resolved_from_output() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());

        let required =
            adapter.observe_pane_output("1", &codex_sample("Approve command execution? [y/N]"));
        assert_eq!(
            required,
            vec![AgentEvent::ApprovalRequired {
                provider: "codex".to_string(),
                detail: Some("Approve command execution? [y/N]".to_string()),
            }]
        );

        let resolved = adapter.observe_pane_output("1", &codex_sample("running task..."));
        assert_eq!(
            resolved,
            vec![AgentEvent::ApprovalResolved {
                provider: "codex".to_string(),
                approved: true,
            }]
        );
    }

    #[test]
    fn extracts_approval_prompt_without_yes_no_hint() {
        let text = "Please review\nApprove and continue?\n";
        assert_eq!(
            extract_approval_prompt(text).as_deref(),
            Some("Approve and continue?")
        );
    }

    #[test]
    fn extracts_app_server_notification_line() {
        let payload = r#"{"id":1,"method":"thread/start","params":{}}"#;
        let notification = r#"{"method":"turn/started","params":{"threadId":"t1","turn":{"id":"x","status":"inProgress","items":[],"error":null}}}"#;
        let text = format!("noise\n{}\n{}\n", payload, notification);
        let extracted =
            extract_app_server_notification(text.as_str()).expect("must extract notification");
        assert_eq!(extracted.method, "turn/started");
    }

    #[test]
    fn extracts_app_server_notification_with_ansi_prefix() {
        let notification =
            "\u{1b}[32m[app]\u{1b}[0m {\"method\":\"thread/status/changed\",\"params\":{\"status\":{\"type\":\"active\",\"activeFlags\":[\"waitingOnApproval\"]}}}";
        let extracted = extract_app_server_notification(notification)
            .expect("must extract notification with prefix");
        assert_eq!(extracted.method, "thread/status/changed");
    }

    #[test]
    fn extracts_app_server_request_with_id() {
        let request = r#"{"id":42,"method":"item/commandExecution/requestApproval","params":{"command":"git push","reason":"network access"}}"#;
        let extracted = extract_app_server_notification(request).expect("must extract request");
        assert_eq!(extracted.method, "item/commandExecution/requestApproval");
        assert!(extracted.is_request);
    }

    #[test]
    fn emits_structured_events_from_app_server_turn_lifecycle() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());

        let started = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"turn/started","params":{"threadId":"t1","turn":{"id":"x","status":"inProgress","items":[],"error":null}}}"#,
            ),
        );
        assert_eq!(
            started,
            vec![AgentEvent::TaskStarted {
                provider: "codex".to_string()
            }]
        );

        let completed = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"turn/completed","params":{"threadId":"t1","turn":{"id":"x","status":"completed","items":[],"error":null}}}"#,
            ),
        );
        assert_eq!(
            completed,
            vec![AgentEvent::TaskCompleted {
                provider: "codex".to_string()
            }]
        );
    }

    #[test]
    fn emits_approval_events_from_app_server_active_flags() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());

        let _ = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"turn/started","params":{"threadId":"t1","turn":{"id":"x","status":"inProgress","items":[],"error":null}}}"#,
            ),
        );

        let required = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"thread/status/changed","params":{"threadId":"t1","status":{"type":"active","activeFlags":["waitingOnApproval"]}}}"#,
            ),
        );
        assert_eq!(
            required,
            vec![AgentEvent::ApprovalRequired {
                provider: "codex".to_string(),
                detail: Some("waitingOnApproval".to_string()),
            }]
        );

        let resolved = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"thread/status/changed","params":{"threadId":"t1","status":{"type":"active","activeFlags":[]}}}"#,
            ),
        );
        assert_eq!(
            resolved,
            vec![
                AgentEvent::ApprovalResolved {
                    provider: "codex".to_string(),
                    approved: true,
                },
                AgentEvent::TaskOutput {
                    provider: "codex".to_string(),
                }
            ]
        );
    }

    #[test]
    fn user_input_waiting_status_completes_loading_state() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());

        let _ = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"turn/started","params":{"threadId":"t1","turn":{"id":"x","status":"inProgress","items":[],"error":null}}}"#,
            ),
        );

        let done = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"thread/status/changed","params":{"threadId":"t1","status":{"type":"active","activeFlags":["waitingOnInput"]}}}"#,
            ),
        );
        assert_eq!(
            done,
            vec![AgentEvent::TaskCompleted {
                provider: "codex".to_string(),
            }]
        );
    }

    #[test]
    fn detects_user_input_waiting_without_confusing_approval() {
        assert!(is_waiting_on_user_input_status(Some(&json!({
            "type": "active",
            "activeFlags": ["waitingOnInput"]
        }))));
        assert!(is_waiting_on_user_input_status(Some(&json!({
            "type": "active",
            "waitReason": "waiting for user input"
        }))));
        assert!(!is_waiting_on_user_input_status(Some(&json!({
            "type": "active",
            "activeFlags": ["waitingOnApproval"]
        }))));
        assert!(is_waiting_on_user_input_status(Some(&json!({
            "type": "active",
            "activeFlags": ["waitingOnUserInput"]
        }))));
    }

    #[test]
    fn detects_app_server_json_payload_in_tail_text() {
        assert!(looks_like_app_server_notification_payload(
            r#"{"method":"turn/started","params":{"turn":{"id":"x"}}}"#
        ));
        assert!(looks_like_app_server_notification_payload(
            r#"noise {"method":"thread/status/changed","params":{"status":{"type":"active"}}}"#
        ));
        assert!(!looks_like_app_server_notification_payload("plain terminal output"));
    }

    #[test]
    fn emits_approval_events_from_app_server_request_methods() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());

        let required = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"id":"r1","method":"item/commandExecution/requestApproval","params":{"threadId":"t1","turnId":"u1","itemId":"i1","command":"git push","reason":"network access"}}"#,
            ),
        );
        assert_eq!(
            required,
            vec![
                AgentEvent::TaskStarted {
                    provider: "codex".to_string(),
                },
                AgentEvent::ApprovalRequired {
                    provider: "codex".to_string(),
                    detail: Some("git push".to_string()),
                },
            ]
        );
    }

    #[test]
    fn ignores_duplicate_app_server_notification() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());
        let payload = r#"{"method":"turn/started","params":{"threadId":"t1","turn":{"id":"x","status":"inProgress","items":[],"error":null}}}"#;
        let first = adapter.observe_pane_output("1", &codex_sample(payload));
        let second = adapter.observe_pane_output("1", &codex_sample(payload));
        assert!(!first.is_empty());
        assert_eq!(
            second,
            vec![AgentEvent::TaskOutput {
                provider: "codex".to_string(),
            }]
        );
    }

    #[test]
    fn maps_turn_failed_to_task_failed_reason() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());

        let failed = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"turn/completed","params":{"threadId":"t1","turn":{"id":"x","status":"failed","items":[],"error":{"message":"boom"}}}}"#,
            ),
        );
        assert_eq!(
            failed,
            vec![AgentEvent::TaskFailed {
                provider: "codex".to_string(),
                reason: Some("boom".to_string()),
            }]
        );
    }

    #[test]
    fn unknown_json_method_does_not_block_prompt_heuristic() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());

        let tail = r#"{"method":"custom/not-app-server","params":{"x":1}}
Approve command execution? [y/N]"#;
        let events = adapter.observe_pane_output("1", &codex_sample(tail));
        assert_eq!(
            events,
            vec![AgentEvent::ApprovalRequired {
                provider: "codex".to_string(),
                detail: Some("Approve command execution? [y/N]".to_string()),
            }]
        );
    }

    #[test]
    fn fallback_output_can_drive_running_and_completion_without_codex_cmd_match() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "c", &HashMap::new());

        let running = adapter.observe_pane_output("1", &codex_sample("thinking..."));
        assert_eq!(
            running,
            vec![
                AgentEvent::TaskStarted {
                    provider: "codex".to_string(),
                },
                AgentEvent::TaskOutput {
                    provider: "codex".to_string(),
                }
            ]
        );

        let mut vars = HashMap::new();
        vars.insert("kaku_last_cmd".to_string(), "c".to_string());
        let completed = adapter.observe_user_var("1", "kaku_last_exit_code", "0", &vars);
        assert_eq!(
            completed,
            vec![AgentEvent::TaskCompleted {
                provider: "codex".to_string(),
            }]
        );
    }

    #[test]
    fn app_server_stream_suppresses_text_fallback_churn() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());
        let _ = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"turn/started","params":{"threadId":"t1","turn":{"id":"x","status":"inProgress","items":[],"error":null}}}"#,
            ),
        );

        let events = adapter.observe_pane_output("1", &codex_sample("still working..."));
        assert_eq!(
            events,
            vec![AgentEvent::TaskOutput {
                provider: "codex".to_string(),
            }]
        );
    }

    #[test]
    fn ignores_structured_notification_outside_codex_context() {
        let mut adapter = CodexAdapter::default();
        let events = adapter.observe_pane_output(
            "1",
            &non_codex_sample(
                r#"{"method":"turn/started","params":{"threadId":"t1","turn":{"id":"x","status":"inProgress","items":[],"error":null}}}"#,
            ),
        );
        assert!(events.is_empty());
    }

    #[test]
    fn app_server_mode_still_emits_text_approval_events() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());
        let _ = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"method":"turn/started","params":{"threadId":"t1","turn":{"id":"x","status":"inProgress","items":[],"error":null}}}"#,
            ),
        );

        let required = adapter.observe_pane_output(
            "1",
            &codex_sample("Press enter to confirm or esc to cancel"),
        );
        assert_eq!(
            required,
            vec![AgentEvent::ApprovalRequired {
                provider: "codex".to_string(),
                detail: Some("Press enter to confirm or esc to cancel".to_string()),
            }]
        );

        let resolved = adapter.observe_pane_output("1", &codex_sample("still working..."));
        assert_eq!(
            resolved,
            vec![AgentEvent::ApprovalResolved {
                provider: "codex".to_string(),
                approved: true,
            }]
        );
    }

    #[test]
    fn maps_generic_app_server_approval_method() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());

        let events = adapter.observe_pane_output(
            "1",
            &codex_sample(
                r#"{"id":"req1","method":"item/tool/requestApprovalV2","params":{"reason":"need user approval"}}"#,
            ),
        );
        assert_eq!(
            events,
            vec![
                AgentEvent::TaskStarted {
                    provider: "codex".to_string(),
                },
                AgentEvent::ApprovalRequired {
                    provider: "codex".to_string(),
                    detail: Some("need user approval".to_string()),
                },
            ]
        );
    }

    #[test]
    fn explicit_non_codex_context_completes_hard_context_turn() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());
        let _ = adapter.observe_pane_output("1", &codex_sample("working..."));

        let completed = adapter.observe_pane_output("1", &non_codex_sample("$ "));
        assert_eq!(
            completed,
            vec![AgentEvent::TaskCompleted {
                provider: "codex".to_string(),
            }]
        );
    }

    #[test]
    fn prompt_from_idle_emits_running_then_need_approve() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "c", &HashMap::new());

        let events = adapter.observe_pane_output("1", &codex_sample("Approve this action? [y/N]"));
        assert_eq!(
            events,
            vec![
                AgentEvent::TaskStarted {
                    provider: "codex".to_string(),
                },
                AgentEvent::ApprovalRequired {
                    provider: "codex".to_string(),
                    detail: Some("Approve this action? [y/N]".to_string()),
                },
            ]
        );
    }

    #[test]
    fn approval_prompt_bootstraps_context_when_command_and_process_are_ambiguous() {
        let mut adapter = CodexAdapter::default();
        let events =
            adapter.observe_pane_output("1", &non_codex_sample("Approve this action? [y/N]"));
        assert_eq!(
            events,
            vec![
                AgentEvent::TaskStarted {
                    provider: "codex".to_string(),
                },
                AgentEvent::ApprovalRequired {
                    provider: "codex".to_string(),
                    detail: Some("Approve this action? [y/N]".to_string()),
                },
            ]
        );
    }

    #[test]
    fn bootstrapped_approval_context_keeps_emitting_running_without_codex_markers() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_pane_output("1", &non_codex_sample("Approve this action? [y/N]"));

        let resolved = adapter.observe_pane_output("1", &non_codex_sample("continuing..."));
        assert_eq!(
            resolved,
            vec![AgentEvent::ApprovalResolved {
                provider: "codex".to_string(),
                approved: true,
            }]
        );

        let running = adapter.observe_pane_output("1", &non_codex_sample("still working..."));
        assert_eq!(
            running,
            vec![AgentEvent::TaskOutput {
                provider: "codex".to_string(),
            }]
        );
    }

    #[test]
    fn waiting_input_prompt_does_not_retrigger_started_while_idle() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());

        let _ = adapter.observe_pane_output("1", &codex_sample("processing..."));
        let done = adapter.observe_pane_output("1", &codex_sample(">"));
        assert_eq!(
            done,
            vec![AgentEvent::TaskCompleted {
                provider: "codex".to_string(),
            }]
        );

        let idle_probe = adapter.observe_pane_output("1", &codex_sample(">"));
        assert!(idle_probe.is_empty());
    }

    #[test]
    fn idle_restarts_only_on_new_output_after_waiting_input() {
        let mut adapter = CodexAdapter::default();
        let _ = adapter.observe_user_var("1", "kaku_last_cmd", "codex", &HashMap::new());
        let _ = adapter.observe_pane_output("1", &codex_sample("processing..."));
        let _ = adapter.observe_pane_output("1", &codex_sample(">"));

        let restarted = adapter.observe_pane_output("1", &codex_sample("new task output"));
        assert_eq!(
            restarted,
            vec![
                AgentEvent::TaskStarted {
                    provider: "codex".to_string(),
                },
                AgentEvent::TaskOutput {
                    provider: "codex".to_string(),
                }
            ]
        );
    }
}
