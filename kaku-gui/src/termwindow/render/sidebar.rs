use crate::quad::TripleLayerQuadAllocator;
use crate::spawn::SpawnWhere;
use crate::termwindow::sidebar_context_menu::{SidebarContextMenuItem, SidebarContextMenuModal};
use crate::termwindow::{
    SidebarAction, TermWindowNotif, UIItem, UIItemType, WorkspaceSidebarActionHit,
    WorkspaceSidebarPendingOpen, WorkspaceSidebarProject, WorkspaceSidebarSession,
};
use anyhow::{anyhow, bail, Context};
use chrono::Utc;
use config::keyassignment::{SpawnCommand, SpawnTabDomain};
use mux::renderable::{RenderableDimensions, StableCursorPosition};
use mux::tab::TabId;
use mux::Mux;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use termwiz::cell::{CellAttributes, Intensity};
use termwiz::surface::Line;
use wezterm_term::color::ColorAttribute;
use window::{color::LinearRgba, WindowOps};

const SIDEBAR_DEFAULT_WIDTH_PX: usize = 320;
const SIDEBAR_MIN_WIDTH_PX: usize = 240;
const SIDEBAR_MAX_WIDTH_PX: usize = 620;
const SIDEBAR_RESIZE_HANDLE_WIDTH_PX: usize = 8;
const SIDEBAR_REFRESH_INTERVAL: Duration = Duration::from_millis(1200);
const SIDEBAR_TEXT_LEFT_PADDING: f32 = 12.0;
const SIDEBAR_TEXT_TOP_PADDING: f32 = 12.0;
const SIDEBAR_UI_STATE_RELATIVE_PATH: &str = "gui/sidebar.json";

