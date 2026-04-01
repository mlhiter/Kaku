use crate::utils::write_atomic;
use anyhow::{anyhow, bail, Context};
use chrono::Utc;
use clap::{Parser, ValueEnum, ValueHint};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Parser, Clone)]
pub struct M1Command {
    /// Override workspace state root for testing/debugging.
    #[arg(long, value_hint = ValueHint::DirPath, hide = true)]
    state_dir: Option<PathBuf>,

    #[command(subcommand)]
    sub: M1SubCommand,
}

impl M1Command {
    pub fn run(&self) -> anyhow::Result<()> {
        let state_root = self
            .state_dir
            .clone()
            .unwrap_or_else(|| config::HOME_DIR.join(".kaku"));
        let service = WorkspaceService::new(state_root);

        match &self.sub {
            M1SubCommand::CreateProject(cmd) => {
                let project = service.create_project(&cmd.name, &cmd.root_path)?;
                print_json(&project)
            }
            M1SubCommand::ListProjects => {
                let projects = service.list_projects()?;
                print_json(&projects)
            }
            M1SubCommand::CreateSession(cmd) => {
                let session =
                    service.create_session(&cmd.project_id, &cmd.title, cmd.agent_type)?;
                print_json(&session)
            }
            M1SubCommand::ListSessions(cmd) => {
                let sessions = service.list_sessions(&cmd.project_id)?;
                print_json(&sessions)
            }
            M1SubCommand::PinSession(cmd) => {
                let session = service.set_session_pinned(&cmd.project_id, &cmd.session_id, true)?;
                print_json(&session)
            }
            M1SubCommand::UnpinSession(cmd) => {
                let session =
                    service.set_session_pinned(&cmd.project_id, &cmd.session_id, false)?;
                print_json(&session)
            }
        }
    }
}

#[derive(Debug, Parser, Clone)]
enum M1SubCommand {
    #[command(
        name = "create-project",
        about = "Create a project in M1 workspace state"
    )]
    CreateProject(CreateProjectCommand),

    #[command(name = "list-projects", about = "List projects in M1 workspace state")]
    ListProjects,

    #[command(name = "create-session", about = "Create a session under a project")]
    CreateSession(CreateSessionCommand),

    #[command(name = "list-sessions", about = "List sessions under a project")]
    ListSessions(ListSessionsCommand),

    #[command(name = "pin-session", about = "Pin a session")]
    PinSession(PinSessionCommand),

    #[command(name = "unpin-session", about = "Unpin a session")]
    UnpinSession(PinSessionCommand),
}

#[derive(Debug, Parser, Clone)]
struct CreateProjectCommand {
    #[arg(long)]
    name: String,

    #[arg(long, value_hint = ValueHint::DirPath)]
    root_path: PathBuf,
}

#[derive(Debug, Parser, Clone)]
struct CreateSessionCommand {
    #[arg(long)]
    project_id: String,

    #[arg(long)]
    title: String,

    #[arg(long, value_enum, default_value_t = AgentType::Codex)]
    agent_type: AgentType,
}

#[derive(Debug, Parser, Clone)]
struct ListSessionsCommand {
    #[arg(long)]
    project_id: String,
}

#[derive(Debug, Parser, Clone)]
struct PinSessionCommand {
    #[arg(long)]
    project_id: String,

