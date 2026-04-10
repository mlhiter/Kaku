use super::{TermWindow, TermWindowNotif};
use crate::agent_status::events::{AgentEvent, SessionStatusConfidence, SessionStatusSource};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use mux::pane::PaneId;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use url::Url;
use window::WindowOps;

const CODEX_PROVIDER: &str = "codex";
const CODEX_APP_SERVER_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const CODEX_APP_SERVER_RECONNECT_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub(super) struct CodexAppServerLaunchContext {
    pub pane_id: PaneId,
}

#[derive(Debug, Clone)]
pub(super) struct CodexAppServerEvent {
    pane_id: PaneId,
    events: Vec<AgentEvent>,
    thread_id: Option<String>,
}

impl TermWindow {
    pub(super) fn maybe_handle_codex_managed_start(
        &mut self,
        pane_id: PaneId,
        name: &str,
        value: &str,
        user_vars: &HashMap<String, String>,
    ) -> bool {
        if name != "kaku_codex_app_server_ws" {
            return false;
        }

        let Some(ws_url) = value
            .split_whitespace()
            .next()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            self.stop_codex_app_server_channel(pane_id);
            return false;
        };
        if ws_url == "__stop__" {
            self.stop_codex_app_server_channel(pane_id);
            return true;
        }
        if !ws_url.starts_with("ws://") && !ws_url.starts_with("wss://") {
            return false;
        }

        // Optional per-pane bearer token env key, emitted by managed wrappers.
        let token = user_vars
            .get("kaku_codex_app_server_token")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        let context = CodexAppServerLaunchContext { pane_id };
        self.start_or_replace_codex_app_server_channel(context, ws_url.to_string(), token);
        true
    }

    pub(super) fn stop_codex_app_server_channel(&mut self, pane_id: PaneId) {
        if let Some(channel) = self.codex_app_server_channels_by_pane.remove(&pane_id) {
            channel.stop_flag.store(true, Ordering::Relaxed);
            log::info!(
                "codex app-server channel stopped pane={} ws={}",
                pane_id,
                channel.url
            );
        }
    }

    pub(super) fn stop_all_codex_app_server_channels(&mut self) {
        let channels = std::mem::take(&mut self.codex_app_server_channels_by_pane);
        for (pane_id, channel) in channels {
            channel.stop_flag.store(true, Ordering::Relaxed);
            log::info!(
                "codex app-server channel stopped pane={} ws={}",
                pane_id,
                channel.url
            );
        }
    }

    fn start_or_replace_codex_app_server_channel(
        &mut self,
        context: CodexAppServerLaunchContext,
        ws_url: String,
        auth_token: Option<String>,
    ) {
        if let Some(existing) = self
            .codex_app_server_channels_by_pane
            .get(&context.pane_id)
            .cloned()
        {
            if existing.url == ws_url {
                return;
            }
            self.stop_codex_app_server_channel(context.pane_id);
        }

        let stop_flag = Arc::new(AtomicBool::new(false));
        self.codex_app_server_channels_by_pane.insert(
            context.pane_id,
            super::CodexAppServerPaneChannel {
                url: ws_url.clone(),
                stop_flag: Arc::clone(&stop_flag),
            },
        );

        let Some(window) = self.window.clone() else {
            return;
        };
        spawn_codex_app_server_task(window, context, ws_url, auth_token, stop_flag);
    }

    pub(super) fn handle_codex_app_server_event(&mut self, event: CodexAppServerEvent) {
        if let Some(thread_id) = event.thread_id.as_deref() {
            if let Some((project_id, session_id)) = self.sidebar_session_binding_for_pane(event.pane_id) {
                if let Err(err) = self.sidebar_set_session_codex_thread_id(
                    project_id.as_str(),
                    session_id.as_str(),
                    thread_id,
                ) {
                    log::warn!(
                        "codex app-server: failed to persist thread id for {}/{}: {:#}",
                        project_id,
                        session_id,
                        err
                    );
                }
            }
        }

        if event.events.is_empty() {
            return;
        }
        self.process_agent_events_for_pane(
            event.pane_id,
            event.events,
            SessionStatusSource::Structured,
            SessionStatusConfidence::High,
        );
    }
}

