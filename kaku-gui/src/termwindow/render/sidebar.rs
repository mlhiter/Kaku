use crate::agent_status::codex_context::{
    is_codex_like_command as shared_is_codex_like_command,
    is_codex_process_name as shared_is_codex_process_name,
};
use crate::agent_status::events::{SessionStatus, SessionStatusConfidence, SessionStatusSource};
use crate::agent_status::manager::SessionStatusSnapshot;
use crate::quad::TripleLayerQuadAllocator;
use crate::spawn::SpawnWhere;
use crate::termwindow::sidebar_context_menu::{SidebarContextMenuItem, SidebarContextMenuModal};
use crate::termwindow::{
    SidebarAction, TermWindowNotif, UIItem, UIItemType, WorkspaceSidebarActionHit,
    WorkspaceSidebarPendingOpen, WorkspaceSidebarProject, WorkspaceSidebarSession,
};
use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Utc};
use config::keyassignment::{SpawnCommand, SpawnTabDomain};
use mux::pane::{CachePolicy, PaneId};
use mux::renderable::{RenderableDimensions, StableCursorPosition};
use mux::tab::TabId;
use mux::Mux;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use termwiz::cell::{CellAttributes, Intensity};
use termwiz::surface::Line;
use wezterm_term::color::ColorAttribute;
use window::{color::LinearRgba, WindowOps};

const SIDEBAR_DEFAULT_WIDTH_PX: usize = 320;
const SIDEBAR_MIN_WIDTH_PX: usize = 240;
const SIDEBAR_MAX_WIDTH_PX: usize = 620;
const SIDEBAR_DEFAULT_VISIBLE: bool = true;
const SIDEBAR_RESIZE_HANDLE_WIDTH_PX: usize = 8;
const SIDEBAR_REFRESH_INTERVAL: Duration = Duration::from_millis(1200);
const SIDEBAR_TRANSIENT_STATUS_STALE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);
const SIDEBAR_STATUS_SPINNER_INTERVAL: Duration = Duration::from_millis(120);
const SIDEBAR_TEXT_LEFT_PADDING: f32 = 12.0;
const SIDEBAR_TEXT_TOP_PADDING: f32 = 12.0;
const SIDEBAR_UI_STATE_RELATIVE_PATH: &str = "gui/sidebar.json";
const SIDEBAR_MORE_BUTTON_WIDTH_PX: usize = 28;
const SIDEBAR_MORE_BUTTON_RIGHT_PADDING_PX: usize = 8;

static SIDEBAR_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectsFile {
    #[serde(default = "schema_version")]
    version: u8,
    #[serde(default)]
    projects: Vec<ProjectEntry>,
}