#[derive(Debug, Default, Deserialize)]
struct ProjectsFile {
    #[serde(default)]
    projects: Vec<ProjectEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectEntry {
    id: String,
    name: String,
    root_path: String,
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

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SessionEntry {
    id: String,
    title: String,
    #[serde(default)]
    status: String,
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
    #[serde(default)]
    updated_at: String,
}

impl Default for SidebarUiStateFile {
    fn default() -> Self {
        Self {
            version: schema_version(),
            width_px: SIDEBAR_DEFAULT_WIDTH_PX,
            updated_at: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct SidebarLine {
    text: String,
    is_header: bool,
    tone: SidebarLineTone,
    action: Option<SidebarAction>,
}

impl SidebarLine {
    fn new(text: impl Into<String>, is_header: bool) -> Self {
        Self {
            text: text.into(),
            is_header,
            tone: SidebarLineTone::Default,
            action: None,
        }
    }

    fn action(text: impl Into<String>, action: SidebarAction) -> Self {
        Self {
            text: text.into(),
            is_header: false,
            tone: SidebarLineTone::Default,
            action: Some(action),
        }
    }

    fn with_tone(mut self, tone: SidebarLineTone) -> Self {
        self.tone = tone;
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
        match value.trim().to_ascii_lowercase().as_str() {
            "idle" => Self::Idle,
            "loading" => Self::Loading,
            "need_approve" | "need-approve" | "needapprove" => Self::NeedApprove,
            "running" => Self::Running,
            "done" => Self::Done,
            "error" => Self::Error,
            _ => Self::Unknown,
        }
    }

    fn badge(self) -> &'static str {
        match self {
            Self::Idle => "I",
            Self::Loading => "L",
            Self::NeedApprove => "?",
            Self::Running => "R",
            Self::Done => "D",
            Self::Error => "E",
            Self::Unknown => "U",
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
}

pub(crate) fn load_persisted_sidebar_width_px() -> usize {
    let path = sidebar_ui_state_path();
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return SIDEBAR_DEFAULT_WIDTH_PX;
        }
        Err(err) => {
            log::warn!(
                "workspace sidebar: failed to read sidebar ui state {}: {}",
                path.display(),
                err
            );
            return SIDEBAR_DEFAULT_WIDTH_PX;
        }
    };

    match serde_json::from_slice::<SidebarUiStateFile>(&data) {
        Ok(state) => state
            .width_px
            .clamp(SIDEBAR_MIN_WIDTH_PX, SIDEBAR_MAX_WIDTH_PX),
        Err(err) => {
            log::warn!(
                "workspace sidebar: failed to parse sidebar ui state {}: {}",
                path.display(),
                err
            );
            SIDEBAR_DEFAULT_WIDTH_PX
        }
    }
}

impl crate::TermWindow {
    pub(crate) fn sidebar_reserved_width_px(&self) -> usize {
        let width = if self.workspace_sidebar_width_px == 0 {
            SIDEBAR_DEFAULT_WIDTH_PX
        } else {
            self.workspace_sidebar_width_px
        };
        width.clamp(SIDEBAR_MIN_WIDTH_PX, SIDEBAR_MAX_WIDTH_PX)
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

    pub(crate) fn sidebar_resize_from_drag(&mut self, start_x: isize, current_x: isize) -> bool {
        let delta = current_x.saturating_sub(start_x);
        let start_width = self.sidebar_reserved_width_px() as isize;
        let next_width = (start_width + delta)
            .clamp(SIDEBAR_MIN_WIDTH_PX as isize, SIDEBAR_MAX_WIDTH_PX as isize)
            as usize;
        self.sidebar_set_width_px(next_width)
    }

    pub(crate) fn sidebar_persist_width_px(&mut self) {
        let path = sidebar_ui_state_path();
        let state = SidebarUiStateFile {
            version: schema_version(),
            width_px: self.sidebar_reserved_width_px(),
            updated_at: Utc::now().to_rfc3339(),
        };

        if let Err(err) = write_json_file_atomic(&path, &state) {
            log::warn!(
                "workspace sidebar: failed to persist sidebar width {}: {:#}",
                path.display(),
                err
            );
            self.show_toast("Failed to save sidebar width".to_string());
        }
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
            SidebarAction::ActivateSession {
                project_id,
                session_id,
            } => self.sidebar_activate_session(project_id.as_str(), session_id.as_str()),
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
                let y = top.max(0.0) as usize;
                let height = line_height.max(1.0).ceil() as usize;
                self.ui_items.push(UIItem {
                    x: x as usize,
                    y,
                    width: width as usize,
                    height,
                    item_type: UIItemType::SidebarAction(action.clone()),
                });
                self.workspace_sidebar_action_hits
                    .push(WorkspaceSidebarActionHit {
                        x: x as usize,
                        y,
                        width: width as usize,
                        height,
                        action,
                    });
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

        for project in projects_file.projects {
            let sessions_path = root
                .join("projects")
                .join(project.id.as_str())
                .join("sessions.json");

            let mut sessions = if sessions_path.exists() {
                match read_json_file::<SessionsFile>(&sessions_path) {
                    Ok(file) => file
                        .sessions
                        .into_iter()
                        .map(|session| WorkspaceSidebarSession {
                            id: session.id,
                            title: session.title,
                            status: if session.status.is_empty() {
                                "idle".to_string()
                            } else {
                                session.status
                            },
                            pinned: session.pinned,
                            updated_at: session.updated_at,
                        })
                        .collect(),
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

    fn build_workspace_sidebar_lines(&self) -> Vec<SidebarLine> {
        let line_char_budget = self.sidebar_line_char_budget();
        let session_title_budget = line_char_budget.saturating_sub(12).max(18);
        let mut lines = vec![
            SidebarLine::new("WORKSPACE", true),
            SidebarLine::new("", false),
            SidebarLine::new("Projects", true),
        ];

        if self.workspace_sidebar.projects.is_empty() {
            lines.push(SidebarLine::new("  (none)", false).with_tone(SidebarLineTone::Muted));
        }

        for project in &self.workspace_sidebar.projects {
            lines.push(SidebarLine::new(
                format!(
                    "- {} [{}]",
                    project.name,
                    truncate_middle(
                        short_project_path(project.root_path.as_str()).as_str(),
                        line_char_budget
                    )
                ),
                false,
            ));

            if project.sessions.is_empty() {
                lines.push(
                    SidebarLine::new("    (no sessions)", false).with_tone(SidebarLineTone::Muted),
                );
                lines.push(SidebarLine::new("", false));
                continue;
            }

            for session in project.sessions.iter().take(8) {
                let pin = if session.pinned { "*" } else { " " };
                let status = SidebarSessionStatus::parse(session.status.as_str());
                lines.push(
                    SidebarLine::action(
                        format!(
                            "  [{}|{}] {}",
                            pin,
                            status.badge(),
                            truncate_middle(session.title.as_str(), session_title_budget)
                        ),
                        SidebarAction::ActivateSession {
                            project_id: project.id.clone(),
                            session_id: session.id.clone(),
                        },
                    )
                    .with_tone(status.tone()),
                );
            }

            lines.push(SidebarLine::new("", false));
        }

        lines.push(SidebarLine::new("Resources", true));
        lines.push(SidebarLine::new("  Snippets (M3)", false).with_tone(SidebarLineTone::Muted));
        lines.push(
            SidebarLine::new("  Env (M3, plaintext)", false).with_tone(SidebarLineTone::Muted),
        );
        lines.push(SidebarLine::new("  Files (M3)", false).with_tone(SidebarLineTone::Muted));
        lines.push(SidebarLine::new("", false));

        lines.push(SidebarLine::new("Actions", true));
        lines.push(
            SidebarLine::new(
                truncate_middle("  Use `kaku m1 ...` to seed data", line_char_budget),
                false,
            )
            .with_tone(SidebarLineTone::Muted),
        );

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
                            term_window.show_toast("Failed to close sessions".to_string());
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

    fn sidebar_project_display_name(&self, project_id: &str) -> String {
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

fn sidebar_session_key(project_id: &str, session_id: &str) -> String {
    format!("{project_id}::{session_id}")
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

fn read_json_file<T>(path: &Path) -> anyhow::Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("parse {}", path.display()))
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
    use super::{SidebarLineTone, SidebarSessionStatus};

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
            (SidebarSessionStatus::Idle, "I", SidebarLineTone::Muted),
            (SidebarSessionStatus::Loading, "L", SidebarLineTone::Info),
            (
                SidebarSessionStatus::NeedApprove,
                "?",
                SidebarLineTone::Warning,
            ),
            (SidebarSessionStatus::Running, "R", SidebarLineTone::Info),
            (SidebarSessionStatus::Done, "D", SidebarLineTone::Success),
            (SidebarSessionStatus::Error, "E", SidebarLineTone::Danger),
            (SidebarSessionStatus::Unknown, "U", SidebarLineTone::Muted),
        ];

        for (status, expected_badge, expected_tone) in cases {
            assert_eq!(status.badge(), expected_badge);
            assert_eq!(status.tone(), expected_tone);
        }
    }
}