fn spawn_codex_app_server_task(
    window: window::Window,
    context: CodexAppServerLaunchContext,
    ws_url: String,
    auth_token: Option<String>,
    stop_flag: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name(format!(
            "codex-app-server-pane-{}",
            context.pane_id.as_usize()
        ))
        .spawn(move || {
            let rt = TokioRuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .expect("codex app-server tokio runtime");
            rt.block_on(async move {
                let mut request_id: u64 = 1;
                while !stop_flag.load(Ordering::Relaxed) {
                    match run_single_ws_session(
                        &window,
                        &context,
                        &ws_url,
                        auth_token.as_deref(),
                        &stop_flag,
                        &mut request_id,
                    )
                    .await
                    {
                        Ok(()) => break,
                        Err(err) => {
                            if stop_flag.load(Ordering::Relaxed) {
                                break;
                            }
                            log::warn!(
                                "codex app-server channel disconnected pane={} ws={} err={:#}",
                                context.pane_id,
                                ws_url,
                                err
                            );
                            tokio::time::sleep(CODEX_APP_SERVER_RECONNECT_DELAY).await;
                        }
                    }
                }
            });
        })
        .expect("spawn codex app-server worker");
}

async fn run_single_ws_session(
    window: &window::Window,
    context: &CodexAppServerLaunchContext,
    ws_url: &str,
    auth_token: Option<&str>,
    stop_flag: &Arc<AtomicBool>,
    request_id: &mut u64,
) -> Result<()> {
    let url = Url::parse(ws_url).with_context(|| format!("invalid ws url: {}", ws_url))?;
    let mut request = url
        .as_str()
        .into_client_request()
        .context("build websocket request for codex app-server")?;
    if let Some(token) = auth_token {
        let header = HeaderValue::from_str(format!("Bearer {token}").as_str())
            .context("invalid auth token header value")?;
        request.headers_mut().insert(AUTHORIZATION, header);
    }

    let connect = tokio::time::timeout(CODEX_APP_SERVER_CONNECT_TIMEOUT, connect_async(request))
        .await
        .context("connect timeout")??;
    let (ws_stream, _) = connect;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    let initialize_payload = json!({
        "id": next_request_id(request_id),
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "kaku-gui",
                "version": config::wezterm_version(),
            },
            "capabilities": {},
        }
    })
    .to_string();
    ws_tx
        .send(WsMessage::Text(initialize_payload.into()))
        .await
        .context("send initialize")?;
    ws_tx
        .send(WsMessage::Text(
            json!({"method":"initialized"}).to_string().into(),
        ))
        .await
        .context("send initialized")?;

    while !stop_flag.load(Ordering::Relaxed) {
        let Some(incoming) = ws_rx.next().await else {
            break;
        };
        let incoming = incoming.context("read websocket message")?;
        let text = match incoming {
            WsMessage::Text(text) => text,
            WsMessage::Binary(_) => continue,
            WsMessage::Ping(payload) => {
                ws_tx.send(WsMessage::Pong(payload)).await.ok();
                continue;
            }
            WsMessage::Pong(_) => continue,
            WsMessage::Frame(_) => continue,
            WsMessage::Close(_) => break,
        };

        let payload: Value = match serde_json::from_str(text.as_str()) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if let Some(id) = payload.get("id").cloned() {
            if let Some(method) = payload.get("method").and_then(Value::as_str) {
                if let Some(response) = handle_server_request(method, payload.get("params")) {
                    let response_payload = json!({
                        "id": id,
                        "result": response,
                    })
                    .to_string();
                    ws_tx
                        .send(WsMessage::Text(response_payload.into()))
                        .await
                        .ok();
                }
            }
            continue;
        }

        let Some(method) = payload.get("method").and_then(Value::as_str) else {
            continue;
        };
        let params = payload.get("params");
        let thread_id = extract_thread_id(params);
        let Some(events) = map_app_server_notification(method, params) else {
            continue;
        };
        if events.is_empty() && thread_id.is_none() {
            continue;
        }

        let notify = CodexAppServerEvent {
            pane_id: context.pane_id,
            events,
            thread_id,
        };
        let window = window.clone();
        window.notify(TermWindowNotif::Apply(Box::new(move |tw| {
            tw.handle_codex_app_server_event(notify);
        })));
    }

    Ok(())
}