impl Default for ProjectsFile {
    fn default() -> Self {
        Self {
            version: schema_version(),
            projects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectEntry {
    id: String,
    name: String,
    root_path: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    last_active_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionsFile {
    #[serde(default = "schema_version")]
    version: u8,
    #[serde(default)]
    sessions: Vec<SessionEntry>,
}

impl Default for SessionsFile {
    fn default() -> Self {
        Self {
            version: schema_version(),
            sessions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnippetsFile {
    #[serde(default = "schema_version")]
    version: u8,
    #[serde(default)]
    snippets: Vec<SnippetEntry>,
}

impl Default for SnippetsFile {
    fn default() -> Self {
        Self {
            version: schema_version(),
            snippets: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SnippetEntry {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    updated_at: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SessionEntry {
    id: String,
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    codex_thread_id: String,
    #[serde(default)]
    status_source: String,
    #[serde(default)]
    status_confidence: String,
    #[serde(default)]
    status_reason: String,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SidebarUiStateFile {
    #[serde(default = "schema_version")]
    version: u8,
    #[serde(default)]
    width_px: usize,
    #[serde(default = "sidebar_default_visible")]
    visible: bool,
    #[serde(default)]
    updated_at: String,
}

impl Default for SidebarUiStateFile {
    fn default() -> Self {
        Self {
            version: schema_version(),
            width_px: SIDEBAR_DEFAULT_WIDTH_PX,
            visible: SIDEBAR_DEFAULT_VISIBLE,
            updated_at: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PersistedSidebarUiState {
    pub width_px: usize,
    pub visible: bool,
}

#[derive(Debug, Clone)]
struct SidebarLine {
    text: String,
    is_header: bool,
    tone: SidebarLineTone,
    background: Option<SidebarLineBackground>,
    action: Option<SidebarAction>,
    trailing_buttons: Vec<SidebarTrailingButton>,
}

#[derive(Debug, Clone)]
struct SidebarTrailingButton {
    label: &'static str,
    action: SidebarAction,
}

impl SidebarLine {
    fn new(text: impl Into<String>, is_header: bool) -> Self {
        Self {
            text: text.into(),
            is_header,
            tone: SidebarLineTone::Default,
            background: None,
            action: None,
            trailing_buttons: Vec::new(),
        }
    }

    fn action(text: impl Into<String>, action: SidebarAction) -> Self {
        Self {
            text: text.into(),
            is_header: false,
            tone: SidebarLineTone::Default,
            background: None,
            action: Some(action),
            trailing_buttons: Vec::new(),
        }
    }

    fn with_tone(mut self, tone: SidebarLineTone) -> Self {
        self.tone = tone;
        self
    }

    fn with_background(mut self, background: SidebarLineBackground) -> Self {
        self.background = Some(background);
        self
    }

    fn with_trailing_button(mut self, label: &'static str, action: SidebarAction) -> Self {
        self.trailing_buttons
            .push(SidebarTrailingButton { label, action });
        self
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SidebarLineTone {
    Default,
    Muted,
    Info,
    Warning,
    Success,
    Danger,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SidebarLineBackground {
    CurrentSession,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SidebarSessionStatus {
    Idle,
    Loading,
    NeedApprove,
    Running,
    Done,
    Error,
    Unknown,
}

impl SidebarSessionStatus {
    fn parse(value: &str) -> Self {
        match SessionStatus::parse_storage(value) {
            Some(SessionStatus::Idle) => Self::Idle,
            Some(SessionStatus::Loading) => Self::Loading,
            Some(SessionStatus::NeedApprove) => Self::NeedApprove,
            Some(SessionStatus::Running) => Self::Running,
            Some(SessionStatus::Done) => Self::Done,
            Some(SessionStatus::Error) => Self::Error,
            None => Self::Unknown,
        }
    }

    fn badge(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Loading => "PREP",
            Self::NeedApprove => "APPROVAL",
            Self::Running => "RUNNING",
            Self::Done => "DONE",
            Self::Error => "ERROR",
            Self::Unknown => "UNKNOWN",
        }
    }

    fn tone(self) -> SidebarLineTone {
        match self {
            Self::Idle => SidebarLineTone::Muted,
            Self::Loading | Self::Running => SidebarLineTone::Info,
            Self::NeedApprove => SidebarLineTone::Warning,
            Self::Done => SidebarLineTone::Success,
            Self::Error => SidebarLineTone::Danger,
            Self::Unknown => SidebarLineTone::Muted,
        }
    }
}

fn parse_session_status_source(value: &str) -> SessionStatusSource {
    SessionStatusSource::parse_storage(value).unwrap_or(SessionStatusSource::Structured)
}

fn parse_session_status_confidence(value: &str) -> SessionStatusConfidence {
    SessionStatusConfidence::parse_storage(value).unwrap_or(SessionStatusConfidence::Low)
}

fn sidebar_status_signal_tag(
    source: SessionStatusSource,
    confidence: SessionStatusConfidence,
) -> &'static str {
    match (source, confidence) {
        (SessionStatusSource::Structured, SessionStatusConfidence::High) => "S+",
        (SessionStatusSource::Structured, SessionStatusConfidence::Low) => "S?",
        (SessionStatusSource::Heuristic, SessionStatusConfidence::High) => "H+",
        (SessionStatusSource::Heuristic, SessionStatusConfidence::Low) => "H?",
    }
}

fn sidebar_status_source_label(source: SessionStatusSource) -> &'static str {
    match source {
        SessionStatusSource::Structured => "structured",
        SessionStatusSource::Heuristic => "heuristic",
    }
}

fn sidebar_status_is_animating(status: SidebarSessionStatus) -> bool {
    matches!(status, SidebarSessionStatus::Loading)
}

fn sidebar_status_spinner_frame(elapsed: Duration) -> char {
    const FRAMES: [char; 4] = ['-', '\\', '|', '/'];
    let interval_ms = SIDEBAR_STATUS_SPINNER_INTERVAL.as_millis().max(1);
    let frame_idx = ((elapsed.as_millis() / interval_ms) % FRAMES.len() as u128) as usize;
    FRAMES[frame_idx]
}

#[derive(Debug, Clone)]
struct SidebarSessionMeta {
    title: String,
    root_path: String,
}

#[derive(Debug, Clone)]
enum SidebarCloseAction {
    CloseOthers {
        project_id: String,
        session_id: String,
    },
    CloseAll {
        project_id: String,
    },
    DeleteSession {
        project_id: String,
        session_id: String,
    },
    DeleteProject {
        project_id: String,
    },
}

pub(crate) fn load_persisted_sidebar_ui_state() -> PersistedSidebarUiState {
    let path = sidebar_ui_state_path();
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return PersistedSidebarUiState {
                width_px: SIDEBAR_DEFAULT_WIDTH_PX,
                visible: SIDEBAR_DEFAULT_VISIBLE,
            };
        }
        Err(err) => {
            log::warn!(
                "workspace sidebar: failed to read sidebar ui state {}: {}",
                path.display(),
                err
            );
            return PersistedSidebarUiState {
                width_px: SIDEBAR_DEFAULT_WIDTH_PX,
                visible: SIDEBAR_DEFAULT_VISIBLE,
            };
        }
    };

    match serde_json::from_slice::<SidebarUiStateFile>(&data) {
        Ok(state) => PersistedSidebarUiState {
            width_px: state
                .width_px
                .clamp(SIDEBAR_MIN_WIDTH_PX, SIDEBAR_MAX_WIDTH_PX),
            visible: state.visible,
        },
        Err(err) => {
            log::warn!(
                "workspace sidebar: failed to parse sidebar ui state {}: {}",
                path.display(),
                err
            );
            PersistedSidebarUiState {
                width_px: SIDEBAR_DEFAULT_WIDTH_PX,
                visible: SIDEBAR_DEFAULT_VISIBLE,
            }
        }
    }
}

impl crate::TermWindow {
    fn sidebar_stored_width_px(&self) -> usize {
        let width = if self.workspace_sidebar_width_px == 0 {
            SIDEBAR_DEFAULT_WIDTH_PX
        } else {
            self.workspace_sidebar_width_px
        };
        width.clamp(SIDEBAR_MIN_WIDTH_PX, SIDEBAR_MAX_WIDTH_PX)
    }

    pub(crate) fn sidebar_reserved_width_px(&self) -> usize {
        if !self.workspace_sidebar_visible {
            return 0;
        }
        self.sidebar_stored_width_px()
    }

    pub(crate) fn sidebar_resize_handle_width_px(&self) -> usize {
        SIDEBAR_RESIZE_HANDLE_WIDTH_PX
    }

    pub(crate) fn sidebar_set_width_px(&mut self, width: usize) -> bool {
        let clamped = width.clamp(SIDEBAR_MIN_WIDTH_PX, SIDEBAR_MAX_WIDTH_PX);
        if clamped == self.workspace_sidebar_width_px {
            return false;
        }
        self.workspace_sidebar_width_px = clamped;
        true
    }

    fn sidebar_persist_ui_state(&mut self, failure_message: &str) {
        let path = sidebar_ui_state_path();
        let state = SidebarUiStateFile {
            version: schema_version(),
            width_px: self.sidebar_stored_width_px(),
            visible: self.workspace_sidebar_visible,
            updated_at: Utc::now().to_rfc3339(),
        };

        if let Err(err) = write_json_file_atomic(&path, &state) {
            log::warn!(
                "workspace sidebar: failed to persist sidebar state {}: {:#}",
                path.display(),
                err
            );
            self.show_toast(failure_message.to_string());
        }
    }

    pub(crate) fn sidebar_set_visible(&mut self, visible: bool) -> bool {
        if self.workspace_sidebar_visible == visible {
            return false;
        }

        self.workspace_sidebar_visible = visible;
        self.sidebar_persist_ui_state("Failed to save sidebar state");
        self.sync_tab_bar_visibility_for_window_state("workspace_sidebar_visibility_changed");

        if let Some(window) = self.window.clone() {
            let dimensions = self.dimensions;
            self.apply_dimensions(&dimensions, None, &window, true);
            window.invalidate();
        }

        true
    }

    pub(crate) fn sidebar_toggle_visible(&mut self) -> bool {
        self.sidebar_set_visible(!self.workspace_sidebar_visible);
        self.workspace_sidebar_visible
    }

    pub(crate) fn sidebar_resize_from_drag(&mut self, start_x: isize, current_x: isize) -> bool {
        let delta = current_x.saturating_sub(start_x);
        let start_width = self.sidebar_stored_width_px() as isize;
        let next_width = (start_width + delta)
            .clamp(SIDEBAR_MIN_WIDTH_PX as isize, SIDEBAR_MAX_WIDTH_PX as isize)
            as usize;
        self.sidebar_set_width_px(next_width)
    }

    pub(crate) fn sidebar_persist_width_px(&mut self) {
        self.sidebar_persist_ui_state("Failed to save sidebar width");
    }

    pub(crate) fn workspace_sidebar_action_at(&self, x: isize, y: isize) -> Option<SidebarAction> {
        self.workspace_sidebar_action_hits
            .iter()
            .find(|hit| {
                x >= hit.x as isize
                    && x <= (hit.x + hit.width) as isize
                    && y >= hit.y as isize
                    && y <= (hit.y + hit.height) as isize
            })
            .map(|hit| hit.action.clone())
    }

    pub(crate) fn perform_workspace_sidebar_action(
        &mut self,
        action: SidebarAction,
    ) -> anyhow::Result<()> {
        self.force_workspace_sidebar_refresh();

        match action {
            SidebarAction::CreateProject => self.sidebar_request_create_project(),
            SidebarAction::CreateSessionInProject { project_id } => {
                self.sidebar_create_session_in_project(project_id.as_str())
            }
            SidebarAction::ProjectRow { .. } => Ok(()),
            SidebarAction::OpenProjectContextMenu { .. } => Ok(()),
            SidebarAction::RenameProject { project_id } => {
                self.sidebar_request_rename_project(project_id.as_str())
            }
            SidebarAction::ActivateSession {
                project_id,
                session_id,
            } => self.sidebar_activate_session(project_id.as_str(), session_id.as_str()),
            SidebarAction::OpenSessionContextMenu { .. } => Ok(()),
            SidebarAction::TogglePin {
                project_id,
                session_id,
                pinned,
            } => self.sidebar_set_session_pin(project_id.as_str(), session_id.as_str(), pinned),
            SidebarAction::CloseOthers {
                project_id,
                session_id,
            } => {
                self.sidebar_request_close_other_sessions(project_id.as_str(), session_id.as_str())
            }
            SidebarAction::CloseAll { project_id } => {
                self.sidebar_request_close_all_unpinned(project_id.as_str())
            }
            SidebarAction::RenameSession {
                project_id,
                session_id,
            } => self.sidebar_request_rename_session(project_id.as_str(), session_id.as_str()),
            SidebarAction::DeleteSession {
                project_id,
                session_id,
            } => self.sidebar_request_delete_session(project_id.as_str(), session_id.as_str()),
            SidebarAction::DeleteProject { project_id } => {
                self.sidebar_request_delete_project(project_id.as_str())
            }
            SidebarAction::CreateSnippet { project_id } => {
                self.sidebar_request_create_snippet(project_id.as_str())
            }
            SidebarAction::InsertSnippet {
                project_id,
                snippet_id,
            } => self.sidebar_insert_snippet(project_id.as_str(), snippet_id.as_str()),
            SidebarAction::OpenSnippetContextMenu { .. } => Ok(()),
            SidebarAction::EditSnippet {
                project_id,
                snippet_id,
            } => self.sidebar_request_edit_snippet(project_id.as_str(), snippet_id.as_str()),
            SidebarAction::DeleteSnippet {
                project_id,
                snippet_id,
            } => self.sidebar_delete_snippet(project_id.as_str(), snippet_id.as_str()),
        }
    }

    pub(crate) fn sidebar_bind_pending_open_to_tab(&mut self, tab_id: TabId) {
        let Some(pending) = self.workspace_sidebar_pending_opens.pop_front() else {
            return;
        };

        self.sidebar_bind_session_to_tab(
            pending.project_id.as_str(),
            pending.session_id.as_str(),
            tab_id,
        );

        let mux = Mux::get();
        if let Some(tab) = mux.get_tab(tab_id) {
            tab.set_title(pending.session_title.as_str());
        }
    }

    pub(crate) fn sidebar_unbind_tab(&mut self, tab_id: TabId) {
        if let Some((project_id, session_id)) =
            self.workspace_sidebar_tab_to_session.remove(&tab_id)
        {
            self.workspace_sidebar_session_to_tab
                .remove(&sidebar_session_key(
                    project_id.as_str(),
                    session_id.as_str(),
                ));
        }
    }

    pub(crate) fn sidebar_is_tab_pinned(&mut self, tab_id: TabId) -> bool {
        self.prune_workspace_sidebar_tab_bindings();
        let Some((project_id, session_id)) =
            self.workspace_sidebar_tab_to_session.get(&tab_id).cloned()
        else {
            return false;
        };

        if let Some(pinned) = self.sidebar_lookup_session_pinned(&project_id, &session_id) {
            return pinned;
        }

        self.force_workspace_sidebar_refresh();
        self.sidebar_lookup_session_pinned(&project_id, &session_id)
            .unwrap_or(false)
    }

    fn sidebar_active_project_and_session(&mut self) -> Option<(String, String)> {
        self.prune_workspace_sidebar_tab_bindings();
        let mux = Mux::get();
        let tab_id = mux.get_active_tab_for_window(self.mux_window_id)?.tab_id();
        self.workspace_sidebar_tab_to_session.get(&tab_id).cloned()
    }

    pub(crate) fn sidebar_session_binding_for_pane(
        &mut self,
        pane_id: PaneId,
    ) -> Option<(String, String)> {
        self.prune_workspace_sidebar_tab_bindings();
        let mux = Mux::get();
        let (_, window_id, tab_id) = mux.resolve_pane_id(pane_id)?;
        if window_id != self.mux_window_id {
            return None;
        }
        if let Some(binding) = self.workspace_sidebar_tab_to_session.get(&tab_id).cloned() {
            return Some(binding);
        }

        self.sidebar_infer_session_binding_for_tab(pane_id, tab_id)
    }

    fn sidebar_infer_session_binding_for_tab(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
    ) -> Option<(String, String)> {
        self.refresh_workspace_sidebar_if_needed();
        let mux = Mux::get();
        let pane = mux.get_pane(pane_id)?;
        let cwd = pane
            .get_current_working_dir(CachePolicy::FetchImmediate)
            .or_else(|| pane.get_current_working_dir(CachePolicy::AllowStale))
            .and_then(|url| url.to_file_path().ok())
            .or_else(|| std::env::current_dir().ok())
            .or_else(|| Some(config::HOME_DIR.clone()))?;
        let mut project_id = pick_project_for_cwd(&self.workspace_sidebar.projects, cwd.as_path())
            .map(|project| project.id.clone());
        if project_id.is_none() {
            match ensure_sidebar_project_for_cwd(cwd.as_path()) {
                Ok(created_project_id) => {
                    log::info!(
                        "workspace sidebar: auto-registered project for pane={} cwd={} project={}",
                        pane_id,
                        cwd.display(),
                        created_project_id
                    );
                    project_id = Some(created_project_id);
                    self.force_workspace_sidebar_refresh();
                }
                Err(err) => {
                    log::warn!(
                        "workspace sidebar: failed to auto-register project for pane={} cwd={} err={:#}",
                        pane_id,
                        cwd.display(),
                        err
                    );
                }
            }
        }
        let project_id = project_id?;

        let tab_title = mux
            .get_tab(tab_id)
            .map(|tab| tab.get_title())
            .unwrap_or_default();
        let bound_session_keys: HashSet<String> = self
            .workspace_sidebar_session_to_tab
            .keys()
            .cloned()
            .collect();
        let mut session_id = self
            .workspace_sidebar
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .and_then(|project| {
                pick_session_for_inferred_binding(project, &bound_session_keys, tab_title.as_str())
            })
            .map(|session| session.id.clone());
        if session_id.is_none() {
            match ensure_sidebar_project_has_session(project_id.as_str()) {
                Ok(ensured_session_id) => {
                    log::info!(
                        "workspace sidebar: auto-created session for pane={} tab={} project={} session={}",
                        pane_id,
                        tab_id,
                        project_id,
                        ensured_session_id
                    );
                    session_id = Some(ensured_session_id);
                    self.force_workspace_sidebar_refresh();
                }
                Err(err) => {
                    log::warn!(
                        "workspace sidebar: failed to ensure session for pane={} tab={} project={} err={:#}",
                        pane_id,
                        tab_id,
                        project_id,
                        err
                    );
                }
            }
        }
        let session_id = session_id?;
        self.sidebar_bind_session_to_tab(project_id.as_str(), session_id.as_str(), tab_id);
        self.sidebar_reset_transient_status_for_non_codex_context(
            project_id.as_str(),
            session_id.as_str(),
            &pane,
        );
        log::info!(
            "workspace sidebar: inferred tab binding pane={} tab={} cwd={} -> {}/{}",
            pane_id,
            tab_id,
            cwd.display(),
            project_id,
            session_id
        );
        Some((project_id, session_id))
    }

    fn sidebar_reset_transient_status_for_non_codex_context(
        &mut self,
        project_id: &str,
        session_id: &str,
        pane: &std::sync::Arc<dyn mux::pane::Pane>,
    ) {
        let user_vars = pane.copy_user_vars();
        let live_command = user_vars
            .get("WEZTERM_PROG")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        let foreground_process_name = pane.get_foreground_process_name(CachePolicy::FetchImmediate);
        let is_codex_context = live_command.is_some_and(is_codex_command)
            || foreground_process_name
                .as_deref()
                .is_some_and(is_codex_process_name);
        if is_codex_context {
            return;
        }

        let Some(snapshot) = self.sidebar_session_status_snapshot(project_id, session_id) else {
            return;
        };
        let is_transient = matches!(
            snapshot.status,
            SessionStatus::Loading | SessionStatus::NeedApprove | SessionStatus::Running
        );
        if !(is_transient
            && snapshot.source == SessionStatusSource::Heuristic
            && snapshot.confidence == SessionStatusConfidence::Low)
        {
            return;
        }

        if let Err(err) = self.sidebar_set_session_status_snapshot(
            project_id,
            session_id,
            &SessionStatusSnapshot::new(
                SessionStatus::Idle,
                SessionStatusSource::Heuristic,
                SessionStatusConfidence::Low,
                None,
            ),
        ) {
            log::warn!(
                "workspace sidebar: failed to reset transient non-codex status {}/{}: {:#}",
                project_id,
                session_id,
                err
            );
        }
    }

    pub(crate) fn sidebar_session_status_snapshot(
        &mut self,
        project_id: &str,
        session_id: &str,
    ) -> Option<SessionStatusSnapshot> {
        self.refresh_workspace_sidebar_if_needed();
        let project = self
            .workspace_sidebar
            .projects
            .iter()
            .find(|project| project.id == project_id)?;
        let session = project
            .sessions
            .iter()
            .find(|session| session.id == session_id)?;

        let status =
            SessionStatus::parse_storage(session.status.as_str()).unwrap_or(SessionStatus::Idle);
        let source = parse_session_status_source(session.status_source.as_str());
        let confidence = parse_session_status_confidence(session.status_confidence.as_str());
        let reason = if session.status_reason.trim().is_empty() {
            None
        } else {
            Some(session.status_reason.clone())
        };

        Some(SessionStatusSnapshot::new(
            status, source, confidence, reason,
        ))
    }

    pub(crate) fn sidebar_set_session_status_snapshot(
        &mut self,
        project_id: &str,
        session_id: &str,
        snapshot: &SessionStatusSnapshot,
    ) -> anyhow::Result<()> {
        let sessions_path = sidebar_state_root()
            .join("projects")
            .join(project_id)
            .join("sessions.json");
        if !sessions_path.exists() {
            bail!("sessions file not found: {}", sessions_path.display());
        }

        let mut sessions_file: SessionsFile = read_json_file(&sessions_path)?;
        let session = sessions_file
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("session not found in storage: {session_id}"))?;

        let updated_at = Utc::now().to_rfc3339();
        session.status = snapshot.status.as_storage_str().to_string();
        session.status_source = snapshot.source.as_storage_str().to_string();
        session.status_confidence = snapshot.confidence.as_storage_str().to_string();
        session.status_reason = snapshot.reason.clone().unwrap_or_default();
        session.updated_at = updated_at;
        write_json_file_atomic(&sessions_path, &sessions_file)?;

        self.force_workspace_sidebar_refresh();
        Ok(())
    }

    fn sidebar_active_project_for_shortcuts(&mut self) -> Option<String> {
        if let Some((project_id, _)) = self.sidebar_active_project_and_session() {
            return Some(project_id);
        }

        self.refresh_workspace_sidebar_if_needed();
        if self.workspace_sidebar.projects.len() == 1 {
            return self
                .workspace_sidebar
                .projects
                .first()
                .map(|project| project.id.clone());
        }

        None
    }

    pub(crate) fn sidebar_shortcut_create_project(&mut self) -> anyhow::Result<()> {
        self.sidebar_request_create_project()
    }

    pub(crate) fn sidebar_shortcut_create_session(&mut self) -> anyhow::Result<()> {
        let Some(project_id) = self.sidebar_active_project_for_shortcuts() else {
            self.show_toast("Select a project before creating a session".to_string());
            return Ok(());
        };

        self.sidebar_create_session_in_project(project_id.as_str())
    }

    pub(crate) fn sidebar_shortcut_toggle_pin_current_session(&mut self) -> anyhow::Result<()> {
        let Some((project_id, session_id)) = self.sidebar_active_project_and_session() else {
            self.show_toast("No active session to pin".to_string());
            return Ok(());
        };

        let pinned = self
            .sidebar_lookup_session_pinned(project_id.as_str(), session_id.as_str())
            .unwrap_or(false);
        self.sidebar_set_session_pin(project_id.as_str(), session_id.as_str(), !pinned)
    }

    pub(crate) fn sidebar_shortcut_rename_current_session(&mut self) -> anyhow::Result<()> {
        let Some((project_id, session_id)) = self.sidebar_active_project_and_session() else {
            self.show_toast("No active session to rename".to_string());
            return Ok(());
        };

        self.sidebar_request_rename_session(project_id.as_str(), session_id.as_str())
    }

    pub(crate) fn sidebar_shortcut_delete_current_session(&mut self) -> anyhow::Result<()> {
        let Some((project_id, session_id)) = self.sidebar_active_project_and_session() else {
            self.show_toast("No active session to delete".to_string());
            return Ok(());
        };

        self.sidebar_request_delete_session(project_id.as_str(), session_id.as_str())
    }

    pub(crate) fn prune_workspace_sidebar_tab_bindings(&mut self) {
        let mux = Mux::get();
        let Some(window) = mux.get_window(self.mux_window_id) else {
            self.workspace_sidebar_pending_opens.clear();
            self.workspace_sidebar_tab_to_session.clear();
            self.workspace_sidebar_session_to_tab.clear();
            return;
        };

        let live_tab_ids: HashSet<TabId> = window.iter().map(|tab| tab.tab_id()).collect();
        self.workspace_sidebar_tab_to_session
            .retain(|tab_id, _| live_tab_ids.contains(tab_id));
        self.workspace_sidebar_session_to_tab
            .retain(|_, tab_id| live_tab_ids.contains(tab_id));
    }

    pub fn paint_workspace_sidebar(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some((x, y, width, height)) = self.sidebar_bounds() else {
            self.workspace_sidebar_action_hits.clear();
            return Ok(());
        };

        self.refresh_workspace_sidebar_if_needed();

        let palette = self.palette().clone();
        let panel_bg = sidebar_background_color(
            palette
                .resolve_bg(ColorAttribute::Default)
                .to_linear()
                .mul_alpha(self.config.window_background_opacity),
        );
        let panel_border = sidebar_border_color(palette.foreground.to_linear());
        let text_color = palette.foreground.to_linear();

        self.filled_rectangle(layers, 0, euclid::rect(x, y, width, height), panel_bg)
            .context("paint sidebar background")?;
        self.filled_rectangle(
            layers,
            1,
            euclid::rect((x + width - 1.0).max(0.0), y, 1.0, height),
            panel_border,
        )
        .context("paint sidebar border")?;

        self.ui_items.push(UIItem {
            x: x as usize,
            y: y as usize,
            width: width as usize,
            height: height as usize,
            item_type: UIItemType::SidebarPanel,
        });
        let handle_width = self.sidebar_resize_handle_width_px();
        self.ui_items.push(UIItem {
            x: x.max(0.0) as usize + width.max(0.0) as usize - handle_width,
            y: y as usize,
            width: handle_width,
            height: height as usize,
            item_type: UIItemType::SidebarResizeHandle,
        });
        self.workspace_sidebar_action_hits.clear();

        let line_height = self.render_metrics.cell_size.height as f32;
        let mut top = y + SIDEBAR_TEXT_TOP_PADDING;
        let max_top = y + height - line_height;
        let left = x + SIDEBAR_TEXT_LEFT_PADDING;
        let text_width = (width - SIDEBAR_TEXT_LEFT_PADDING * 2.0).max(8.0);
        let lines = self.build_workspace_sidebar_lines();

        for entry in lines {
            if top > max_top {
                break;
            }
            let row_y = top.max(0.0);
            let row_height = line_height.max(1.0).ceil();

            if let Some(background) = entry.background {
                self.filled_rectangle(
                    layers,
                    1,
                    euclid::rect(x + 6.0, row_y, (width - 12.0).max(0.0), row_height),
                    sidebar_row_background_color(panel_bg, text_color, background),
                )
                .context("paint sidebar row background")?;
            }

            if entry.is_header && !entry.text.trim().is_empty() {
                self.filled_rectangle(
                    layers,
                    1,
                    euclid::rect(x + 4.0, row_y, (width - 8.0).max(0.0), row_height),
                    sidebar_header_background_color(panel_bg, text_color),
                )
                .context("paint sidebar header background")?;
            }

            let mut attrs = CellAttributes::default();
            if entry.is_header {
                attrs.set_intensity(Intensity::Bold);
            }
            let line = Line::from_text(&entry.text, &attrs, 0, None);
            let line_color = sidebar_line_color(text_color, entry.tone);

            self.paint_workspace_sidebar_line(
                layers, line, left, top, text_width, line_color, panel_bg,
            )
            .context("paint sidebar line")?;

            if let Some(action) = entry.action {
                let y = row_y as usize;
                let height = row_height as usize;
                self.ui_items.push(UIItem {
                    x: x as usize,
                    y,
                    width: width as usize,
                    height,
                    item_type: UIItemType::SidebarAction(action.clone()),
                });
                if !sidebar_action_needs_pointer_position(&action) {
                    self.workspace_sidebar_action_hits
                        .push(WorkspaceSidebarActionHit {
                            x: x as usize,
                            y,
                            width: width as usize,
                            height,
                            action,
                        });
                }
            }

            if !entry.trailing_buttons.is_empty() {
                let is_hovered = self.current_mouse_event.as_ref().is_some_and(|event| {
                    event.coords.x as f32 >= x
                        && event.coords.x as f32 <= x + width
                        && event.coords.y as f32 >= row_y
                        && event.coords.y as f32 <= row_y + row_height
                });
                if is_hovered {
                    let button_width = SIDEBAR_MORE_BUTTON_WIDTH_PX
                        .min((width as usize).saturating_sub(16))
                        .max(22) as f32;
                    let glyph_width = self.render_metrics.cell_size.width as f32;
                    let button_bg = sidebar_more_button_background(panel_bg, text_color);
                    let mut button_right = x + width - SIDEBAR_MORE_BUTTON_RIGHT_PADDING_PX as f32;

                    for button in entry.trailing_buttons.iter().rev() {
                        let button_x = (button_right - button_width).max(left);
                        if button_x >= button_right {
                            break;
                        }

                        self.filled_rectangle(
                            layers,
                            1,
                            euclid::rect(button_x, row_y, button_width, row_height),
                            button_bg,
                        )
                        .context("paint sidebar more button background")?;

                        let mut more_attrs = CellAttributes::default();
                        more_attrs.set_intensity(Intensity::Bold);
                        let more = Line::from_text(button.label, &more_attrs, 0, None);
                        let more_left = button_x + ((button_width - glyph_width).max(0.0) / 2.0);
                        self.paint_workspace_sidebar_line(
                            layers,
                            more,
                            more_left,
                            top,
                            glyph_width.max(8.0),
                            sidebar_more_button_foreground(text_color),
                            button_bg,
                        )
                        .context("paint sidebar more button glyph")?;

                        let button_action = button.action.clone();
                        self.ui_items.push(UIItem {
                            x: button_x.max(0.0) as usize,
                            y: row_y as usize,
                            width: button_width.max(1.0) as usize,
                            height: row_height.max(1.0) as usize,
                            item_type: UIItemType::SidebarAction(button_action.clone()),
                        });
                        if !sidebar_action_needs_pointer_position(&button_action) {
                            self.workspace_sidebar_action_hits
                                .push(WorkspaceSidebarActionHit {
                                    x: button_x.max(0.0) as usize,
                                    y: row_y as usize,
                                    width: button_width.max(1.0) as usize,
                                    height: row_height.max(1.0) as usize,
                                    action: button_action,
                                });
                        }

                        button_right = button_x - 4.0;
                        if button_right <= left {
                            break;
                        }
                    }
                }
            }

            top += line_height;
        }

        Ok(())
    }

    fn paint_workspace_sidebar_line(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        line: Line,
        left: f32,
        top: f32,
        pixel_width: f32,
        foreground: LinearRgba,
        default_bg: LinearRgba,
    ) -> anyhow::Result<()> {
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();
        let cols = (pixel_width / self.render_metrics.cell_size.width as f32).max(1.0) as usize;
        let cursor = StableCursorPosition::default();
        let palette = self.palette().clone();
        let config = self.config.clone();
        let use_pixel_positioning = config.experimental_pixel_positioning;
        let render_metrics = self.render_metrics;
        let dims = RenderableDimensions {
            cols,
            physical_top: 0,
            scrollback_rows: 0,
            scrollback_top: 0,
            viewport_rows: 1,
            dpi: self.terminal_size.dpi,
            pixel_height: self.render_metrics.cell_size.height as usize,
            pixel_width: pixel_width as usize,
            reverse_video: false,
        };

        self.render_screen_line(
            crate::termwindow::render::RenderScreenLineParams {
                top_pixel_y: top,
                left_pixel_x: left,
                pixel_width,
                stable_line_idx: None,
                line: &line,
                selection: 0..0,
                cursor: &cursor,
                palette: &palette,
                dims: &dims,
                config: &config,
                pane: None,
                white_space,
                filled_box,
                cursor_border_color: LinearRgba::default(),
                foreground,
                is_active: true,
                selection_fg: LinearRgba::default(),
                selection_bg: LinearRgba::default(),
                cursor_fg: LinearRgba::default(),
                cursor_bg: LinearRgba::default(),
                cursor_is_default_color: true,
                window_is_transparent: false,
                default_bg,
                font: None,
                style: None,
                use_pixel_positioning,
                render_metrics,
                shape_key: None,
                password_input: false,
            },
            layers,
        )?;

        Ok(())
    }

    fn sidebar_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let width = self.sidebar_reserved_width_px() as f32;
        if width <= 0.0 {
            return None;
        }

        let border = self.get_os_border();
        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.0)
        } else {
            0.0
        };
        let (_, padding_top) = self.padding_left_top();
        let top_bar_height = if self.config.tab_bar_at_bottom {
            0.0
        } else {
            tab_bar_height
        };
        let effective_border_top = if self.show_tab_bar && !self.config.tab_bar_at_bottom {
            0.0
        } else {
            border.top.get() as f32
        };
        let top = (top_bar_height + padding_top + effective_border_top).max(0.0);

        let bottom_tab_height = if self.show_tab_bar && self.config.tab_bar_at_bottom {
            tab_bar_height
        } else {
            0.0
        };
        let effective_border_bottom = if self.show_tab_bar && self.config.tab_bar_at_bottom {
            0.0
        } else {
            border.bottom.get() as f32
        };
        let height = (self.dimensions.pixel_height as f32
            - top
            - bottom_tab_height
            - effective_border_bottom)
            .max(0.0);

        if self.dimensions.pixel_width < (width as usize).saturating_add(64) || height < 24.0 {
            return None;
        }

        Some((0.0, top, width, height))
    }

    fn refresh_workspace_sidebar_if_needed(&mut self) {
        self.prune_workspace_sidebar_tab_bindings();

        let now = Instant::now();
        let should_refresh = self
            .workspace_sidebar
            .last_loaded_at
            .map(|loaded| now.duration_since(loaded) >= SIDEBAR_REFRESH_INTERVAL)
            .unwrap_or(true);

        if !should_refresh {
            return;
        }

        match self.load_workspace_sidebar_projects() {
            Ok(projects) => {
                self.workspace_sidebar.projects = projects;
                self.workspace_sidebar.last_load_error = None;
            }
            Err(err) => {
                self.workspace_sidebar.last_load_error = Some(format!("{:#}", err));
            }
        }
        self.workspace_sidebar.last_loaded_at = Some(now);
    }

    fn force_workspace_sidebar_refresh(&mut self) {
        self.workspace_sidebar.last_loaded_at = None;
        self.refresh_workspace_sidebar_if_needed();
    }

    fn load_workspace_sidebar_projects(&self) -> anyhow::Result<Vec<WorkspaceSidebarProject>> {
        let root = sidebar_state_root();
        let projects_path = root.join("projects.json");
        if !projects_path.exists() {
            return Ok(Vec::new());
        }

        let projects_file: ProjectsFile = read_json_file(&projects_path)?;
        let mut projects: Vec<WorkspaceSidebarProject> = Vec::new();
        let now = Utc::now();

        for project in projects_file.projects {
            let sessions_path = root
                .join("projects")
                .join(project.id.as_str())
                .join("sessions.json");

            let mut sessions = if sessions_path.exists() {
                match read_json_file::<SessionsFile>(&sessions_path) {
                    Ok(mut file) => {
                        let mut repaired = false;
                        for session in &mut file.sessions {
                            if session.status.is_empty() {
                                session.status = SessionStatus::Idle.as_storage_str().to_string();
                                repaired = true;
                            }
                            if session.status_source.is_empty() {
                                session.status_source =
                                    SessionStatusSource::Structured.as_storage_str().to_string();
                                repaired = true;
                            }
                            if session.status_confidence.is_empty() {
                                session.status_confidence =
                                    SessionStatusConfidence::Low.as_storage_str().to_string();
                                repaired = true;
                            }

                            let normalized_status = normalize_transient_session_status(
                                session.status.as_str(),
                                session.updated_at.as_str(),
                                now,
                            );
                            if normalized_status != session.status {
                                session.status = normalized_status;
                                session.status_source =
                                    SessionStatusSource::Structured.as_storage_str().to_string();
                                session.status_confidence =
                                    SessionStatusConfidence::Low.as_storage_str().to_string();
                                session.status_reason.clear();
                                repaired = true;
                            }
                        }

                        if repaired {
                            if let Err(err) = write_json_file_atomic(&sessions_path, &file) {
                                log::warn!(
                                    "workspace sidebar: failed to persist repaired sessions {}: {:#}",
                                    sessions_path.display(),
                                    err
                                );
                            }
                        }

                        file.sessions
                            .into_iter()
                            .map(|session| WorkspaceSidebarSession {
                                id: session.id,
                                title: session.title,
                                status: session.status,
                                status_source: session.status_source,
                                status_confidence: session.status_confidence,
                                status_reason: session.status_reason,
                                pinned: session.pinned,
                                updated_at: session.updated_at,
                            })
                            .collect()
                    }
                    Err(err) => {
                        log::warn!(
                            "workspace sidebar: failed to read sessions {}: {:#}",
                            sessions_path.display(),
                            err
                        );
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };

            sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

            projects.push(WorkspaceSidebarProject {
                id: project.id,
                name: project.name,
                root_path: project.root_path,
                sessions,
            });
        }

        projects.sort_by_key(|project| project.name.to_ascii_lowercase());
        Ok(projects)
    }

    fn sidebar_line_char_budget(&self) -> usize {
        let usable_px = self
            .sidebar_reserved_width_px()
            .saturating_sub((SIDEBAR_TEXT_LEFT_PADDING * 2.0) as usize);
        let cell_width = self.render_metrics.cell_size.width.max(1) as usize;
        (usable_px / cell_width).max(16)
    }

    fn sidebar_current_session_binding_for_render(&mut self) -> Option<(String, String)> {
        if let Some(binding) = self.sidebar_active_project_and_session() {
            return Some(binding);
        }

        let pane_id = self.get_active_pane_or_overlay()?.pane_id();
        self.sidebar_session_binding_for_pane(pane_id)
    }

    fn build_workspace_sidebar_lines(&mut self) -> Vec<SidebarLine> {
        let line_char_budget = self.sidebar_line_char_budget();
        let active_session = self.sidebar_current_session_binding_for_render();
        let mut has_status_animation = false;
        let mut lines = vec![
            SidebarLine::new("WORKSPACE", true).with_tone(SidebarLineTone::Info),
            SidebarLine::new("", false),
            SidebarLine::new("Projects", true)
                .with_tone(SidebarLineTone::Info)
                .with_trailing_button("+", SidebarAction::CreateProject),
        ];

        if self.workspace_sidebar.projects.is_empty() {
            lines.push(SidebarLine::new("  (none)", false).with_tone(SidebarLineTone::Muted));
        }

        for project in &self.workspace_sidebar.projects {
            lines.push(
                SidebarLine::action(
                    format!(
                        "- {}",
                        truncate_middle(project.name.as_str(), line_char_budget)
                    ),
                    SidebarAction::ProjectRow {
                        project_id: project.id.clone(),
                    },
                )
                .with_trailing_button(
                    "+",
                    SidebarAction::CreateSessionInProject {
                        project_id: project.id.clone(),
                    },
                )
                .with_trailing_button(
                    "⋯",
                    SidebarAction::OpenProjectContextMenu {
                        project_id: project.id.clone(),
                    },
                ),
            );

            if project.sessions.is_empty() {
                lines.push(
                    SidebarLine::new("    (no sessions)", false).with_tone(SidebarLineTone::Muted),
                );
                lines.push(SidebarLine::new("", false));
                continue;
            }

            for session in project.sessions.iter().take(8) {
                let is_current = active_session
                    .as_ref()
                    .is_some_and(|(project_id, session_id)| {
                        project_id == &project.id && session_id == &session.id
                    });
                let pin = if session.pinned { "📌" } else { "·" };
                let status = SidebarSessionStatus::parse(session.status.as_str());
                let source = parse_session_status_source(session.status_source.as_str());
                let confidence =
                    parse_session_status_confidence(session.status_confidence.as_str());
                let is_animating = sidebar_status_is_animating(status);
                if is_animating {
                    has_status_animation = true;
                }
                let status_chip = if is_animating {
                    let spinner = sidebar_status_spinner_frame(self.created.elapsed());
                    format!(
                        "[{pin}|{}|{}|{spinner}]",
                        status.badge(),
                        sidebar_status_signal_tag(source, confidence)
                    )
                } else {
                    format!(
                        "[{pin}|{}|{}]",
                        status.badge(),
                        sidebar_status_signal_tag(source, confidence)
                    )
                };
                let session_title_budget = line_char_budget
                    .saturating_sub(status_chip.chars().count().saturating_add(4))
                    .max(12);
                let mut line = SidebarLine::action(
                    format!(
                        "  {} {}",
                        status_chip,
                        truncate_middle(session.title.as_str(), session_title_budget)
                    ),
                    SidebarAction::ActivateSession {
                        project_id: project.id.clone(),
                        session_id: session.id.clone(),
                    },
                )
                .with_tone(status.tone())
                .with_trailing_button(
                    "⋯",
                    SidebarAction::OpenSessionContextMenu {
                        project_id: project.id.clone(),
                        session_id: session.id.clone(),
                    },
                );
                if is_current {
                    line = line.with_background(SidebarLineBackground::CurrentSession);
                }
                lines.push(line);

                if matches!(status, SidebarSessionStatus::NeedApprove) {
                    let detail = session.status_reason.trim();
                    let detail_head = if matches!(confidence, SessionStatusConfidence::Low) {
                        "approval maybe required [low confidence]"
                    } else {
                        "approval required"
                    };
                    let detail_text = if detail.is_empty() {
                        format!("      {detail_head}: review and choose allow/deny")
                    } else {
                        let detail_budget = line_char_budget.saturating_sub(30).max(20);
                        format!(
                            "      {detail_head}: {}",
                            truncate_middle(detail, detail_budget)
                        )
                    };
                    lines.push(
                        SidebarLine::new(detail_text, false).with_tone(SidebarLineTone::Warning),
                    );
                } else if matches!(confidence, SessionStatusConfidence::Low)
                    && matches!(
                        status,
                        SidebarSessionStatus::Loading
                            | SidebarSessionStatus::Running
                            | SidebarSessionStatus::Unknown
                    )
                {
                    let signal_text = format!(
                        "      signal: {} (low confidence)",
                        sidebar_status_source_label(source)
                    );
                    lines.push(
                        SidebarLine::new(signal_text, false).with_tone(SidebarLineTone::Muted),
                    );
                }
            }

            lines.push(SidebarLine::new("", false));
        }

        if has_status_animation {
            self.update_next_frame_time(Some(Instant::now() + SIDEBAR_STATUS_SPINNER_INTERVAL));
        }

        let resources_project = self.sidebar_active_project_for_shortcuts();
        if let Some(project_id) = resources_project.as_ref() {
            let project_label = self.sidebar_project_display_name(project_id.as_str());
            lines.push(
                SidebarLine::new(format!("Resources ({project_label})"), true)
                    .with_tone(SidebarLineTone::Info)
                    .with_trailing_button(
                        "+",
                        SidebarAction::CreateSnippet {
                            project_id: project_id.clone(),
                        },
                    ),
            );

            match self.sidebar_load_project_snippets(project_id.as_str()) {
                Ok(snippets) => {
                    if snippets.is_empty() {
                        lines.push(
                            SidebarLine::new("  Snippets: (none)", false)
                                .with_tone(SidebarLineTone::Muted),
                        );
                    } else {
                        lines.push(
                            SidebarLine::new("  Snippets", false).with_tone(SidebarLineTone::Info),
                        );
                        for snippet in snippets.iter().take(8) {
                            let snippet_name = truncate_middle(
                                snippet.name.as_str(),
                                line_char_budget.saturating_sub(8),
                            );
                            lines.push(
                                SidebarLine::action(
                                    format!("    > {snippet_name}"),
                                    SidebarAction::InsertSnippet {
                                        project_id: project_id.clone(),
                                        snippet_id: snippet.id.clone(),
                                    },
                                )
                                .with_trailing_button(
                                    "⋯",
                                    SidebarAction::OpenSnippetContextMenu {
                                        project_id: project_id.clone(),
                                        snippet_id: snippet.id.clone(),
                                    },
                                ),
                            );
                        }
                    }
                }
                Err(err) => {
                    lines.push(
                        SidebarLine::new(
                            format!(
                                "  Snippets load error: {}",
                                truncate_middle(
                                    format!("{err:#}").as_str(),
                                    line_char_budget.saturating_sub(8)
                                )
                            ),
                            false,
                        )
                        .with_tone(SidebarLineTone::Warning),
                    );
                }
            }

            lines.push(SidebarLine::new("  Env (next)", false).with_tone(SidebarLineTone::Muted));
            lines.push(SidebarLine::new("  Files (next)", false).with_tone(SidebarLineTone::Muted));
        } else {
            lines.push(SidebarLine::new("Resources", true).with_tone(SidebarLineTone::Info));
            lines.push(
                SidebarLine::new("  Select a project to manage resources", false)
                    .with_tone(SidebarLineTone::Muted),
            );
        }
        lines.push(SidebarLine::new("", false));

        if let Some(error) = &self.workspace_sidebar.last_load_error {
            lines.push(SidebarLine::new("", false));
            lines.push(SidebarLine::new("Load error", true));
            lines.push(SidebarLine::new(
                format!("  {}", truncate_middle(error, line_char_budget)),
                false,
            ));
        }

        lines
    }

    fn sidebar_request_create_project(&mut self) -> anyhow::Result<()> {
        self.sidebar_open_create_project_modal()
    }

    fn sidebar_open_create_project_modal(&mut self) -> anyhow::Result<()> {
        let row_height = self
            .render_metrics
            .cell_size
            .height
            .max(1)
            .saturating_add(12) as usize;
        let mut anchor = UIItem {
            x: 12,
            y: 32,
            width: self
                .sidebar_reserved_width_px()
                .saturating_sub(24)
                .clamp(260, 560),
            height: row_height,
            item_type: UIItemType::SidebarPanel,
        };
        if let Some(hit) = self
            .workspace_sidebar_action_hits
            .iter()
            .find(|hit| hit.action == SidebarAction::CreateProject)
        {
            anchor.x = hit.x;
            anchor.y = hit.y;
            anchor.width = hit.width.clamp(240, 560);
            anchor.height = hit.height.max(1);
        }

        let modal =
            crate::termwindow::tab_rename::TabRenameModal::new_create_project(self, anchor)?;
        self.set_modal(Rc::new(modal));
        Ok(())
    }

    pub(crate) fn sidebar_create_project_from_modal(
        &mut self,
        root_path: &str,
    ) -> anyhow::Result<()> {
        self.sidebar_create_project_with_root_path(root_path)
    }

    fn sidebar_create_project_with_root_path(&mut self, root_path: &str) -> anyhow::Result<()> {
        let normalized_root = normalize_root_path(Path::new(root_path.trim()))?;

        let projects_path = sidebar_state_root().join("projects.json");
        let mut projects_file: ProjectsFile = read_json_file_or_default(&projects_path)?;
        if let Some(existing) = projects_file
            .projects
            .iter()
            .find(|project| project.root_path == normalized_root)
        {
            self.show_toast(format!("Project \"{}\" already exists.", existing.name));
            return Ok(());
        }

        let now = Utc::now().to_rfc3339();
        let project_name = short_project_path(normalized_root.as_str());
        let project = ProjectEntry {
            id: generate_sidebar_id("proj"),
            name: if project_name.is_empty() {
                "project".to_string()
            } else {
                project_name
            },
            root_path: normalized_root,
            created_at: now.clone(),
            last_active_at: now,
        };
        let project_id = project.id.clone();
        let project_name = project.name.clone();
        projects_file.projects.push(project);
        write_json_file_atomic(&projects_path, &projects_file)?;

        let sessions_path = sidebar_state_root()
            .join("projects")
            .join(project_id)
            .join("sessions.json");
        if !sessions_path.exists() {
            write_json_file_atomic(&sessions_path, &SessionsFile::default())?;
        }

        self.force_workspace_sidebar_refresh();
        self.show_toast(format!("Created project \"{}\".", project_name));
        Ok(())
    }

    fn sidebar_load_project_snippets(&self, project_id: &str) -> anyhow::Result<Vec<SnippetEntry>> {
        let snippets_path = sidebar_project_snippets_path(project_id);
        let mut snippets_file: SnippetsFile = read_json_file_or_default(&snippets_path)?;
        snippets_file
            .snippets
            .retain(|snippet| !snippet.id.trim().is_empty());
        snippets_file
            .snippets
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(snippets_file.snippets)
    }

    fn sidebar_snippet_content(
        &self,
        project_id: &str,
        snippet_id: &str,
    ) -> anyhow::Result<String> {
        let snippets = self.sidebar_load_project_snippets(project_id)?;
        let snippet = snippets
            .into_iter()
            .find(|snippet| snippet.id == snippet_id)
            .ok_or_else(|| anyhow!("snippet not found: {project_id}/{snippet_id}"))?;
        Ok(snippet.content)
    }

    fn sidebar_request_create_snippet(&mut self, project_id: &str) -> anyhow::Result<()> {
        let row_height = self
            .render_metrics
            .cell_size
            .height
            .max(1)
            .saturating_add(12) as usize;
        let anchor = UIItem {
            x: 12,
            y: 32,
            width: self
                .sidebar_reserved_width_px()
                .saturating_sub(24)
                .clamp(260, 560),
            height: row_height,
            item_type: UIItemType::SidebarPanel,
        };
        let modal = crate::termwindow::tab_rename::TabRenameModal::new_create_snippet(
            self,
            project_id.to_string(),
            anchor,
        )?;
        self.set_modal(Rc::new(modal));
        Ok(())
    }

    pub(crate) fn sidebar_create_snippet_from_modal(
        &mut self,
        project_id: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        let content = content.trim();
        if content.is_empty() {
            self.show_toast("Snippet cannot be empty".to_string());
            return Ok(());
        }

        let snippets_path = sidebar_project_snippets_path(project_id);
        let mut snippets_file: SnippetsFile = read_json_file_or_default(&snippets_path)?;
        let now = Utc::now().to_rfc3339();
        let name = derive_snippet_name(content);
        snippets_file.snippets.push(SnippetEntry {
            id: generate_sidebar_id("snip"),
            name: name.clone(),
            content: content.to_string(),
            updated_at: now,
        });
        write_json_file_atomic(&snippets_path, &snippets_file)?;
        self.force_workspace_sidebar_refresh();
        self.show_toast(format!("Added snippet \"{name}\"."));
        Ok(())
    }

    fn sidebar_request_edit_snippet(
        &mut self,
        project_id: &str,
        snippet_id: &str,
    ) -> anyhow::Result<()> {
        let initial_content = self.sidebar_snippet_content(project_id, snippet_id)?;
        let row_height = self
            .render_metrics
            .cell_size
            .height
            .max(1)
            .saturating_add(12) as usize;
        let anchor = UIItem {
            x: 12,
            y: 32,
            width: self
                .sidebar_reserved_width_px()
                .saturating_sub(24)
                .clamp(260, 560),
            height: row_height,
            item_type: UIItemType::SidebarPanel,
        };
        let modal = crate::termwindow::tab_rename::TabRenameModal::new_edit_snippet(
            self,
            project_id.to_string(),
            snippet_id.to_string(),
            initial_content,
            anchor,
        )?;
        self.set_modal(Rc::new(modal));
        Ok(())
    }

    pub(crate) fn sidebar_edit_snippet_from_modal(
        &mut self,
        project_id: &str,
        snippet_id: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        let content = content.trim();
        if content.is_empty() {
            self.show_toast("Snippet cannot be empty".to_string());
            return Ok(());
        }

        let snippets_path = sidebar_project_snippets_path(project_id);
        let mut snippets_file: SnippetsFile = read_json_file_or_default(&snippets_path)?;
        let snippet = snippets_file
            .snippets
            .iter_mut()
            .find(|snippet| snippet.id == snippet_id)
            .ok_or_else(|| anyhow!("snippet not found in storage: {snippet_id}"))?;
        let name = derive_snippet_name(content);
        snippet.name = name.clone();
        snippet.content = content.to_string();
        snippet.updated_at = Utc::now().to_rfc3339();
        write_json_file_atomic(&snippets_path, &snippets_file)?;
        self.force_workspace_sidebar_refresh();
        self.show_toast(format!("Updated snippet \"{name}\"."));
        Ok(())
    }

    fn sidebar_insert_snippet(&mut self, project_id: &str, snippet_id: &str) -> anyhow::Result<()> {
        let content = self.sidebar_snippet_content(project_id, snippet_id)?;
        let pane = self
            .get_active_pane_or_overlay()
            .ok_or_else(|| anyhow!("no active pane to insert snippet"))?;
        pane.send_paste(content.as_str())?;
        self.show_toast("Snippet inserted".to_string());
        Ok(())
    }

    fn sidebar_delete_snippet(&mut self, project_id: &str, snippet_id: &str) -> anyhow::Result<()> {
        let snippets_path = sidebar_project_snippets_path(project_id);
        let mut snippets_file: SnippetsFile = read_json_file_or_default(&snippets_path)?;
        let idx = snippets_file
            .snippets
            .iter()
            .position(|snippet| snippet.id == snippet_id)
            .ok_or_else(|| anyhow!("snippet not found in storage: {snippet_id}"))?;
        let removed = snippets_file.snippets.remove(idx);
        write_json_file_atomic(&snippets_path, &snippets_file)?;
        self.force_workspace_sidebar_refresh();
        self.show_toast(format!("Deleted snippet \"{}\".", removed.name));
        Ok(())
    }

    fn sidebar_create_session_in_project(&mut self, project_id: &str) -> anyhow::Result<()> {
        let sessions_path = sidebar_state_root()
            .join("projects")
            .join(project_id)
            .join("sessions.json");
        let mut sessions_file: SessionsFile = read_json_file_or_default(&sessions_path)?;
        let now = Utc::now().to_rfc3339();
        let title = sidebar_next_session_title(&sessions_file.sessions);
        let session_id = generate_sidebar_id("sess");
        sessions_file.sessions.push(SessionEntry {
            id: session_id.clone(),
            title,
            status: SessionStatus::Idle.as_storage_str().to_string(),
            codex_thread_id: String::new(),
            status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
            status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
            status_reason: String::new(),
            pinned: false,
            updated_at: now.clone(),
        });
        write_json_file_atomic(&sessions_path, &sessions_file)?;
        self.sidebar_touch_project_last_active(project_id, now.as_str())?;
        self.force_workspace_sidebar_refresh();
        self.sidebar_activate_session(project_id, session_id.as_str())?;
        self.sidebar_open_session_rename_modal(project_id, session_id.as_str())?;
        Ok(())
    }

    fn sidebar_request_rename_project(&mut self, project_id: &str) -> anyhow::Result<()> {
        self.sidebar_open_project_rename_modal(project_id)
    }

    fn sidebar_open_project_rename_modal(&mut self, project_id: &str) -> anyhow::Result<()> {
        let row_height = self
            .render_metrics
            .cell_size
            .height
            .max(1)
            .saturating_add(12) as usize;
        let mut anchor = UIItem {
            x: 12,
            y: 64,
            width: self
                .sidebar_reserved_width_px()
                .saturating_sub(24)
                .clamp(220, 560),
            height: row_height,
            item_type: UIItemType::SidebarPanel,
        };
        if let Some(hit) = self.workspace_sidebar_action_hits.iter().find(|hit| {
            hit.action
                == (SidebarAction::ProjectRow {
                    project_id: project_id.to_string(),
                })
        }) {
            anchor.x = hit.x;
            anchor.y = hit.y;
            anchor.width = hit.width.clamp(220, 560);
            anchor.height = hit.height.max(1);
        }

        let modal = crate::termwindow::tab_rename::TabRenameModal::new_project(
            self,
            project_id.to_string(),
            anchor,
        )?;
        self.set_modal(Rc::new(modal));
        Ok(())
    }

    fn sidebar_rename_project_now(
        &mut self,
        project_id: &str,
        title: &str,
    ) -> anyhow::Result<String> {
        let normalized_title = title.trim();
        if normalized_title.is_empty() {
            bail!("project title cannot be empty");
        }

        let projects_path = sidebar_state_root().join("projects.json");
        let mut projects_file: ProjectsFile = read_json_file_or_default(&projects_path)?;
        let project = projects_file
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .ok_or_else(|| anyhow!("project not found in storage: {project_id}"))?;
        project.name = normalized_title.to_string();
        project.last_active_at = Utc::now().to_rfc3339();
        write_json_file_atomic(&projects_path, &projects_file)?;
        self.force_workspace_sidebar_refresh();
        Ok(normalized_title.to_string())
    }

    pub(crate) fn sidebar_rename_project_from_modal(
        &mut self,
        project_id: &str,
        title: &str,
    ) -> anyhow::Result<()> {
        let normalized_title = title.trim();
        if normalized_title.is_empty() {
            self.show_toast("Project title cannot be empty".to_string());
            return Ok(());
        }

        let previous = self.sidebar_project_display_name(project_id);
        if normalized_title == previous.trim() {
            return Ok(());
        }

        let updated = self.sidebar_rename_project_now(project_id, normalized_title)?;
        self.show_toast(format!("Renamed project to \"{updated}\"."));
        Ok(())
    }

    fn sidebar_touch_project_last_active(
        &mut self,
        project_id: &str,
        last_active_at: &str,
    ) -> anyhow::Result<()> {
        let projects_path = sidebar_state_root().join("projects.json");
        let mut projects_file: ProjectsFile = read_json_file_or_default(&projects_path)?;
        if let Some(project) = projects_file
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
        {
            project.last_active_at = last_active_at.to_string();
        }
        write_json_file_atomic(&projects_path, &projects_file)?;
        Ok(())
    }

    pub(crate) fn sidebar_current_working_directory(&self) -> Option<PathBuf> {
        self.get_active_pane_or_overlay()
            .and_then(|pane| {
                pane.get_current_working_dir(CachePolicy::AllowStale)
                    .and_then(|url| url.to_file_path().ok())
            })
            .filter(|cwd| cwd.is_absolute())
    }

    fn sidebar_activate_session(
        &mut self,
        project_id: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        self.prune_workspace_sidebar_tab_bindings();
        let key = sidebar_session_key(project_id, session_id);

        if let Some(tab_id) = self.workspace_sidebar_session_to_tab.get(&key).copied() {
            if self.sidebar_activate_tab(tab_id) {
                return Ok(());
            }
            self.sidebar_unbind_tab(tab_id);
        }

        if self
            .workspace_sidebar_pending_opens
            .iter()
            .any(|pending| pending.project_id == project_id && pending.session_id == session_id)
        {
            return Ok(());
        }

        let session = self
            .sidebar_session_meta(project_id, session_id)
            .ok_or_else(|| anyhow!("session not found: {project_id}/{session_id}"))?;

        self.workspace_sidebar_pending_opens
            .push_back(WorkspaceSidebarPendingOpen {
                project_id: project_id.to_string(),
                session_id: session_id.to_string(),
                session_title: session.title.clone(),
            });

        let spawn = SpawnCommand {
            cwd: Some(PathBuf::from(session.root_path)),
            domain: SpawnTabDomain::CurrentPaneDomain,
            ..SpawnCommand::default()
        };
        self.spawn_command(&spawn, SpawnWhere::NewTab);
        Ok(())
    }

    fn sidebar_activate_tab(&mut self, tab_id: TabId) -> bool {
        let mux = Mux::get();
        let Some(window) = mux.get_window(self.mux_window_id) else {
            return false;
        };
        let Some(tab_idx) = window.idx_by_id(tab_id) else {
            return false;
        };
        drop(window);
        self.activate_tab(tab_idx as isize).is_ok()
    }

    fn sidebar_bind_session_to_tab(&mut self, project_id: &str, session_id: &str, tab_id: TabId) {
        self.prune_workspace_sidebar_tab_bindings();

        if let Some((old_project, old_session)) = self
            .workspace_sidebar_tab_to_session
            .insert(tab_id, (project_id.to_string(), session_id.to_string()))
        {
            self.workspace_sidebar_session_to_tab
                .remove(&sidebar_session_key(
                    old_project.as_str(),
                    old_session.as_str(),
                ));
        }

        let key = sidebar_session_key(project_id, session_id);
        if let Some(old_tab_id) = self.workspace_sidebar_session_to_tab.insert(key, tab_id) {
            if old_tab_id != tab_id {
                self.workspace_sidebar_tab_to_session.remove(&old_tab_id);
            }
        }
    }

    fn sidebar_session_meta(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Option<SidebarSessionMeta> {
        let project = self
            .workspace_sidebar
            .projects
            .iter()
            .find(|project| project.id == project_id)?;
        let session = project
            .sessions
            .iter()
            .find(|session| session.id == session_id)?;
        Some(SidebarSessionMeta {
            title: session.title.clone(),
            root_path: project.root_path.clone(),
        })
    }

    fn sidebar_lookup_session_pinned(&self, project_id: &str, session_id: &str) -> Option<bool> {
        self.workspace_sidebar
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .map(|session| session.pinned)
            })
    }

    fn sidebar_set_session_pin(
        &mut self,
        project_id: &str,
        session_id: &str,
        pinned: bool,
    ) -> anyhow::Result<()> {
        let sessions_path = sidebar_state_root()
            .join("projects")
            .join(project_id)
            .join("sessions.json");
        if !sessions_path.exists() {
            bail!("sessions file not found: {}", sessions_path.display());
        }

        let mut sessions_file: SessionsFile = read_json_file(&sessions_path)?;
        let session = sessions_file
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("session not found in storage: {session_id}"))?;

        session.pinned = pinned;
        session.updated_at = Utc::now().to_rfc3339();
        write_json_file_atomic(&sessions_path, &sessions_file)?;
        self.force_workspace_sidebar_refresh();
        Ok(())
    }

    fn sidebar_request_close_other_sessions(
        &mut self,
        project_id: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let preview = self.sidebar_collect_close_other_tabs(project_id, session_id);
        if preview.is_empty() {
            self.show_toast("No unpinned sessions to close".to_string());
            return Ok(());
        }

        let project_name = self.sidebar_project_display_name(project_id);
        let session_name = self.sidebar_session_display_name(project_id, session_id);
        let count = preview.len();
        let message = format!(
            "Close {count} unpinned {} in \"{project_name}\"?\nKeep current \"{session_name}\" and all pinned sessions.",
            pluralize_session(count)
        );

        self.sidebar_confirm_close_action(
            message,
            SidebarCloseAction::CloseOthers {
                project_id: project_id.to_string(),
                session_id: session_id.to_string(),
            },
        )
    }

    fn sidebar_request_close_all_unpinned(&mut self, project_id: &str) -> anyhow::Result<()> {
        let preview = self.sidebar_collect_close_all_tabs(project_id);
        if preview.is_empty() {
            self.show_toast("No unpinned sessions to close".to_string());
            return Ok(());
        }

        let project_name = self.sidebar_project_display_name(project_id);
        let count = preview.len();
        let message = format!(
            "Close all {count} unpinned {} in \"{project_name}\"?\nPinned sessions will remain open.",
            pluralize_session(count)
        );

        self.sidebar_confirm_close_action(
            message,
            SidebarCloseAction::CloseAll {
                project_id: project_id.to_string(),
            },
        )
    }

    fn sidebar_request_rename_session(
        &mut self,
        project_id: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        self.sidebar_open_session_rename_modal(project_id, session_id)
    }

    pub(crate) fn sidebar_open_project_context_menu(
        &mut self,
        project_id: &str,
        mouse_x: isize,
        mouse_y: isize,
    ) -> anyhow::Result<()> {
        let project_id = project_id.to_string();
        let items = vec![
            SidebarContextMenuItem::new(
                "New Session",
                SidebarAction::CreateSessionInProject {
                    project_id: project_id.clone(),
                },
            ),
            SidebarContextMenuItem::new(
                "Rename Project",
                SidebarAction::RenameProject {
                    project_id: project_id.clone(),
                },
            ),
            SidebarContextMenuItem::danger(
                "Delete Project",
                SidebarAction::DeleteProject { project_id },
            ),
        ];

        let anchor = UIItem {
            x: mouse_x.max(0) as usize,
            y: mouse_y.max(0) as usize,
            width: self
                .sidebar_reserved_width_px()
                .saturating_sub(24)
                .clamp(220, 360),
            height: self
                .render_metrics
                .cell_size
                .height
                .max(1)
                .saturating_add(8) as usize,
            item_type: UIItemType::SidebarPanel,
        };
        let modal = SidebarContextMenuModal::new(self, anchor, items)?;
        self.set_modal(Rc::new(modal));
        Ok(())
    }

    pub(crate) fn sidebar_open_session_context_menu(
        &mut self,
        project_id: &str,
        session_id: &str,
        mouse_x: isize,
        mouse_y: isize,
    ) -> anyhow::Result<()> {
        let pinned = self
            .sidebar_lookup_session_pinned(project_id, session_id)
            .unwrap_or(false);
        let pin_label = if pinned {
            "Unpin Session"
        } else {
            "Pin Session"
        };
        let project_id = project_id.to_string();
        let session_id = session_id.to_string();
        let items = vec![
            SidebarContextMenuItem::new(
                "Open Session",
                SidebarAction::ActivateSession {
                    project_id: project_id.clone(),
                    session_id: session_id.clone(),
                },
            ),
            SidebarContextMenuItem::new(
                "Rename Session",
                SidebarAction::RenameSession {
                    project_id: project_id.clone(),
                    session_id: session_id.clone(),
                },
            ),
            SidebarContextMenuItem::new(
                pin_label,
                SidebarAction::TogglePin {
                    project_id: project_id.clone(),
                    session_id: session_id.clone(),
                    pinned: !pinned,
                },
            ),
            SidebarContextMenuItem::new(
                "Close Others (keep pinned)",
                SidebarAction::CloseOthers {
                    project_id: project_id.clone(),
                    session_id: session_id.clone(),
                },
            ),
            SidebarContextMenuItem::danger(
                "Delete Session",
                SidebarAction::DeleteSession {
                    project_id,
                    session_id,
                },
            ),
        ];

        let anchor = UIItem {
            x: mouse_x.max(0) as usize,
            y: mouse_y.max(0) as usize,
            width: self
                .sidebar_reserved_width_px()
                .saturating_sub(24)
                .clamp(220, 360),
            height: self
                .render_metrics
                .cell_size
                .height
                .max(1)
                .saturating_add(8) as usize,
            item_type: UIItemType::SidebarPanel,
        };
        let modal = SidebarContextMenuModal::new(self, anchor, items)?;
        self.set_modal(Rc::new(modal));
        Ok(())
    }

    pub(crate) fn sidebar_open_snippet_context_menu(
        &mut self,
        project_id: &str,
        snippet_id: &str,
        mouse_x: isize,
        mouse_y: isize,
    ) -> anyhow::Result<()> {
        let project_id = project_id.to_string();
        let snippet_id = snippet_id.to_string();
        let items = vec![
            SidebarContextMenuItem::new(
                "Insert Snippet",
                SidebarAction::InsertSnippet {
                    project_id: project_id.clone(),
                    snippet_id: snippet_id.clone(),
                },
            ),
            SidebarContextMenuItem::new(
                "Edit Snippet",
                SidebarAction::EditSnippet {
                    project_id: project_id.clone(),
                    snippet_id: snippet_id.clone(),
                },
            ),
            SidebarContextMenuItem::danger(
                "Delete Snippet",
                SidebarAction::DeleteSnippet {
                    project_id,
                    snippet_id,
                },
            ),
        ];

        let anchor = UIItem {
            x: mouse_x.max(0) as usize,
            y: mouse_y.max(0) as usize,
            width: self
                .sidebar_reserved_width_px()
                .saturating_sub(24)
                .clamp(220, 360),
            height: self
                .render_metrics
                .cell_size
                .height
                .max(1)
                .saturating_add(8) as usize,
            item_type: UIItemType::SidebarPanel,
        };
        let modal = SidebarContextMenuModal::new(self, anchor, items)?;
        self.set_modal(Rc::new(modal));
        Ok(())
    }

    fn sidebar_request_delete_session(
        &mut self,
        project_id: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let project_name = self.sidebar_project_display_name(project_id);
        let session_name = self.sidebar_session_display_name(project_id, session_id);
        let pinned = self
            .sidebar_lookup_session_pinned(project_id, session_id)
            .unwrap_or(false);

        let message = if pinned {
            format!(
                "Delete pinned session \"{session_name}\" from \"{project_name}\"?\nThis cannot be undone."
            )
        } else {
            format!(
                "Delete session \"{session_name}\" from \"{project_name}\"?\nThis cannot be undone."
            )
        };

        self.sidebar_confirm_close_action(
            message,
            SidebarCloseAction::DeleteSession {
                project_id: project_id.to_string(),
                session_id: session_id.to_string(),
            },
        )
    }

    fn sidebar_request_delete_project(&mut self, project_id: &str) -> anyhow::Result<()> {
        let project_name = self.sidebar_project_display_name(project_id);
        let (session_count, pinned_count) = self
            .workspace_sidebar
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| {
                let sessions = project.sessions.len();
                let pinned = project
                    .sessions
                    .iter()
                    .filter(|session| session.pinned)
                    .count();
                (sessions, pinned)
            })
            .unwrap_or((0, 0));

        let message = if session_count == 0 {
            format!("Delete project \"{project_name}\"?\nThis cannot be undone.")
        } else if pinned_count > 0 {
            format!(
                "Delete project \"{project_name}\" with {session_count} {} ({pinned_count} pinned)?\nThis cannot be undone.",
                pluralize_session(session_count)
            )
        } else {
            format!(
                "Delete project \"{project_name}\" with {session_count} {}?\nThis cannot be undone.",
                pluralize_session(session_count)
            )
        };

        self.sidebar_confirm_close_action(
            message,
            SidebarCloseAction::DeleteProject {
                project_id: project_id.to_string(),
            },
        )
    }

    fn sidebar_open_session_rename_modal(
        &mut self,
        project_id: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let rename_action = SidebarAction::RenameSession {
            project_id: project_id.to_string(),
            session_id: session_id.to_string(),
        };
        let activate_action = SidebarAction::ActivateSession {
            project_id: project_id.to_string(),
            session_id: session_id.to_string(),
        };
        let hit_anchor = self
            .workspace_sidebar_action_hits
            .iter()
            .find(|hit| hit.action == rename_action || hit.action == activate_action);

        let row_height = self
            .render_metrics
            .cell_size
            .height
            .max(1)
            .saturating_add(12) as usize;
        let (anchor_x, anchor_y, anchor_width, anchor_height) =
            if let Some((x, y, width, height)) = self.sidebar_bounds() {
                let sidebar_width = width.max(1.0) as usize;
                let modal_width = sidebar_width.saturating_sub(24).clamp(220, 520);
                let x = x.max(0.0) as usize + (sidebar_width.saturating_sub(modal_width) / 2);
                if let Some(hit) = hit_anchor {
                    let y = hit.y;
                    let hit_height = hit.height.max(1);
                    (x, y, modal_width, hit_height)
                } else {
                    let y = y.max(0.0) as usize
                        + ((height.max(1.0) as usize).saturating_mul(35) / 100).min(
                            (height.max(1.0) as usize).saturating_sub(row_height.saturating_add(4)),
                        );
                    (x, y, modal_width, row_height)
                }
            } else {
                let modal_width = self
                    .dimensions
                    .pixel_width
                    .saturating_sub(48)
                    .clamp(260, 560);
                let x = self
                    .dimensions
                    .pixel_width
                    .saturating_sub(modal_width)
                    .saturating_div(2);
                let y = self
                    .dimensions
                    .pixel_height
                    .saturating_sub(row_height)
                    .saturating_div(2);
                (x, y, modal_width, row_height)
            };

        let anchor = UIItem {
            x: anchor_x,
            y: anchor_y,
            width: anchor_width,
            height: anchor_height,
            item_type: UIItemType::SidebarPanel,
        };
        let modal = crate::termwindow::tab_rename::TabRenameModal::new_session(
            self,
            project_id.to_string(),
            session_id.to_string(),
            anchor,
        )?;
        self.set_modal(Rc::new(modal));
        Ok(())
    }

    fn sidebar_confirm_close_action(
        &mut self,
        message: String,
        action: SidebarCloseAction,
    ) -> anyhow::Result<()> {
        let mux = Mux::get();
        let tab = mux
            .get_active_tab_for_window(self.mux_window_id)
            .ok_or_else(|| anyhow!("no active tab for confirmation overlay"))?;
        let window = self
            .window
            .clone()
            .ok_or_else(|| anyhow!("window is unavailable for confirmation overlay"))?;

        let (overlay, future) =
            crate::overlay::start_overlay(self, &tab, move |_tab_id, mut term| {
                crate::overlay::confirm::run_confirmation(message.as_str(), &mut term)
            });
        self.assign_overlay(tab.tab_id(), overlay);

        promise::spawn::spawn(async move {
            match future.await {
                Ok(true) => {
                    window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                        if let Err(err) = term_window.sidebar_execute_confirmed_close(action) {
                            log::warn!(
                                "workspace sidebar: failed to execute confirmed close action: {:#}",
                                err
                            );
                            term_window.show_toast("Failed to apply sidebar action".to_string());
                        }
                    })));
                }
                Ok(false) => {}
                Err(err) => {
                    log::warn!("workspace sidebar: confirmation overlay failed: {:#}", err);
                    window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                        term_window.show_toast("Close action canceled".to_string());
                    })));
                }
            }
            anyhow::Result::<()>::Ok(())
        })
        .detach();

        Ok(())
    }

    fn sidebar_execute_confirmed_close(
        &mut self,
        action: SidebarCloseAction,
    ) -> anyhow::Result<()> {
        match action {
            SidebarCloseAction::CloseOthers {
                project_id,
                session_id,
            } => {
                let count = self
                    .sidebar_close_other_sessions_now(project_id.as_str(), session_id.as_str())?;
                if count == 0 {
                    self.show_toast("No unpinned sessions to close".to_string());
                } else {
                    self.show_toast(format!("Closed {count} {}.", pluralize_session(count)));
                }
            }
            SidebarCloseAction::CloseAll { project_id } => {
                let count = self.sidebar_close_all_unpinned_now(project_id.as_str())?;
                if count == 0 {
                    self.show_toast("No unpinned sessions to close".to_string());
                } else {
                    self.show_toast(format!("Closed {count} {}.", pluralize_session(count)));
                }
            }
            SidebarCloseAction::DeleteSession {
                project_id,
                session_id,
            } => {
                let deleted_title =
                    self.sidebar_delete_session_now(project_id.as_str(), session_id.as_str())?;
                self.show_toast(format!("Deleted session \"{deleted_title}\"."));
            }
            SidebarCloseAction::DeleteProject { project_id } => {
                let deleted_name = self.sidebar_delete_project_now(project_id.as_str())?;
                self.show_toast(format!("Deleted project \"{deleted_name}\"."));
            }
        }
        Ok(())
    }

    fn sidebar_close_other_sessions_now(
        &mut self,
        project_id: &str,
        session_id: &str,
    ) -> anyhow::Result<usize> {
        let to_close = self.sidebar_collect_close_other_tabs(project_id, session_id);
        Ok(self.sidebar_close_tabs(&to_close))
    }

    fn sidebar_close_all_unpinned_now(&mut self, project_id: &str) -> anyhow::Result<usize> {
        let to_close = self.sidebar_collect_close_all_tabs(project_id);
        Ok(self.sidebar_close_tabs(&to_close))
    }

    fn sidebar_rename_session_now(
        &mut self,
        project_id: &str,
        session_id: &str,
        title: &str,
    ) -> anyhow::Result<String> {
        let normalized_title = title.trim();
        if normalized_title.is_empty() {
            bail!("session title cannot be empty");
        }

        let sessions_path = sidebar_state_root()
            .join("projects")
            .join(project_id)
            .join("sessions.json");
        if !sessions_path.exists() {
            bail!("sessions file not found: {}", sessions_path.display());
        }

        let mut sessions_file: SessionsFile = read_json_file(&sessions_path)?;
        let session = sessions_file
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("session not found in storage: {session_id}"))?;

        session.title = normalized_title.to_string();
        session.updated_at = Utc::now().to_rfc3339();
        write_json_file_atomic(&sessions_path, &sessions_file)?;

        let key = sidebar_session_key(project_id, session_id);
        if let Some(tab_id) = self.workspace_sidebar_session_to_tab.get(&key).copied() {
            let mux = Mux::get();
            if let Some(tab) = mux.get_tab(tab_id) {
                tab.set_title(normalized_title);
            }
        }

        self.force_workspace_sidebar_refresh();
        Ok(normalized_title.to_string())
    }