    #[arg(long)]
    session_id: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    Codex,
    Claude,
    Shell,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Loading,
    NeedApprove,
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created_at: String,
    pub last_active_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub agent_type: AgentType,
    pub pinned: bool,
    pub status: SessionStatus,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectsFile {
    #[serde(default = "schema_version")]
    version: u8,
    #[serde(default)]
    projects: Vec<Project>,
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
struct SessionsFile {
    #[serde(default = "schema_version")]
    version: u8,
    #[serde(default)]
    sessions: Vec<Session>,
}

impl Default for SessionsFile {
    fn default() -> Self {
        Self {
            version: schema_version(),
            sessions: Vec::new(),
        }
    }
}

fn schema_version() -> u8 {
    1
}

#[derive(Debug, Clone)]
struct WorkspaceStore {
    root: PathBuf,
}

impl WorkspaceStore {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn projects_file_path(&self) -> PathBuf {
        self.root.join("projects.json")
    }

    fn sessions_file_path(&self, project_id: &str) -> PathBuf {
        self.root
            .join("projects")
            .join(project_id)
            .join("sessions.json")
    }

    fn load_projects(&self) -> anyhow::Result<ProjectsFile> {
        load_json_file(&self.projects_file_path())
    }

    fn save_projects(&self, value: &ProjectsFile) -> anyhow::Result<()> {
        save_json_file(&self.projects_file_path(), value)
    }

    fn load_sessions(&self, project_id: &str) -> anyhow::Result<SessionsFile> {
        load_json_file(&self.sessions_file_path(project_id))
    }

    fn save_sessions(&self, project_id: &str, value: &SessionsFile) -> anyhow::Result<()> {
        save_json_file(&self.sessions_file_path(project_id), value)
    }
}

#[derive(Debug, Clone)]
struct WorkspaceService {
    store: WorkspaceStore,
}

impl WorkspaceService {
    fn new(state_root: PathBuf) -> Self {
        Self {
            store: WorkspaceStore::new(state_root),
        }
    }

    fn create_project(&self, name: &str, root_path: &Path) -> anyhow::Result<Project> {
        let normalized_root = normalize_root_path(root_path)?;
        let mut projects_file = self.store.load_projects()?;

        if projects_file
            .projects
            .iter()
            .any(|project| project.root_path == normalized_root)
        {
            bail!("project already exists for root path {normalized_root}");
        }

        let now = now_rfc3339();
        let project = Project {
            id: generate_id("proj"),
            name: name.to_string(),
            root_path: normalized_root,
            created_at: now.clone(),
            last_active_at: now,
        };
        projects_file.projects.push(project.clone());
        self.store.save_projects(&projects_file)?;
        Ok(project)
    }

    fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        let projects_file = self.store.load_projects()?;
        Ok(projects_file.projects)
    }

    fn create_session(
        &self,
        project_id: &str,
        title: &str,
        agent_type: AgentType,
    ) -> anyhow::Result<Session> {
        let mut projects_file = self.store.load_projects()?;
        let now = now_rfc3339();
        let project = find_project_mut(&mut projects_file.projects, project_id)?;
        project.last_active_at = now.clone();

        let mut sessions_file = self.store.load_sessions(project_id)?;
        let session = Session {
            id: generate_id("sess"),
            project_id: project_id.to_string(),
            title: title.to_string(),
            agent_type,
            pinned: false,
            status: SessionStatus::Idle,
            updated_at: now,
        };
        sessions_file.sessions.push(session.clone());

        self.store.save_sessions(project_id, &sessions_file)?;
        self.store.save_projects(&projects_file)?;
        Ok(session)
    }

    fn list_sessions(&self, project_id: &str) -> anyhow::Result<Vec<Session>> {
        let projects_file = self.store.load_projects()?;
        ensure_project_exists(&projects_file.projects, project_id)?;
        let sessions_file = self.store.load_sessions(project_id)?;
        Ok(sessions_file.sessions)
    }

    fn set_session_pinned(
        &self,
        project_id: &str,
        session_id: &str,
        pinned: bool,
    ) -> anyhow::Result<Session> {
        let mut projects_file = self.store.load_projects()?;
        let now = now_rfc3339();
        let project = find_project_mut(&mut projects_file.projects, project_id)?;
        project.last_active_at = now.clone();

        let mut sessions_file = self.store.load_sessions(project_id)?;
        let session = sessions_file
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;

        session.pinned = pinned;
        session.updated_at = now;

        let updated = session.clone();
        self.store.save_sessions(project_id, &sessions_file)?;
        self.store.save_projects(&projects_file)?;
        Ok(updated)
    }
}

fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let rendered = serde_json::to_string_pretty(value).context("serialize json output")?;
    println!("{rendered}");
    Ok(())
}

fn ensure_project_exists(projects: &[Project], project_id: &str) -> anyhow::Result<()> {
    if projects.iter().any(|project| project.id == project_id) {
        return Ok(());
    }
    bail!("project not found: {project_id}");
}

fn find_project_mut<'a>(
    projects: &'a mut [Project],
    project_id: &str,
) -> anyhow::Result<&'a mut Project> {
    projects
        .iter_mut()
        .find(|project| project.id == project_id)
        .ok_or_else(|| anyhow!("project not found: {project_id}"))
}

fn normalize_root_path(path: &Path) -> anyhow::Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("read current directory")?
            .join(path)
    };
    if !absolute.is_dir() {
        bail!("root path is not a directory: {}", absolute.display());
    }
    Ok(absolute.to_string_lossy().into_owned())
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn generate_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{nanos:x}{seq:x}")
}

fn load_json_file<T>(path: &Path) -> anyhow::Result<T>
where
    T: for<'de> Deserialize<'de> + Default,
{
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };

    match serde_json::from_str::<T>(&content) {
        Ok(value) => Ok(value),
        Err(err) => {
            log::warn!(
                "m1 workspace state parse failed for {}: {:#}. falling back to defaults",
                path.display(),
                err
            );
            Ok(T::default())
        }
    }
}

fn save_json_file<T>(path: &Path, value: &T) -> anyhow::Result<()>
where
    T: Serialize,
{
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("invalid state path: {}", path.display()))?;
    config::create_user_owned_dirs(parent)
        .with_context(|| format!("create state directory {}", parent.display()))?;

    let bytes = serde_json::to_vec_pretty(value).context("serialize state to json")?;
    write_atomic(path, &bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn persists_projects_sessions_and_pin_state() {
        let temp = tempdir().expect("tempdir");
        let state_root = temp.path().join(".kaku");
        let service = WorkspaceService::new(state_root.clone());

        let project = service
            .create_project("demo", temp.path())
            .expect("create project");
        let first = service
            .create_session(&project.id, "first", AgentType::Codex)
            .expect("create first session");
        service
            .create_session(&project.id, "second", AgentType::Claude)
            .expect("create second session");

        service
            .set_session_pinned(&project.id, &first.id, true)
            .expect("pin first session");

        let reloaded = WorkspaceService::new(state_root.clone());
        let projects = reloaded.list_projects().expect("list projects");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, project.id);

        let sessions = reloaded
            .list_sessions(&project.id)
            .expect("list sessions after reload");
        assert_eq!(sessions.len(), 2);
        assert!(
            sessions
                .iter()
                .find(|session| session.id == first.id)
                .expect("find pinned session")
                .pinned
        );

        assert!(state_root.join("projects.json").exists());
        assert!(state_root
            .join("projects")
            .join(&project.id)
            .join("sessions.json")
            .exists());
    }
}