fn next_request_id(id: &mut u64) -> u64 {
    let current = *id;
    *id = id.saturating_add(1);
    current
}

fn handle_server_request(method: &str, _params: Option<&Value>) -> Option<Value> {
    match method {
        "item/commandExecution/requestApproval" => Some(json!({"decision":"cancel"})),
        "item/fileChange/requestApproval" => Some(json!({"decision":"cancel"})),
        "item/permissions/requestApproval" => Some(json!({"decision":"cancel"})),
        "item/tool/requestUserInput" => Some(json!({"answers": []})),
        "mcpServer/elicitation/request" => Some(json!({"response": null})),
        "item/tool/call" => Some(json!({"error":"kaku-gui does not proxy dynamic tool calls"})),
        "applyPatchApproval" => Some(json!({"decision":"cancel"})),
        "execCommandApproval" => Some(json!({"decision":"cancel"})),
        _ => Some(json!({})),
    }
}

fn extract_thread_id(params: Option<&Value>) -> Option<String> {
    let params = params?;
    params
        .get("threadId")
        .and_then(Value::as_str)
        .or_else(|| {
            params
                .get("thread")
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            params
                .get("status")
                .and_then(|status| status.get("threadId"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn map_app_server_notification(method: &str, params: Option<&Value>) -> Option<Vec<AgentEvent>> {
    let params = params.unwrap_or(&Value::Null);
    match method {
        "turn/started" => Some(vec![AgentEvent::TaskStarted {
            provider: CODEX_PROVIDER.to_string(),
        }]),
        "item/started" | "item/completed" => Some(vec![AgentEvent::TaskOutput {
            provider: CODEX_PROVIDER.to_string(),
        }]),
        "thread/status/changed" => map_thread_status_changed(params),
        "item/autoApprovalReview/started" => {
            let detail = params
                .get("review")
                .and_then(|value| value.get("rationale"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some("autoApprovalReview".to_string()));
            Some(vec![AgentEvent::ApprovalRequired {
                provider: CODEX_PROVIDER.to_string(),
                detail,
            }])
        }
        "item/autoApprovalReview/completed" => {
            let approved = params
                .get("review")
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                .map(|status| matches!(status, "approved" | "inProgress"))
                .unwrap_or(true);
            Some(vec![AgentEvent::ApprovalResolved {
                provider: CODEX_PROVIDER.to_string(),
                approved,
            }])
        }
        "turn/completed" => Some(map_turn_completed(params)),
        "error" => {
            let will_retry = params
                .get("willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if will_retry {
                return Some(Vec::new());
            }
            let reason = params
                .get("error")
                .and_then(|value| value.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some("codex app-server error".to_string()));
            Some(vec![AgentEvent::TaskFailed {
                provider: CODEX_PROVIDER.to_string(),
                reason,
            }])
        }
        "serverRequest/resolved" => Some(vec![AgentEvent::ApprovalResolved {
            provider: CODEX_PROVIDER.to_string(),
            approved: true,
        }]),
        _ => None,
    }
}

fn map_thread_status_changed(params: &Value) -> Option<Vec<AgentEvent>> {
    let status = params.get("status");
    let status_type = status
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    match status_type {
        "active" => {
            if is_waiting_on_approval_status(status) {
                let detail = status
                    .and_then(|value| value.get("activeFlags"))
                    .and_then(Value::as_array)
                    .and_then(|flags| {
                        flags
                            .iter()
                            .filter_map(Value::as_str)
                            .find(|flag| flag.eq_ignore_ascii_case("waitingOnApproval"))
                            .map(str::to_string)
                    })
                    .or_else(|| Some("waitingOnApproval".to_string()));
                return Some(vec![AgentEvent::ApprovalRequired {
                    provider: CODEX_PROVIDER.to_string(),
                    detail,
                }]);
            }
            if is_waiting_on_user_input_status(status) {
                return Some(vec![AgentEvent::TaskOutput {
                    provider: CODEX_PROVIDER.to_string(),
                }]);
            }
            Some(vec![AgentEvent::TaskOutput {
                provider: CODEX_PROVIDER.to_string(),
            }])
        }
        "idle" => Some(vec![AgentEvent::TaskCompleted {
            provider: CODEX_PROVIDER.to_string(),
        }]),
        "systemError" => Some(vec![AgentEvent::TaskFailed {
            provider: CODEX_PROVIDER.to_string(),
            reason: Some("codex app-server reported systemError".to_string()),
        }]),
        _ => None,
    }
}

fn map_turn_completed(params: &Value) -> Vec<AgentEvent> {
    let turn = params.get("turn");
    let turn_status = turn
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    match turn_status {
        "completed" => vec![AgentEvent::TaskCompleted {
            provider: CODEX_PROVIDER.to_string(),
        }],
        "failed" => {
            let reason = turn
                .and_then(|value| value.get("error"))
                .and_then(|value| value.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some("codex turn failed".to_string()));
            vec![AgentEvent::TaskFailed {
                provider: CODEX_PROVIDER.to_string(),
                reason,
            }]
        }
        "interrupted" => vec![AgentEvent::TaskFailed {
            provider: CODEX_PROVIDER.to_string(),
            reason: Some("codex turn interrupted".to_string()),
        }],
        _ => Vec::new(),
    }
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

#[cfg(test)]
mod tests {
    use super::{extract_thread_id, map_app_server_notification, map_turn_completed};
    use crate::agent_status::events::AgentEvent;
    use serde_json::json;

    #[test]
    fn maps_turn_started_to_loading_event() {
        let events = map_app_server_notification("turn/started", Some(&json!({"threadId":"t1"})))
            .expect("mapped");
        assert_eq!(
            events,
            vec![AgentEvent::TaskStarted {
                provider: "codex".to_string(),
            }]
        );
    }

    #[test]
    fn maps_waiting_on_approval_to_need_approve() {
        let events = map_app_server_notification(
            "thread/status/changed",
            Some(&json!({"status":{"type":"active","activeFlags":["waitingOnApproval"]}})),
        )
        .expect("mapped");
        assert_eq!(
            events,
            vec![AgentEvent::ApprovalRequired {
                provider: "codex".to_string(),
                detail: Some("waitingOnApproval".to_string()),
            }]
        );
    }

    #[test]
    fn maps_turn_completed_failed_to_error() {
        let events = map_turn_completed(&json!({
            "turn": {"status":"failed","error":{"message":"boom"}}
        }));
        assert_eq!(
            events,
            vec![AgentEvent::TaskFailed {
                provider: "codex".to_string(),
                reason: Some("boom".to_string()),
            }]
        );
    }

    #[test]
    fn extracts_thread_id_from_standard_params_shape() {
        let thread_id = extract_thread_id(Some(&json!({
            "threadId": "thread_123"
        })));
        assert_eq!(thread_id.as_deref(), Some("thread_123"));
    }

    #[test]
    fn extracts_thread_id_from_nested_thread_shape() {
        let thread_id = extract_thread_id(Some(&json!({
            "thread": {"id": "thread_456"}
        })));
        assert_eq!(thread_id.as_deref(), Some("thread_456"));
    }
}