    pub(crate) fn sidebar_rename_session_from_modal(
        &mut self,
        project_id: &str,
        session_id: &str,
        title: &str,
    ) -> anyhow::Result<()> {
        let normalized_title = title.trim();
        if normalized_title.is_empty() {
            self.show_toast("Session title cannot be empty".to_string());
            return Ok(());
        }

        let previous = self.sidebar_session_display_name(project_id, session_id);
        if normalized_title == previous.trim() {
            return Ok(());
        }

        let updated = self.sidebar_rename_session_now(project_id, session_id, normalized_title)?;
        self.show_toast(format!("Renamed session to \"{updated}\"."));
        Ok(())
    }

    fn sidebar_delete_session_now(
        &mut self,
        project_id: &str,
        session_id: &str,
    ) -> anyhow::Result<String> {
        let sessions_path = sidebar_state_root()
            .join("projects")
            .join(project_id)
            .join("sessions.json");
        if !sessions_path.exists() {
            bail!("sessions file not found: {}", sessions_path.display());
        }

        let mut sessions_file: SessionsFile = read_json_file(&sessions_path)?;
        let idx = sessions_file
            .sessions
            .iter()
            .position(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("session not found in storage: {session_id}"))?;
        let removed = sessions_file.sessions.remove(idx);
        write_json_file_atomic(&sessions_path, &sessions_file)?;

        self.prune_workspace_sidebar_tab_bindings();
        let key = sidebar_session_key(project_id, session_id);
        if let Some(tab_id) = self.workspace_sidebar_session_to_tab.get(&key).copied() {
            self.sidebar_close_tabs(&[tab_id]);
        }

        self.force_workspace_sidebar_refresh();
        Ok(removed.title)
    }

    fn sidebar_delete_project_now(&mut self, project_id: &str) -> anyhow::Result<String> {
        let projects_path = sidebar_state_root().join("projects.json");
        let mut projects_file: ProjectsFile = read_json_file_or_default(&projects_path)?;
        let idx = projects_file
            .projects
            .iter()
            .position(|project| project.id == project_id)
            .ok_or_else(|| anyhow!("project not found in storage: {project_id}"))?;
        let removed = projects_file.projects.remove(idx);
        write_json_file_atomic(&projects_path, &projects_file)?;

        let project_dir = sidebar_state_root().join("projects").join(project_id);
        if project_dir.exists() {
            std::fs::remove_dir_all(&project_dir)
                .with_context(|| format!("remove {}", project_dir.display()))?;
        }

        self.workspace_sidebar_pending_opens
            .retain(|pending| pending.project_id != project_id);
        self.prune_workspace_sidebar_tab_bindings();
        let to_close: Vec<TabId> = self
            .workspace_sidebar_tab_to_session
            .iter()
            .filter_map(|(tab_id, (tab_project_id, _))| {
                (tab_project_id == project_id).then_some(*tab_id)
            })
            .collect();
        self.sidebar_close_tabs(&to_close);

        self.force_workspace_sidebar_refresh();
        Ok(removed.name)
    }

    fn sidebar_collect_close_other_tabs(
        &mut self,
        project_id: &str,
        session_id: &str,
    ) -> Vec<TabId> {
        self.prune_workspace_sidebar_tab_bindings();
        let pinned = self.sidebar_pinned_sessions(project_id);
        let mut to_close: Vec<TabId> = Vec::new();

        for (tab_id, (tab_project_id, tab_session_id)) in &self.workspace_sidebar_tab_to_session {
            if tab_project_id != project_id {
                continue;
            }
            if tab_session_id == session_id {
                continue;
            }
            if pinned.contains(tab_session_id) {
                continue;
            }
            to_close.push(*tab_id);
        }

        to_close
    }

    fn sidebar_collect_close_all_tabs(&mut self, project_id: &str) -> Vec<TabId> {
        self.prune_workspace_sidebar_tab_bindings();
        let pinned = self.sidebar_pinned_sessions(project_id);
        let mut to_close: Vec<TabId> = Vec::new();

        for (tab_id, (tab_project_id, tab_session_id)) in &self.workspace_sidebar_tab_to_session {
            if tab_project_id != project_id {
                continue;
            }
            if pinned.contains(tab_session_id) {
                continue;
            }
            to_close.push(*tab_id);
        }

        to_close
    }

    fn sidebar_close_tabs(&mut self, tab_ids: &[TabId]) -> usize {
        if tab_ids.is_empty() {
            return 0;
        }

        let mux = Mux::get();
        let mut closed = 0;
        for tab_id in tab_ids {
            self.sidebar_unbind_tab(*tab_id);
            if mux.get_tab(*tab_id).is_some() {
                mux.remove_tab(*tab_id);
                closed += 1;
            }
        }
        closed
    }

    pub(crate) fn sidebar_project_display_name(&self, project_id: &str) -> String {
        self.workspace_sidebar
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.name.clone())
            .unwrap_or_else(|| project_id.to_string())
    }

    pub(crate) fn sidebar_session_display_name(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> String {
        self.workspace_sidebar
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .map(|session| session.title.clone())
            })
            .unwrap_or_else(|| session_id.to_string())
    }

    fn sidebar_pinned_sessions(&self, project_id: &str) -> HashSet<String> {
        self.workspace_sidebar
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| {
                project
                    .sessions
                    .iter()
                    .filter(|session| session.pinned)
                    .map(|session| session.id.clone())
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default()
    }
}

fn sidebar_state_root() -> PathBuf {
    config::HOME_DIR.join(".kaku")
}

fn sidebar_ui_state_path() -> PathBuf {
    sidebar_state_root().join(SIDEBAR_UI_STATE_RELATIVE_PATH)
}

fn sidebar_project_resources_root(project_id: &str) -> PathBuf {
    sidebar_state_root()
        .join("projects")
        .join(project_id)
        .join("resources")
}

fn sidebar_project_snippets_path(project_id: &str) -> PathBuf {
    sidebar_project_resources_root(project_id).join("snippets.json")
}

fn sidebar_session_key(project_id: &str, session_id: &str) -> String {
    format!("{project_id}::{session_id}")
}

fn derive_snippet_name(content: &str) -> String {
    let compact = content
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    let candidate = if compact.is_empty() {
        content.trim()
    } else {
        compact.as_str()
    };
    if candidate.is_empty() {
        return "snippet".to_string();
    }
    truncate_middle(candidate, 36)
}

fn ensure_sidebar_project_for_cwd(cwd: &Path) -> anyhow::Result<String> {
    let root_path = normalize_root_path(cwd)?;
    let projects_path = sidebar_state_root().join("projects.json");
    let mut projects_file: ProjectsFile = read_json_file_or_default(&projects_path)?;
    let now = Utc::now().to_rfc3339();

    if let Some(existing_idx) = projects_file
        .projects
        .iter()
        .position(|project| project.root_path == root_path)
    {
        let mut needs_write = false;
        let project_id = {
            let project = projects_file
                .projects
                .get_mut(existing_idx)
                .ok_or_else(|| anyhow!("project index out of bounds"))?;
            if project.last_active_at.is_empty() {
                project.last_active_at = now.clone();
                needs_write = true;
            }
            project.id.clone()
        };
        if needs_write {
            write_json_file_atomic(&projects_path, &projects_file)?;
        }
        return Ok(project_id);
    }

    let project_id = generate_sidebar_id("proj");
    let project_name = short_project_path(root_path.as_str());
    projects_file.projects.push(ProjectEntry {
        id: project_id.clone(),
        name: project_name,
        root_path,
        created_at: now.clone(),
        last_active_at: now,
    });
    write_json_file_atomic(&projects_path, &projects_file)?;

    let _ = ensure_sidebar_project_has_session(project_id.as_str())?;
    Ok(project_id)
}

fn ensure_sidebar_project_has_session(project_id: &str) -> anyhow::Result<String> {
    let sessions_path = sidebar_state_root()
        .join("projects")
        .join(project_id)
        .join("sessions.json");
    let mut sessions_file: SessionsFile = read_json_file_or_default(&sessions_path)?;

    if sessions_file.sessions.is_empty() {
        let now = Utc::now().to_rfc3339();
        let session_id = generate_sidebar_id("sess");
        sessions_file.sessions.push(SessionEntry {
            id: session_id.clone(),
            title: "session-1".to_string(),
            status: SessionStatus::Idle.as_storage_str().to_string(),
            codex_thread_id: String::new(),
            status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
            status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
            status_reason: String::new(),
            pinned: false,
            updated_at: now,
        });
        write_json_file_atomic(&sessions_path, &sessions_file)?;
        return Ok(session_id);
    }

    let selected = sessions_file
        .sessions
        .iter()
        .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
        .ok_or_else(|| anyhow!("project has no sessions after load: {project_id}"))?;
    Ok(selected.id.clone())
}

fn pick_project_for_cwd<'a>(
    projects: &'a [WorkspaceSidebarProject],
    cwd: &Path,
) -> Option<&'a WorkspaceSidebarProject> {
    projects
        .iter()
        .filter_map(|project| {
            let root = Path::new(project.root_path.as_str());
            cwd.starts_with(root)
                .then_some((root.components().count(), project))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, project)| project)
}

fn pick_session_for_inferred_binding<'a>(
    project: &'a WorkspaceSidebarProject,
    bound_session_keys: &HashSet<String>,
    tab_title: &str,
) -> Option<&'a WorkspaceSidebarSession> {
    let unbound: Vec<&WorkspaceSidebarSession> = project
        .sessions
        .iter()
        .filter(|session| {
            let key = sidebar_session_key(project.id.as_str(), session.id.as_str());
            !bound_session_keys.contains(&key)
        })
        .collect();

    let candidates: Vec<&WorkspaceSidebarSession> = if unbound.is_empty() {
        project.sessions.iter().collect()
    } else {
        unbound
    };
    if candidates.is_empty() {
        return None;
    }

    let normalized_title = tab_title.trim();
    if !normalized_title.is_empty() {
        if let Some(session) = candidates
            .iter()
            .find(|session| session.title.as_str() == normalized_title)
        {
            return Some(*session);
        }
    }

    if candidates.len() == 1 {
        return Some(candidates[0]);
    }

    let now = Utc::now();
    if let Some(active_like) = candidates.iter().find(|session| {
        matches!(
            SessionStatus::parse_storage(session.status.as_str()),
            Some(SessionStatus::Loading | SessionStatus::NeedApprove | SessionStatus::Running)
        ) && !is_stale_transient_session_status(
            session.status.as_str(),
            session.updated_at.as_str(),
            now,
        )
    }) {
        return Some(*active_like);
    }

    let non_stale_candidates: Vec<&WorkspaceSidebarSession> = candidates
        .iter()
        .copied()
        .filter(|session| {
            !is_stale_transient_session_status(
                session.status.as_str(),
                session.updated_at.as_str(),
                now,
            )
        })
        .collect();

    let ranked = if non_stale_candidates.is_empty() {
        candidates
    } else {
        non_stale_candidates
    };

    ranked
        .into_iter()
        .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
}

fn normalize_transient_session_status(
    status: &str,
    updated_at: &str,
    now: DateTime<Utc>,
) -> String {
    if is_stale_transient_session_status(status, updated_at, now) {
        return SessionStatus::Idle.as_storage_str().to_string();
    }

    SessionStatus::parse_storage(status)
        .map(SessionStatus::as_storage_str)
        .unwrap_or(SessionStatus::Idle.as_storage_str())
        .to_string()
}

fn is_codex_command(command: &str) -> bool {
    shared_is_codex_like_command(command)
}

fn is_codex_process_name(process_name: &str) -> bool {
    shared_is_codex_process_name(process_name)
}

fn is_stale_transient_session_status(status: &str, updated_at: &str, now: DateTime<Utc>) -> bool {
    let Some(status) = SessionStatus::parse_storage(status) else {
        return false;
    };
    if !matches!(
        status,
        SessionStatus::Loading | SessionStatus::NeedApprove | SessionStatus::Running
    ) {
        return false;
    }

    let stale_after = chrono::Duration::from_std(SIDEBAR_TRANSIENT_STATUS_STALE_AFTER)
        .unwrap_or_else(|_| chrono::Duration::hours(6));
    let parsed = match DateTime::parse_from_rfc3339(updated_at) {
        Ok(value) => value.with_timezone(&Utc),
        Err(_) => return true,
    };
    now.signed_duration_since(parsed) >= stale_after
}

fn pluralize_session(count: usize) -> &'static str {
    if count == 1 {
        "session"
    } else {
        "sessions"
    }
}

fn schema_version() -> u8 {
    1
}

fn sidebar_default_visible() -> bool {
    SIDEBAR_DEFAULT_VISIBLE
}

fn read_json_file<T>(path: &Path) -> anyhow::Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("parse {}", path.display()))
}

fn read_json_file_or_default<T>(path: &Path) -> anyhow::Result<T>
where
    T: for<'de> Deserialize<'de> + Default,
{
    match std::fs::read_to_string(path) {
        Ok(content) => {
            serde_json::from_str(&content).with_context(|| format!("parse {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn write_json_file_atomic<T>(path: &Path, value: &T) -> anyhow::Result<()>
where
    T: Serialize,
{
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("invalid state path: {}", path.display()))?;
    config::create_user_owned_dirs(parent)
        .with_context(|| format!("create state directory {}", parent.display()))?;

    let bytes = serde_json::to_vec_pretty(value).context("serialize state to json")?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid state path: {}", path.display()))?;
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        file_name,
        std::process::id(),
        now_nanos
    ));

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .with_context(|| format!("create {}", tmp_path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("write {}", tmp_path.display()))?;
    file.flush()
        .with_context(|| format!("flush {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "move temporary sidebar state {} into place at {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn normalize_root_path(path: &Path) -> anyhow::Result<String> {
    let expanded = match path.to_str().map(str::trim) {
        Some("~") => config::HOME_DIR.clone(),
        Some(raw) if raw.starts_with("~/") => {
            let suffix = raw.trim_start_matches("~/");
            config::HOME_DIR.join(suffix)
        }
        _ => path.to_path_buf(),
    };

    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .context("read current directory")?
            .join(expanded)
    };

    if !absolute.is_dir() {
        bail!("root path is not a directory: {}", absolute.display());
    }

    let normalized = absolute.canonicalize().unwrap_or_else(|_| absolute.clone());

    Ok(normalized.to_string_lossy().into_owned())
}

fn generate_sidebar_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = SIDEBAR_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{:x}{:x}{:x}", nanos, std::process::id(), seq)
}

fn sidebar_next_session_title(existing: &[SessionEntry]) -> String {
    let mut idx = existing.len().saturating_add(1);
    loop {
        let candidate = format!("session-{idx}");
        if !existing.iter().any(|session| session.title == candidate) {
            return candidate;
        }
        idx = idx.saturating_add(1);
    }
}

fn sidebar_action_needs_pointer_position(action: &SidebarAction) -> bool {
    matches!(
        action,
        SidebarAction::OpenSessionContextMenu { .. }
            | SidebarAction::OpenProjectContextMenu { .. }
            | SidebarAction::OpenSnippetContextMenu { .. }
    )
}

fn short_project_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

fn truncate_middle(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }

    if max_chars <= 3 {
        return "...".to_string();
    }

    let keep = (max_chars - 3) / 2;
    let head: String = input.chars().take(keep).collect();
    let tail: String = input
        .chars()
        .rev()
        .take(keep)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}...{tail}")
}

fn sidebar_background_color(background: LinearRgba) -> LinearRgba {
    let luminance = 0.299 * background.0 + 0.587 * background.1 + 0.114 * background.2;
    if luminance > 0.5 {
        LinearRgba(
            (background.0 * 0.92).clamp(0.0, 1.0),
            (background.1 * 0.92).clamp(0.0, 1.0),
            (background.2 * 0.92).clamp(0.0, 1.0),
            0.97,
        )
    } else {
        LinearRgba(
            (background.0 + 0.08).clamp(0.0, 1.0),
            (background.1 + 0.08).clamp(0.0, 1.0),
            (background.2 + 0.08).clamp(0.0, 1.0),
            0.97,
        )
    }
}

fn sidebar_border_color(foreground: LinearRgba) -> LinearRgba {
    foreground.mul_alpha(0.35)
}

fn sidebar_header_background_color(background: LinearRgba, foreground: LinearRgba) -> LinearRgba {
    mix_linear(background, foreground.mul_alpha(0.22), 0.26).mul_alpha(0.96)
}

fn sidebar_more_button_background(background: LinearRgba, foreground: LinearRgba) -> LinearRgba {
    mix_linear(background, foreground.mul_alpha(0.18), 0.22).mul_alpha(0.98)
}

fn sidebar_more_button_foreground(foreground: LinearRgba) -> LinearRgba {
    foreground.mul_alpha(0.92)
}

fn sidebar_row_background_color(
    background: LinearRgba,
    foreground: LinearRgba,
    row_background: SidebarLineBackground,
) -> LinearRgba {
    match row_background {
        SidebarLineBackground::CurrentSession => {
            // Keep this between panel background and header strip intensity:
            // distinct from default row, but not as strong as section headers.
            mix_linear(background, foreground.mul_alpha(0.22), 0.18).mul_alpha(0.97)
        }
    }
}

fn sidebar_line_color(base: LinearRgba, tone: SidebarLineTone) -> LinearRgba {
    match tone {
        SidebarLineTone::Default => base,
        SidebarLineTone::Muted => base.mul_alpha(0.72),
        SidebarLineTone::Info => mix_linear(base, LinearRgba(0.38, 0.70, 0.96, base.3), 0.68),
        SidebarLineTone::Warning => mix_linear(base, LinearRgba(0.96, 0.74, 0.22, base.3), 0.72),
        SidebarLineTone::Success => mix_linear(base, LinearRgba(0.37, 0.83, 0.47, base.3), 0.68),
        SidebarLineTone::Danger => mix_linear(base, LinearRgba(0.93, 0.36, 0.36, base.3), 0.70),
    }
}

fn mix_linear(base: LinearRgba, target: LinearRgba, factor: f32) -> LinearRgba {
    let t = factor.clamp(0.0, 1.0);
    LinearRgba(
        (base.0 * (1.0 - t) + target.0 * t).clamp(0.0, 1.0),
        (base.1 * (1.0 - t) + target.1 * t).clamp(0.0, 1.0),
        (base.2 * (1.0 - t) + target.2 * t).clamp(0.0, 1.0),
        base.3,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        is_codex_command, is_codex_process_name, is_stale_transient_session_status,
        normalize_transient_session_status, parse_session_status_confidence,
        parse_session_status_source, pick_project_for_cwd, pick_session_for_inferred_binding,
        sidebar_action_needs_pointer_position, sidebar_next_session_title,
        sidebar_status_is_animating, sidebar_status_signal_tag, sidebar_status_spinner_frame,
        SessionEntry, SidebarAction, SidebarLineTone, SidebarSessionStatus,
        WorkspaceSidebarProject, WorkspaceSidebarSession,
    };
    use crate::agent_status::events::{
        SessionStatus, SessionStatusConfidence, SessionStatusSource,
    };
    use chrono::Utc;
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn parses_session_status_aliases() {
        assert_eq!(
            SidebarSessionStatus::parse("idle"),
            SidebarSessionStatus::Idle
        );
        assert_eq!(
            SidebarSessionStatus::parse("need_approve"),
            SidebarSessionStatus::NeedApprove
        );
        assert_eq!(
            SidebarSessionStatus::parse("need-approve"),
            SidebarSessionStatus::NeedApprove
        );
        assert_eq!(
            SidebarSessionStatus::parse("needapprove"),
            SidebarSessionStatus::NeedApprove
        );
        assert_eq!(
            SidebarSessionStatus::parse("unknown-status"),
            SidebarSessionStatus::Unknown
        );
    }

    #[test]
    fn maps_status_to_badge_and_tone() {
        let cases = [
            (SidebarSessionStatus::Idle, "IDLE", SidebarLineTone::Muted),
            (SidebarSessionStatus::Loading, "PREP", SidebarLineTone::Info),
            (
                SidebarSessionStatus::NeedApprove,
                "APPROVAL",
                SidebarLineTone::Warning,
            ),
            (
                SidebarSessionStatus::Running,
                "RUNNING",
                SidebarLineTone::Info,
            ),
            (SidebarSessionStatus::Done, "DONE", SidebarLineTone::Success),
            (
                SidebarSessionStatus::Error,
                "ERROR",
                SidebarLineTone::Danger,
            ),
            (
                SidebarSessionStatus::Unknown,
                "UNKNOWN",
                SidebarLineTone::Muted,
            ),
        ];

        for (status, expected_badge, expected_tone) in cases {
            assert_eq!(status.badge(), expected_badge);
            assert_eq!(status.tone(), expected_tone);
        }
    }

    #[test]
    fn parses_status_source_and_confidence_with_fallbacks() {
        assert_eq!(
            parse_session_status_source("structured"),
            SessionStatusSource::Structured
        );
        assert_eq!(
            parse_session_status_source("heuristic"),
            SessionStatusSource::Heuristic
        );
        assert_eq!(
            parse_session_status_source("unknown"),
            SessionStatusSource::Structured
        );
        assert_eq!(
            parse_session_status_confidence("high"),
            SessionStatusConfidence::High
        );
        assert_eq!(
            parse_session_status_confidence("low"),
            SessionStatusConfidence::Low
        );
        assert_eq!(
            parse_session_status_confidence("unknown"),
            SessionStatusConfidence::Low
        );
    }

    #[test]
    fn maps_source_and_confidence_to_signal_tag() {
        assert_eq!(
            sidebar_status_signal_tag(
                SessionStatusSource::Structured,
                SessionStatusConfidence::High,
            ),
            "S+"
        );
        assert_eq!(
            sidebar_status_signal_tag(
                SessionStatusSource::Structured,
                SessionStatusConfidence::Low,
            ),
            "S?"
        );
        assert_eq!(
            sidebar_status_signal_tag(
                SessionStatusSource::Heuristic,
                SessionStatusConfidence::High,
            ),
            "H+"
        );
        assert_eq!(
            sidebar_status_signal_tag(SessionStatusSource::Heuristic, SessionStatusConfidence::Low,),
            "H?"
        );
    }

    #[test]
    fn running_and_loading_statuses_animate() {
        assert!(sidebar_status_is_animating(SidebarSessionStatus::Loading));
        assert!(!sidebar_status_is_animating(SidebarSessionStatus::Running));
        assert!(!sidebar_status_is_animating(
            SidebarSessionStatus::NeedApprove
        ));
        assert!(!sidebar_status_is_animating(SidebarSessionStatus::Done));
    }

    #[test]
    fn spinner_frame_cycles_in_expected_order() {
        assert_eq!(
            sidebar_status_spinner_frame(std::time::Duration::from_millis(0)),
            '-'
        );
        assert_eq!(
            sidebar_status_spinner_frame(std::time::Duration::from_millis(120)),
            '\\'
        );
        assert_eq!(
            sidebar_status_spinner_frame(std::time::Duration::from_millis(240)),
            '|'
        );
        assert_eq!(
            sidebar_status_spinner_frame(std::time::Duration::from_millis(360)),
            '/'
        );
        assert_eq!(
            sidebar_status_spinner_frame(std::time::Duration::from_millis(480)),
            '-'
        );
    }

    #[test]
    fn next_session_title_skips_existing_suffix() {
        let existing = vec![
            SessionEntry {
                id: "sess_1".to_string(),
                title: "session-1".to_string(),
                status: SessionStatus::Idle.as_storage_str().to_string(),
                codex_thread_id: String::new(),
                status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
                status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
                status_reason: String::new(),
                pinned: false,
                updated_at: String::new(),
            },
            SessionEntry {
                id: "sess_2".to_string(),
                title: "session-2".to_string(),
                status: SessionStatus::Idle.as_storage_str().to_string(),
                codex_thread_id: String::new(),
                status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
                status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
                status_reason: String::new(),
                pinned: false,
                updated_at: String::new(),
            },
        ];

        assert_eq!(sidebar_next_session_title(&existing), "session-3");
    }

    #[test]
    fn open_menu_action_requires_pointer_context() {
        let action = SidebarAction::OpenSessionContextMenu {
            project_id: "proj".to_string(),
            session_id: "sess".to_string(),
        };
        assert!(sidebar_action_needs_pointer_position(&action));
        assert!(sidebar_action_needs_pointer_position(
            &SidebarAction::OpenProjectContextMenu {
                project_id: "proj".to_string()
            }
        ));
        assert!(sidebar_action_needs_pointer_position(
            &SidebarAction::OpenSnippetContextMenu {
                project_id: "proj".to_string(),
                snippet_id: "snip".to_string(),
            }
        ));
        assert!(!sidebar_action_needs_pointer_position(
            &SidebarAction::CreateProject
        ));
    }

    #[test]
    fn picks_longest_matching_project_root_for_cwd() {
        let projects = vec![
            WorkspaceSidebarProject {
                id: "p1".to_string(),
                name: "p1".to_string(),
                root_path: "/Users/mlhiter/personal-projects".to_string(),
                sessions: Vec::new(),
            },
            WorkspaceSidebarProject {
                id: "p2".to_string(),
                name: "p2".to_string(),
                root_path: "/Users/mlhiter/personal-projects/Kaku".to_string(),
                sessions: Vec::new(),
            },
        ];

        let picked = pick_project_for_cwd(
            &projects,
            Path::new("/Users/mlhiter/personal-projects/Kaku/kaku-gui/src"),
        )
        .expect("must pick project");
        assert_eq!(picked.id, "p2");
    }

    #[test]
    fn picks_session_by_tab_title_from_unbound_candidates() {
        let project = WorkspaceSidebarProject {
            id: "proj".to_string(),
            name: "proj".to_string(),
            root_path: "/tmp/proj".to_string(),
            sessions: vec![
                WorkspaceSidebarSession {
                    id: "sess-1".to_string(),
                    title: "session-1".to_string(),
                    status: SessionStatus::Idle.as_storage_str().to_string(),
                    status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
                    status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
                    status_reason: String::new(),
                    pinned: false,
                    updated_at: "2026-04-03T10:00:00Z".to_string(),
                },
                WorkspaceSidebarSession {
                    id: "sess-2".to_string(),
                    title: "session-2".to_string(),
                    status: SessionStatus::Idle.as_storage_str().to_string(),
                    status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
                    status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
                    status_reason: String::new(),
                    pinned: false,
                    updated_at: "2026-04-03T10:01:00Z".to_string(),
                },
            ],
        };

        let mut bound = HashSet::new();
        bound.insert("proj::sess-1".to_string());
        let picked = pick_session_for_inferred_binding(&project, &bound, "session-2")
            .expect("must pick session");
        assert_eq!(picked.id, "sess-2");
    }

    #[test]
    fn falls_back_to_most_recent_when_title_not_matched() {
        let project = WorkspaceSidebarProject {
            id: "proj".to_string(),
            name: "proj".to_string(),
            root_path: "/tmp/proj".to_string(),
            sessions: vec![
                WorkspaceSidebarSession {
                    id: "sess-old".to_string(),
                    title: "old".to_string(),
                    status: SessionStatus::Idle.as_storage_str().to_string(),
                    status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
                    status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
                    status_reason: String::new(),
                    pinned: false,
                    updated_at: "2026-04-03T10:00:00Z".to_string(),
                },
                WorkspaceSidebarSession {
                    id: "sess-new".to_string(),
                    title: "new".to_string(),
                    status: SessionStatus::Idle.as_storage_str().to_string(),
                    status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
                    status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
                    status_reason: String::new(),
                    pinned: false,
                    updated_at: "2026-04-03T10:02:00Z".to_string(),
                },
            ],
        };

        let picked = pick_session_for_inferred_binding(&project, &HashSet::new(), "unknown")
            .expect("must pick session");
        assert_eq!(picked.id, "sess-new");
    }

    #[test]
    fn skips_stale_transient_session_when_inferring_binding() {
        let project = WorkspaceSidebarProject {
            id: "proj".to_string(),
            name: "proj".to_string(),
            root_path: "/tmp/proj".to_string(),
            sessions: vec![
                WorkspaceSidebarSession {
                    id: "sess-stale-running".to_string(),
                    title: "old-running".to_string(),
                    status: SessionStatus::Running.as_storage_str().to_string(),
                    status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
                    status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
                    status_reason: String::new(),
                    pinned: false,
                    updated_at: "2025-01-01T00:00:00Z".to_string(),
                },
                WorkspaceSidebarSession {
                    id: "sess-idle".to_string(),
                    title: "idle".to_string(),
                    status: SessionStatus::Idle.as_storage_str().to_string(),
                    status_source: SessionStatusSource::Heuristic.as_storage_str().to_string(),
                    status_confidence: SessionStatusConfidence::Low.as_storage_str().to_string(),
                    status_reason: String::new(),
                    pinned: false,
                    updated_at: "2026-04-03T10:02:00Z".to_string(),
                },
            ],
        };

        let picked = pick_session_for_inferred_binding(&project, &HashSet::new(), "unknown")
            .expect("must pick session");
        assert_eq!(picked.id, "sess-idle");
    }

    #[test]
    fn normalizes_stale_transient_status_to_idle() {
        let normalized = normalize_transient_session_status(
            SessionStatus::NeedApprove.as_storage_str(),
            "2024-01-01T00:00:00Z",
            Utc::now(),
        );
        assert_eq!(normalized, SessionStatus::Idle.as_storage_str());
    }

    #[test]
    fn preserves_recent_transient_status() {
        let now = Utc::now();
        let updated_recent = (now - chrono::Duration::minutes(5)).to_rfc3339();
        assert!(!is_stale_transient_session_status(
            SessionStatus::Running.as_storage_str(),
            updated_recent.as_str(),
            now,
        ));
    }

    #[test]
    fn detects_codex_context_helpers() {
        assert!(is_codex_command("codex run"));
        assert!(is_codex_command("env A=1 codex run"));
        assert!(!is_codex_command("echo codex"));
        assert!(is_codex_process_name("codex"));
        assert!(is_codex_process_name("/usr/local/bin/codex"));
        assert!(!is_codex_process_name("zsh"));
    }

    #[test]
    fn preserves_structured_high_confidence_transient_status() {
        let now = Utc::now();
        let normalized = normalize_transient_session_status(
            SessionStatus::Running.as_storage_str(),
            now.to_rfc3339().as_str(),
            now,
        );
        assert_eq!(normalized, SessionStatus::Running.as_storage_str());
    }
}
