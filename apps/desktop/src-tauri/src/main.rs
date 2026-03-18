#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod db;
mod embedded_tmux {
    include!(concat!(env!("OUT_DIR"), "/embedded_tmux.rs"));
}
mod layout;
mod models;
mod sessions;

use std::{
    collections::HashMap,
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::anyhow;
use arboard::Clipboard;
use db::{Database, now_iso};
use layout::{
    ClosePaneResult, CloseSessionResult, add_session_to_stack, close_session_in_layout,
    close_stack_node, collect_session_ids, find_stack_id_for_session, first_stack_id,
    new_stack_node, new_workspace_tab, reset_tab_layout, set_active_stack_item, split_stack_node,
    stack_exists, wrap_root_with_ai_workspace,
};
use models::{
    DeleteProjectResult, LaunchProfile, PaneCreatedBy, Project, SessionStatus, SplitZoneKind,
    TerminalSession, WorkspaceChangedEvent, WorkspaceSnapshot, WorkspaceTab,
};
use serde::{Deserialize, Serialize};
use sessions::{SessionCreateOptions, SessionManager, SessionShellKind};
use tauri::{Emitter, Manager, RunEvent, State};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use uuid::Uuid;
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
    UI::Shell::DragQueryFileW,
};

const TMUX_SHIM_FILENAME: &str = if cfg!(windows) { "tmux.exe" } else { "tmux" };

struct AppState {
    db: Arc<Mutex<Database>>,
    sessions: SessionManager,
    workspaces: Arc<Mutex<HashMap<String, WorkspaceSnapshot>>>,
    tab_viewports: Arc<Mutex<HashMap<String, TabViewport>>>,
    tmux: TmuxShimState,
}

#[derive(Clone, Copy, Debug, Default)]
struct TabViewport {
    width: f64,
    height: f64,
}

#[derive(Clone)]
struct TmuxTokenContext {
    session_id: String,
    project_id: String,
    tab_id: String,
    pane_id: String,
    launch_profile: LaunchProfile,
}

#[derive(Clone)]
struct TmuxShimState {
    port: u16,
    fallback_shim_dir: PathBuf,
    app_handle: tauri::AppHandle,
    extracted_shim_dir: Arc<Mutex<Option<PathBuf>>>,
    tokens: Arc<Mutex<HashMap<String, TmuxTokenContext>>>,
}

impl TmuxShimState {
    fn new(port: u16, fallback_shim_dir: PathBuf, app_handle: tauri::AppHandle) -> Self {
        Self {
            port,
            fallback_shim_dir,
            app_handle,
            extracted_shim_dir: Arc::new(Mutex::new(None)),
            tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn register_token(&self, token: String, context: TmuxTokenContext) -> Result<(), String> {
        self.tokens
            .lock()
            .map_err(|_| "Tmux token lock poisoned".to_string())?
            .insert(token, context);
        Ok(())
    }

    fn resolve_token(&self, token: &str) -> Result<Option<TmuxTokenContext>, String> {
        let ctx = self
            .tokens
            .lock()
            .map_err(|_| "Tmux token lock poisoned".to_string())?
            .get(token)
            .cloned();
        Ok(ctx)
    }

    fn remove_session_tokens(&self, session_id: &str) -> Result<(), String> {
        self.tokens
            .lock()
            .map_err(|_| "Tmux token lock poisoned".to_string())?
            .retain(|_, context| context.session_id != session_id);
        Ok(())
    }

    fn resolve_shim_dir(&self) -> Result<PathBuf, String> {
        let cached_dir = self
            .extracted_shim_dir
            .lock()
            .map_err(|_| "Tmux shim path lock poisoned".to_string())?
            .clone();
        if let Some(cached_dir) = cached_dir {
            if cached_dir.join(TMUX_SHIM_FILENAME).is_file() {
                return Ok(cached_dir);
            }
        }

        if let Some(extracted_dir) = self.extract_embedded_shim_dir()? {
            self.extracted_shim_dir
                .lock()
                .map_err(|_| "Tmux shim path lock poisoned".to_string())?
                .replace(extracted_dir.clone());
            return Ok(extracted_dir);
        }

        let fallback_exe = self.fallback_shim_dir.join(TMUX_SHIM_FILENAME);
        if fallback_exe.is_file() {
            return Ok(self.fallback_shim_dir.clone());
        }

        if let Some(adjacent_dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
        {
            let adjacent_exe = adjacent_dir.join(TMUX_SHIM_FILENAME);
            if adjacent_exe.is_file() {
                return Ok(adjacent_dir);
            }
        }

        Err(
            "embedded tmux shim extraction failed: no embedded payload or sibling fallback shim found"
                .to_string(),
        )
    }

    fn extract_embedded_shim_dir(&self) -> Result<Option<PathBuf>, String> {
        let Some(bytes) = embedded_tmux::EMBEDDED_TMUX_BYTES else {
            return Ok(None);
        };

        let cache_key = embedded_tmux::EMBEDDED_TMUX_HASH.unwrap_or(env!("CARGO_PKG_VERSION"));
        let shim_dir = std::env::temp_dir()
            .join("WorkspaceTerminal")
            .join(cache_key);
        let shim_exe_path = shim_dir.join(TMUX_SHIM_FILENAME);
        let needs_write = match fs::metadata(&shim_exe_path) {
            Ok(metadata) => metadata.len() != bytes.len() as u64,
            Err(_) => true,
        };

        if needs_write {
            fs::create_dir_all(&shim_dir)
                .map_err(|err| format!("embedded tmux shim extraction failed: {err}"))?;

            // Write to a temp name first so interrupted extractions do not leave a partial helper behind.
            let temp_path = shim_dir.join(format!("{TMUX_SHIM_FILENAME}.{}.tmp", Uuid::new_v4()));
            fs::write(&temp_path, bytes)
                .map_err(|err| format!("embedded tmux shim extraction failed: {err}"))?;

            if shim_exe_path.is_file() {
                fs::remove_file(&shim_exe_path).map_err(|err| {
                    let _ = fs::remove_file(&temp_path);
                    format!("embedded tmux shim extraction failed: {err}")
                })?;
            }

            if let Err(rename_err) = fs::rename(&temp_path, &shim_exe_path) {
                fs::copy(&temp_path, &shim_exe_path).map_err(|copy_err| {
                    let _ = fs::remove_file(&temp_path);
                    format!(
                        "embedded tmux shim extraction failed: {rename_err}; fallback copy failed: {copy_err}"
                    )
                })?;
                let _ = fs::remove_file(&temp_path);
            }
        }

        Ok(Some(shim_dir))
    }
}

#[tauri::command]
fn list_projects(state: State<'_, AppState>) -> Result<Vec<Project>, String> {
    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    db.list_projects()
}

#[tauri::command]
fn create_project(
    state: State<'_, AppState>,
    name: String,
    path: String,
) -> Result<Project, String> {
    if name.trim().is_empty() {
        return Err("Project name is required".to_string());
    }
    if path.trim().is_empty() || !Path::new(&path).is_dir() {
        return Err("Valid project path is required".to_string());
    }

    let normalized_path = normalize_path(path.trim())?;
    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    db.create_project(name.trim(), &normalized_path)
}

#[tauri::command]
fn rename_project(
    state: State<'_, AppState>,
    project_id: String,
    name: String,
) -> Result<Project, String> {
    if name.trim().is_empty() {
        return Err("Project name is required".to_string());
    }

    let updated = {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.rename_project(&project_id, name.trim())?
    };

    Ok(updated)
}

#[tauri::command]
fn delete_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<DeleteProjectResult, String> {
    let (session_ids, next_project_id) = {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        let projects = db.list_projects()?;
        let index = projects
            .iter()
            .position(|project| project.id == project_id)
            .ok_or_else(|| "Project not found".to_string())?;
        let next_project_id = projects
            .get(index + 1)
            .or_else(|| index.checked_sub(1).and_then(|prev| projects.get(prev)))
            .map(|project| project.id.clone());
        let session_ids = db
            .list_sessions(&project_id)?
            .into_iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        (session_ids, next_project_id)
    };

    for session_id in &session_ids {
        terminate_if_running(&state, session_id)?;
    }

    {
        let mut db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.delete_project(&project_id)?;
    }

    state
        .workspaces
        .lock()
        .map_err(|_| "Workspace cache lock poisoned".to_string())?
        .remove(&project_id);

    Ok(DeleteProjectResult {
        deleted_project_id: project_id,
        next_project_id,
    })
}

#[tauri::command]
fn open_workspace(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    db.touch_project(&project_id)?;
    drop(db);
    load_workspace_snapshot(&state, &project_id)
}

#[tauri::command]
fn create_tab(
    state: State<'_, AppState>,
    project_id: String,
    title: Option<String>,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_snapshot(&state, &project_id)?;
    let tab =
        new_workspace_tab(title.unwrap_or_else(|| format!("tab-{}", snapshot.tabs.len() + 1)));
    snapshot.active_tab_id = Some(tab.id.clone());
    snapshot.tabs.push(tab);
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn close_tab(
    state: State<'_, AppState>,
    project_id: String,
    tab_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let (session_ids, mut snapshot) = {
        let mut session_ids = Vec::new();
        let mut snapshot = load_workspace_snapshot(&state, &project_id)?;
        let Some(tab_index) = snapshot.tabs.iter().position(|tab| tab.id == tab_id) else {
            return Err("Tab not found".to_string());
        };

        collect_session_ids(&snapshot.tabs[tab_index].root, &mut session_ids);
        snapshot.tabs.remove(tab_index);

        if snapshot.tabs.is_empty() {
            let replacement = new_workspace_tab("main".to_string());
            snapshot.active_tab_id = Some(replacement.id.clone());
            snapshot.tabs.push(replacement);
        } else if snapshot.active_tab_id.as_deref() == Some(tab_id.as_str()) {
            snapshot.active_tab_id = Some(snapshot.tabs[tab_index.saturating_sub(1)].id.clone());
        }
        (session_ids, snapshot)
    };

    for session_id in &session_ids {
        terminate_if_running(&state, &session_id)?;
    }

    refresh_snapshot_sessions(&state, &mut snapshot)?;
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn rename_tab(
    state: State<'_, AppState>,
    project_id: String,
    tab_id: String,
    title: String,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_snapshot(&state, &project_id)?;
    if let Some(tab) = snapshot.tabs.iter_mut().find(|tab| tab.id == tab_id) {
        tab.title = title;
    }
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn set_active_tab(
    state: State<'_, AppState>,
    project_id: String,
    tab_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_snapshot(&state, &project_id)?;
    snapshot.active_tab_id = Some(tab_id);
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn split_pane(
    state: State<'_, AppState>,
    project_id: String,
    tab_id: String,
    stack_id: String,
    direction: String,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_snapshot(&state, &project_id)?;

    if let Some(tab) = snapshot.tabs.iter_mut().find(|tab| tab.id == tab_id) {
        let changed = split_stack_node(
            &mut tab.root,
            &stack_id,
            &direction,
            &mut tab.next_pane_ordinal,
            PaneCreatedBy::User,
        );
        if !changed {
            return Err("Target stack not found".to_string());
        }
    }

    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn close_stack_session(
    state: State<'_, AppState>,
    project_id: String,
    tab_id: String,
    stack_id: String,
    session_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_snapshot(&state, &project_id)?;
    close_session_in_snapshot(&mut snapshot, &tab_id, &stack_id, &session_id)?;

    terminate_if_running(&state, &session_id)?;
    refresh_snapshot_sessions(&state, &mut snapshot)?;
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn close_pane(
    state: State<'_, AppState>,
    project_id: String,
    tab_id: String,
    stack_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let (session_ids, mut snapshot) = {
        let mut snapshot = load_workspace_snapshot(&state, &project_id)?;
        let session_ids = close_pane_in_snapshot(&mut snapshot, &tab_id, &stack_id)?;
        (session_ids, snapshot)
    };

    for session_id in &session_ids {
        terminate_if_running(&state, session_id)?;
    }

    refresh_snapshot_sessions(&state, &mut snapshot)?;
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn set_active_stack_item_command(
    state: State<'_, AppState>,
    project_id: String,
    tab_id: String,
    stack_id: String,
    item_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_snapshot(&state, &project_id)?;

    if let Some(tab) = snapshot.tabs.iter_mut().find(|tab| tab.id == tab_id) {
        set_active_stack_item(&mut tab.root, &stack_id, &item_id);
    }

    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn create_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    tab_id: String,
    stack_id: String,
    title: Option<String>,
    program: Option<String>,
    args: Option<Vec<String>>,
    cwd: Option<String>,
    launch_profile: Option<LaunchProfile>,
) -> Result<WorkspaceSnapshot, String> {
    let (snapshot, _session) = spawn_session_in_stack(
        app,
        &state,
        SessionSpawnRequest {
            project_id,
            tab_id,
            stack_id,
            title,
            program,
            args,
            cwd,
            launch_profile: launch_profile.unwrap_or(LaunchProfile::Terminal),
            env_overrides: None,
            shell_kind: SessionShellKind::Default,
        },
    )?;
    Ok(snapshot)
}

struct SessionSpawnRequest {
    project_id: String,
    tab_id: String,
    stack_id: String,
    title: Option<String>,
    program: Option<String>,
    args: Option<Vec<String>>,
    cwd: Option<String>,
    launch_profile: LaunchProfile,
    env_overrides: Option<HashMap<String, String>>,
    shell_kind: SessionShellKind,
}

struct TmuxChildLaunchSpec {
    program: String,
    args: Option<Vec<String>>,
    env_overrides: HashMap<String, String>,
    shell_kind: SessionShellKind,
}

struct GitBashRuntime {
    bash_exe: PathBuf,
    git_bin_dir: PathBuf,
    git_usr_bin_dir: PathBuf,
}

fn spawn_session_in_stack(
    app: tauri::AppHandle,
    state: &AppState,
    request: SessionSpawnRequest,
) -> Result<(WorkspaceSnapshot, TerminalSession), String> {
    let program = request.program.unwrap_or_else(|| "powershell".to_string());
    let session_title = request.title.unwrap_or_else(|| program.clone());

    let project = {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.get_project(&request.project_id)?
            .ok_or_else(|| "Project not found".to_string())?
    };

    let mut snapshot = {
        let snapshot = load_workspace_snapshot(state, &request.project_id)?;
        let Some(tab) = snapshot.tabs.iter().find(|tab| tab.id == request.tab_id) else {
            return Err("Tab not found".to_string());
        };
        if !stack_exists(&tab.root, &request.stack_id) {
            return Err("Target stack not found".to_string());
        }
        snapshot
    };

    let session_cwd = match request.cwd {
        Some(custom_cwd) => normalize_path(&custom_cwd)?,
        None => runtime_path(&project.path),
    };
    if !Path::new(&session_cwd).is_dir() {
        return Err("Working directory does not exist".to_string());
    }

    let mut session = TerminalSession {
        id: Uuid::new_v4().to_string(),
        project_id: request.project_id.clone(),
        title: session_title,
        program,
        args: request.args,
        launch_profile: request.launch_profile.clone(),
        tmux_shim_enabled: request.launch_profile != LaunchProfile::Terminal,
        cwd: session_cwd,
        status: SessionStatus::Starting,
        started_at: Some(now_iso()),
        ended_at: None,
        exit_code: None,
    };

    let (tmux_token, tmux_env) =
        build_tmux_runtime_env(state, &session, &request.tab_id, &request.stack_id)?;
    let extra_env = merge_env_maps(tmux_env, request.env_overrides);

    session = state.sessions.create(
        app,
        state.db.clone(),
        session,
        SessionCreateOptions {
            extra_env,
            shell_kind: request.shell_kind,
        },
    )?;
    if let Some(tab) = snapshot
        .tabs
        .iter_mut()
        .find(|tab| tab.id == request.tab_id)
    {
        add_session_to_stack(
            &mut tab.root,
            &request.stack_id,
            &session.id,
            &session.title,
        );
    }

    if let Some(token) = tmux_token {
        state.tmux.register_token(
            token,
            TmuxTokenContext {
                session_id: session.id.clone(),
                project_id: request.project_id.clone(),
                tab_id: request.tab_id.clone(),
                pane_id: request.stack_id.clone(),
                launch_profile: request.launch_profile,
            },
        )?;
    }

    refresh_snapshot_sessions(state, &mut snapshot)?;
    if let Err(err) = persist_workspace_snapshot(state, snapshot.clone()) {
        let _ = terminate_if_running(state, &session.id);
        return Err(err);
    }

    Ok((snapshot, session))
}

fn build_tmux_runtime_env(
    state: &AppState,
    session: &TerminalSession,
    tab_id: &str,
    pane_id: &str,
) -> Result<(Option<String>, Option<HashMap<String, String>>), String> {
    if !session.tmux_shim_enabled {
        return Ok((None, None));
    }

    let token = Uuid::new_v4().to_string();
    let mut env = HashMap::new();
    env.insert(
        "TMUX".to_string(),
        format!("workspace-terminal-shim,{},{}", state.tmux.port, pane_id),
    );
    env.insert(
        "WORKSPACE_TERMINAL_TMUX_URL".to_string(),
        format!("http://127.0.0.1:{}", state.tmux.port),
    );
    env.insert("WORKSPACE_TERMINAL_TMUX_TOKEN".to_string(), token.clone());
    env.insert(
        "WORKSPACE_TERMINAL_PROJECT_ID".to_string(),
        session.project_id.clone(),
    );
    env.insert("WORKSPACE_TERMINAL_TAB_ID".to_string(), tab_id.to_string());
    env.insert(
        "WORKSPACE_TERMINAL_PANE_ID".to_string(),
        pane_id.to_string(),
    );
    env.insert(
        "WORKSPACE_TERMINAL_SESSION_ID".to_string(),
        session.id.clone(),
    );

    let shim_dir = state.tmux.resolve_shim_dir()?.to_string_lossy().to_string();
    if shim_dir.is_empty() {
        return Err("Tmux shim directory is not available".to_string());
    }
    let path_value = if let Ok(existing_path) = std::env::var("PATH") {
        format!("{shim_dir};{existing_path}")
    } else {
        shim_dir
    };
    env.insert("PATH".to_string(), path_value);

    Ok((Some(token), Some(env)))
}

fn merge_env_maps(
    base: Option<HashMap<String, String>>,
    overrides: Option<HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    match (base, overrides) {
        (None, None) => None,
        (Some(base), None) => Some(base),
        (None, Some(overrides)) => Some(overrides),
        (Some(mut base), Some(overrides)) => {
            for (key, value) in overrides {
                base.insert(key, value);
            }
            Some(base)
        }
    }
}

fn build_tmux_child_launch_spec(
    state: &AppState,
    command: Option<String>,
) -> Result<TmuxChildLaunchSpec, String> {
    if cfg!(windows) {
        let git_bash = resolve_git_bash().ok_or_else(build_missing_git_bash_error)?;
        let shim_dir = state.tmux.resolve_shim_dir()?;
        let path_value = build_git_bash_path_value(&git_bash, &shim_dir);
        let bash_exe = git_bash.bash_exe.to_string_lossy().to_string();

        let mut env_overrides = HashMap::new();
        env_overrides.insert("PATH".to_string(), path_value);
        env_overrides.insert("SHELL".to_string(), slashify_path(&git_bash.bash_exe));
        env_overrides.insert("CHERE_INVOKING".to_string(), "1".to_string());
        env_overrides.insert("MSYSTEM".to_string(), "MINGW64".to_string());
        env_overrides.insert("TERM".to_string(), "xterm-256color".to_string());

        let shell_kind = if command.is_some() {
            SessionShellKind::TmuxGitBashCommand
        } else {
            SessionShellKind::TmuxGitBashInteractive
        };

        return Ok(TmuxChildLaunchSpec {
            program: bash_exe,
            args: command.map(|command| vec!["-lc".to_string(), command]),
            env_overrides,
            shell_kind,
        });
    }

    Ok(TmuxChildLaunchSpec {
        program: default_tmux_shell_program(),
        args: command.map(|command| vec!["-lc".to_string(), command]),
        env_overrides: HashMap::new(),
        shell_kind: SessionShellKind::Default,
    })
}

fn resolve_git_bash() -> Option<GitBashRuntime> {
    git_bash_candidates().into_iter().find_map(|bash_exe| {
        if !bash_exe.is_file() {
            return None;
        }

        let git_bin_dir = bash_exe.parent()?.to_path_buf();
        let git_root_dir = git_bin_dir.parent()?.to_path_buf();
        let git_usr_bin_dir = git_root_dir.join("usr").join("bin");
        if !git_usr_bin_dir.is_dir() {
            return None;
        }

        Some(GitBashRuntime {
            bash_exe,
            git_bin_dir,
            git_usr_bin_dir,
        })
    })
}

fn git_bash_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("ProgramFiles") {
        candidates.push(
            PathBuf::from(&path)
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        );
    }
    if let Some(path) = std::env::var_os("ProgramFiles(x86)") {
        candidates.push(
            PathBuf::from(&path)
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        );
    }
    if let Some(path) = std::env::var_os("LocalAppData") {
        candidates.push(
            PathBuf::from(&path)
                .join("Programs")
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        );
    }
    candidates.push(PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"));
    candidates.push(PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe"));
    candidates
}

fn build_git_bash_path_value(git_bash: &GitBashRuntime, shim_dir: &Path) -> String {
    let mut segments = vec![
        git_bash.git_bin_dir.to_string_lossy().to_string(),
        git_bash.git_usr_bin_dir.to_string_lossy().to_string(),
        shim_dir.to_string_lossy().to_string(),
    ];

    if let Ok(existing_path) = std::env::var("PATH") {
        segments.push(existing_path);
    }

    segments.join(";")
}

fn build_missing_git_bash_error() -> String {
    let wsl_available = has_usable_wsl_bash();
    if wsl_available {
        "Git Bash was not found. Windows tmux harness requires Git Bash because the harness sends POSIX shell commands with Windows CLI paths.".to_string()
    } else {
        "Git Bash was not found and WSL bash is not available. Windows tmux harness requires a POSIX shell for Claude Teams child panes.".to_string()
    }
}

fn has_usable_wsl_bash() -> bool {
    if !cfg!(windows) {
        return false;
    }

    std::process::Command::new("wsl.exe")
        .args(["-e", "bash", "-lc", "echo wsl-ok"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn slashify_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[tauri::command]
fn write_session_input(
    state: State<'_, AppState>,
    session_id: String,
    input: String,
) -> Result<(), String> {
    state.sessions.write_input(&session_id, &input)
}

#[tauri::command]
fn resize_session(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state.sessions.resize(&session_id, cols, rows)
}

#[tauri::command]
fn terminate_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let _ = state.tmux.remove_session_tokens(&session_id);
    state.sessions.terminate(&session_id)
}

#[tauri::command]
fn report_tab_viewport(
    state: State<'_, AppState>,
    project_id: String,
    tab_id: String,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if width <= 0.0 || height <= 0.0 {
        return Ok(());
    }

    state
        .tab_viewports
        .lock()
        .map_err(|_| "Viewport cache lock poisoned".to_string())?
        .insert(
            tab_viewport_key(&project_id, &tab_id),
            TabViewport { width, height },
        );
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum PastePayload {
    Files { paths: Vec<String> },
    ImagePath { image_path: String },
    Text { text: String },
    Empty,
}

#[tauri::command]
fn read_clipboard_payload() -> Result<PastePayload, String> {
    #[cfg(windows)]
    if let Some(paths) = read_windows_clipboard_file_paths()? {
        if !paths.is_empty() {
            return Ok(PastePayload::Files { paths });
        }
    }

    let mut clipboard = Clipboard::new().map_err(|err| err.to_string())?;

    if let Ok(image) = clipboard.get_image() {
        let image_path = save_clipboard_image_to_temp(
            image.width as u32,
            image.height as u32,
            image.bytes.as_ref(),
        )?;
        return Ok(PastePayload::ImagePath { image_path });
    }

    if let Ok(text) = clipboard.get_text() {
        if !text.is_empty() {
            return Ok(PastePayload::Text { text });
        }
    }

    Ok(PastePayload::Empty)
}

fn main() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .map_err(|err| tauri::Error::Anyhow(anyhow!(err.to_string())))?;
            let db = Database::new(app_dir).map_err(|err| tauri::Error::Anyhow(anyhow!(err)))?;
            db.mark_stale_sessions()
                .map_err(|err| tauri::Error::Anyhow(anyhow!(err)))?;

            let db = Arc::new(Mutex::new(db));
            let sessions = SessionManager::default();
            let workspaces = Arc::new(Mutex::new(HashMap::new()));
            let tab_viewports = Arc::new(Mutex::new(HashMap::new()));
            let tmux = start_tmux_server(
                db.clone(),
                sessions.clone(),
                workspaces.clone(),
                tab_viewports.clone(),
                app.handle().clone(),
            )
            .map_err(|err| tauri::Error::Anyhow(anyhow!(err)))?;

            app.manage(AppState {
                db,
                sessions,
                workspaces,
                tab_viewports,
                tmux,
            });
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            list_projects,
            create_project,
            rename_project,
            delete_project,
            open_workspace,
            create_tab,
            close_tab,
            rename_tab,
            set_active_tab,
            split_pane,
            close_stack_session,
            close_pane,
            create_session,
            write_session_input,
            resize_session,
            report_tab_viewport,
            read_clipboard_payload,
            terminate_session,
            set_active_stack_item_command
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
            if let Some(state) = app_handle.try_state::<AppState>() {
                let _ = state.sessions.terminate_all();
            }
        }
    });
}

fn normalize_path(path: &str) -> Result<String, String> {
    let canonical = fs::canonicalize(path).map_err(|err| err.to_string())?;
    let normalized = strip_windows_verbatim_prefix(&canonical.to_string_lossy().replace('/', "\\"));
    if cfg!(windows) {
        Ok(normalized.to_lowercase())
    } else {
        Ok(normalized)
    }
}

fn runtime_path(path: &str) -> String {
    strip_windows_verbatim_prefix(path)
}

fn strip_windows_verbatim_prefix(path: &str) -> String {
    if cfg!(windows) {
        path.strip_prefix("\\\\?\\")
            .or_else(|| path.strip_prefix("//?/"))
            .unwrap_or(path)
            .to_string()
    } else {
        path.to_string()
    }
}

fn terminate_if_running(state: &AppState, session_id: &str) -> Result<(), String> {
    let _ = state.tmux.remove_session_tokens(session_id);
    match state.sessions.terminate(session_id) {
        Ok(()) => Ok(()),
        Err(err) if err == "Session not found" => Ok(()),
        Err(err) => Err(err),
    }
}

pub(crate) fn handle_runtime_session_exit(
    state: &AppState,
    session_id: &str,
) -> Result<(), String> {
    let _ = state.tmux.remove_session_tokens(session_id);

    let mut changed_snapshots = {
        let mut workspaces = state
            .workspaces
            .lock()
            .map_err(|_| "Workspace cache lock poisoned".to_string())?;
        let mut changed = Vec::new();

        for snapshot in workspaces.values_mut() {
            if remove_session_from_snapshot(snapshot, session_id) {
                changed.push(snapshot.clone());
            }
        }

        changed
    };

    for snapshot in changed_snapshots.iter_mut() {
        refresh_snapshot_sessions(state, snapshot)?;
        let _ = persist_workspace_snapshot(state, snapshot.clone())?;
        emit_workspace_changed(&state.tmux.app_handle, &snapshot.project_id);
    }

    Ok(())
}

fn remove_session_from_snapshot(snapshot: &mut WorkspaceSnapshot, session_id: &str) -> bool {
    for tab_index in 0..snapshot.tabs.len() {
        let stack_id = snapshot
            .tabs
            .get(tab_index)
            .and_then(|tab| find_stack_id_for_session(&tab.root, session_id));
        let Some(stack_id) = stack_id else {
            continue;
        };

        let close_result = {
            let tab = &mut snapshot.tabs[tab_index];
            close_session_in_layout(&mut tab.root, &stack_id, session_id)
        };

        match close_result {
            CloseSessionResult::NotFound => continue,
            CloseSessionResult::Updated => {
                if let Some(tab) = snapshot.tabs.get_mut(tab_index) {
                    ensure_valid_active_pane(tab);
                }
            }
            CloseSessionResult::RootRemoved => {
                close_tab_root_or_reset(snapshot, tab_index);
            }
        }

        ensure_valid_active_tab(snapshot);
        return true;
    }

    false
}

fn close_session_in_snapshot(
    snapshot: &mut WorkspaceSnapshot,
    tab_id: &str,
    stack_id: &str,
    session_id: &str,
) -> Result<(), String> {
    let tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == tab_id)
        .ok_or_else(|| "Tab not found".to_string())?;

    let close_result = {
        let tab = &mut snapshot.tabs[tab_index];
        close_session_in_layout(&mut tab.root, stack_id, session_id)
    };

    match close_result {
        CloseSessionResult::NotFound => Err("Session not found in stack".to_string()),
        CloseSessionResult::Updated => {
            if let Some(tab) = snapshot.tabs.get_mut(tab_index) {
                ensure_valid_active_pane(tab);
            }
            ensure_valid_active_tab(snapshot);
            Ok(())
        }
        CloseSessionResult::RootRemoved => {
            close_tab_root_or_reset(snapshot, tab_index);
            ensure_valid_active_tab(snapshot);
            Ok(())
        }
    }
}

fn close_pane_in_snapshot(
    snapshot: &mut WorkspaceSnapshot,
    tab_id: &str,
    stack_id: &str,
) -> Result<Vec<String>, String> {
    let tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == tab_id)
        .ok_or_else(|| "Tab not found".to_string())?;

    let close_result = {
        let tab = &mut snapshot.tabs[tab_index];
        close_stack_node(&mut tab.root, stack_id)
    };

    match close_result {
        ClosePaneResult::NotFound => Err("Pane not found".to_string()),
        ClosePaneResult::Updated(session_ids) => {
            if let Some(tab) = snapshot.tabs.get_mut(tab_index) {
                ensure_valid_active_pane(tab);
            }
            ensure_valid_active_tab(snapshot);
            Ok(session_ids)
        }
        ClosePaneResult::RootRemoved(session_ids) => {
            close_tab_root_or_reset(snapshot, tab_index);
            ensure_valid_active_tab(snapshot);
            Ok(session_ids)
        }
    }
}

fn close_tab_root_or_reset(snapshot: &mut WorkspaceSnapshot, tab_index: usize) {
    if snapshot.tabs.len() > 1 {
        let removed_tab_id = snapshot.tabs[tab_index].id.clone();
        snapshot.tabs.remove(tab_index);
        if snapshot.active_tab_id.as_deref() == Some(removed_tab_id.as_str()) {
            snapshot.active_tab_id = snapshot
                .tabs
                .get(tab_index.saturating_sub(1))
                .or_else(|| snapshot.tabs.get(tab_index))
                .map(|tab| tab.id.clone());
        }
        return;
    }

    if let Some(tab) = snapshot.tabs.get_mut(tab_index) {
        reset_tab_layout(tab);
        snapshot.active_tab_id = Some(tab.id.clone());
    }
}

fn ensure_valid_active_pane(tab: &mut WorkspaceTab) {
    if !tab
        .active_pane_id
        .as_ref()
        .is_some_and(|pane_id| stack_exists(&tab.root, pane_id))
    {
        tab.active_pane_id = first_stack_id(&tab.root);
    }
}

fn ensure_valid_active_tab(snapshot: &mut WorkspaceSnapshot) {
    if !snapshot
        .active_tab_id
        .as_ref()
        .is_some_and(|tab_id| snapshot.tabs.iter().any(|tab| tab.id == *tab_id))
    {
        snapshot.active_tab_id = snapshot.tabs.first().map(|tab| tab.id.clone());
    }
}

fn emit_workspace_changed(app_handle: &tauri::AppHandle, project_id: &str) {
    let _ = app_handle.emit(
        "workspace-changed",
        WorkspaceChangedEvent {
            project_id: project_id.to_string(),
        },
    );
}

#[derive(Debug)]
struct TmuxHttpError {
    status: u16,
    message: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SplitWindowRequest {
    direction: Option<String>,
    command: Option<String>,
    cwd: Option<String>,
    name: Option<String>,
    target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct NewWindowRequest {
    command: Option<String>,
    cwd: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SendKeysRequest {
    target: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct KillPaneRequest {
    target: Option<String>,
}

#[derive(Debug, Clone)]
struct PaneListing {
    pane_id: String,
    pane_index: usize,
    pane_title: String,
    pane_current_command: String,
    window_index: usize,
    session_name: String,
    window_id: String,
}

fn start_tmux_server(
    db: Arc<Mutex<Database>>,
    sessions: SessionManager,
    workspaces: Arc<Mutex<HashMap<String, WorkspaceSnapshot>>>,
    tab_viewports: Arc<Mutex<HashMap<String, TabViewport>>>,
    app_handle: tauri::AppHandle,
) -> Result<TmuxShimState, String> {
    let fallback_shim_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("shim")))
        .unwrap_or_else(|| PathBuf::from("shim"));

    let server = Server::http("127.0.0.1:0").map_err(|err| err.to_string())?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|addr| addr.port())
        .ok_or_else(|| "Failed to read tmux shim server port".to_string())?;
    let tmux = TmuxShimState::new(port, fallback_shim_dir, app_handle.clone());

    let runtime_state = AppState {
        db,
        sessions,
        workspaces,
        tab_viewports,
        tmux: tmux.clone(),
    };

    thread::spawn(move || {
        for request in server.incoming_requests() {
            handle_tmux_http_request(request, &runtime_state);
        }
    });

    Ok(tmux)
}

fn handle_tmux_http_request(mut request: Request, state: &AppState) {
    let response = match dispatch_tmux_http_request(&mut request, state) {
        Ok((status, body, content_type)) => build_http_response(status, body, content_type),
        Err(err) => build_http_response(err.status, err.message, "text/plain; charset=utf-8"),
    };
    let _ = request.respond(response);
}

fn dispatch_tmux_http_request(
    request: &mut Request,
    state: &AppState,
) -> Result<(u16, String, &'static str), TmuxHttpError> {
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let context = authenticate_tmux(request, state)?;

    match (request.method(), path) {
        (&Method::Get, "/v1/tmux/has-session") => {
            if ensure_tmux_context_alive(state, &context)? {
                Ok((200, "{\"ok\":true}".to_string(), "application/json"))
            } else {
                Err(TmuxHttpError {
                    status: 401,
                    message: "Session is no longer active".to_string(),
                })
            }
        }
        (&Method::Post, "/v1/tmux/split-window") => {
            let payload: SplitWindowRequest = parse_json_body(request)?;
            let result = tmux_split_window(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/new-window") => {
            let payload: NewWindowRequest = parse_json_body(request)?;
            let result = tmux_new_window(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/send-keys") => {
            let payload: SendKeysRequest = parse_json_body(request)?;
            tmux_send_keys(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Get, "/v1/tmux/list-panes") => {
            let format =
                get_query_value(query, "format").unwrap_or_else(|| "#{pane_id}".to_string());
            let lines = tmux_list_panes(state, &context, &format)?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Post, "/v1/tmux/kill-pane") => {
            let payload: KillPaneRequest = parse_json_body(request)?;
            tmux_kill_pane(state, &context, payload.target.as_deref())?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Get, "/v1/tmux/display-message") => {
            let format =
                get_query_value(query, "format").unwrap_or_else(|| "#{pane_id}".to_string());
            let text = tmux_display_message(state, &context, &format)?;
            Ok((200, text, "text/plain; charset=utf-8"))
        }
        _ => Err(TmuxHttpError {
            status: 404,
            message: "tmux endpoint not found".to_string(),
        }),
    }
}

fn parse_json_body<T: for<'de> Deserialize<'de>>(
    request: &mut Request,
) -> Result<T, TmuxHttpError> {
    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .map_err(|err| TmuxHttpError {
            status: 400,
            message: format!("Failed to read request body: {err}"),
        })?;

    let body = if body.trim().is_empty() {
        "{}".to_string()
    } else {
        body
    };

    serde_json::from_str::<T>(&body).map_err(|err| TmuxHttpError {
        status: 400,
        message: format!("Invalid JSON payload: {err}"),
    })
}

fn authenticate_tmux(
    request: &Request,
    state: &AppState,
) -> Result<TmuxTokenContext, TmuxHttpError> {
    let token = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .and_then(|header| header.value.as_str().strip_prefix("Bearer "))
        .map(|token| token.trim().to_string())
        .ok_or_else(|| TmuxHttpError {
            status: 401,
            message: "Missing Bearer token".to_string(),
        })?;

    let context = state
        .tmux
        .resolve_token(&token)
        .map_err(internal_error)?
        .ok_or_else(|| TmuxHttpError {
            status: 401,
            message: "Invalid tmux token".to_string(),
        })?;

    Ok(context)
}

fn ensure_tmux_context_alive(
    state: &AppState,
    context: &TmuxTokenContext,
) -> Result<bool, TmuxHttpError> {
    let snapshot = load_workspace_snapshot(state, &context.project_id).map_err(internal_error)?;
    let session_alive = snapshot.sessions.iter().any(|session| {
        session.id == context.session_id
            && matches!(
                session.status,
                SessionStatus::Running | SessionStatus::Starting
            )
    });
    if !session_alive {
        return Ok(false);
    }

    let tab = snapshot.tabs.iter().find(|tab| tab.id == context.tab_id);
    let Some(tab) = tab else {
        return Ok(false);
    };

    Ok(stack_exists(&tab.root, &context.pane_id))
}

fn tmux_split_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SplitWindowRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    if !ensure_tmux_context_alive(state, context)? {
        return Err(TmuxHttpError {
            status: 401,
            message: "Session is no longer active".to_string(),
        });
    }

    let mut snapshot =
        load_workspace_snapshot(state, &context.project_id).map_err(internal_error)?;
    let Some(tab) = snapshot
        .tabs
        .iter_mut()
        .find(|tab| tab.id == context.tab_id)
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Caller tab not found".to_string(),
        });
    };
    let explicit_target_pane_id = payload
        .target
        .as_deref()
        .map(|target| resolve_target_pane_id(tab, Some(target), &context.pane_id))
        .transpose()?;
    let source_pane_id = explicit_target_pane_id
        .clone()
        .unwrap_or_else(|| context.pane_id.clone());
    let (new_pane_id, direction) = split_into_ai_workspace(
        state,
        &context.project_id,
        tab,
        &context.pane_id,
        explicit_target_pane_id.as_deref(),
        payload.direction.as_deref(),
        source_pane_id.clone(),
    )?;

    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;

    let child_launch =
        build_tmux_child_launch_spec(state, payload.command.clone()).map_err(internal_error)?;
    let (_snapshot, session) = spawn_session_in_stack(
        state.tmux.app_handle.clone(),
        state,
        SessionSpawnRequest {
            project_id: context.project_id.clone(),
            tab_id: context.tab_id.clone(),
            stack_id: new_pane_id.clone(),
            title: payload.name.clone().or_else(|| {
                Some(if payload.command.is_some() {
                    "tmux-command".to_string()
                } else {
                    "AI Terminal".to_string()
                })
            }),
            program: Some(child_launch.program),
            args: child_launch.args,
            cwd: payload.cwd,
            launch_profile: context.launch_profile.clone(),
            env_overrides: Some(child_launch.env_overrides),
            shell_kind: child_launch.shell_kind,
        },
    )
    .map_err(internal_error)?;

    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);

    Ok(serde_json::json!({
        "paneId": new_pane_id,
        "sessionId": session.id,
        "direction": direction
    }))
}

fn tmux_new_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: NewWindowRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    if !ensure_tmux_context_alive(state, context)? {
        return Err(TmuxHttpError {
            status: 401,
            message: "Session is no longer active".to_string(),
        });
    }

    let mut snapshot =
        load_workspace_snapshot(state, &context.project_id).map_err(internal_error)?;
    let mut tab = new_workspace_tab(
        payload
            .name
            .clone()
            .unwrap_or_else(|| format!("tmux-{}", now_iso())),
    );
    tab.root = new_stack_node(1, PaneCreatedBy::Ai, Some(context.pane_id.clone()));
    tab.next_pane_ordinal = 2;
    tab.active_pane_id = first_stack_id(&tab.root);
    let new_tab_id = tab.id.clone();
    let new_pane_id = tab
        .active_pane_id
        .clone()
        .unwrap_or_else(|| context.pane_id.clone());

    snapshot.tabs.push(tab);
    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;

    let child_launch =
        build_tmux_child_launch_spec(state, payload.command.clone()).map_err(internal_error)?;
    let (_snapshot, session) = spawn_session_in_stack(
        state.tmux.app_handle.clone(),
        state,
        SessionSpawnRequest {
            project_id: context.project_id.clone(),
            tab_id: new_tab_id.clone(),
            stack_id: new_pane_id.clone(),
            title: payload.name.clone().or_else(|| {
                Some(if payload.command.is_some() {
                    "tmux-window".to_string()
                } else {
                    "AI Terminal".to_string()
                })
            }),
            program: Some(child_launch.program),
            args: child_launch.args,
            cwd: payload.cwd,
            launch_profile: context.launch_profile.clone(),
            env_overrides: Some(child_launch.env_overrides),
            shell_kind: child_launch.shell_kind,
        },
    )
    .map_err(internal_error)?;

    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);

    Ok(serde_json::json!({
        "tabId": new_tab_id,
        "paneId": new_pane_id,
        "sessionId": session.id
    }))
}

fn tmux_send_keys(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SendKeysRequest,
) -> Result<(), TmuxHttpError> {
    if !ensure_tmux_context_alive(state, context)? {
        return Err(TmuxHttpError {
            status: 401,
            message: "Session is no longer active".to_string(),
        });
    }
    let snapshot = load_workspace_snapshot(state, &context.project_id).map_err(internal_error)?;
    let Some(tab) = snapshot.tabs.iter().find(|tab| tab.id == context.tab_id) else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Caller tab not found".to_string(),
        });
    };
    let target_pane = resolve_target_pane_id(tab, payload.target.as_deref(), &context.pane_id)?;
    let Some(target_session_id) = active_session_for_pane(&tab.root, &target_pane) else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Target pane session not found".to_string(),
        });
    };
    let text = payload.text.unwrap_or_default();
    state
        .sessions
        .ensure_tmux_child_ready(&target_session_id)
        .map_err(internal_error)?;
    state
        .sessions
        .write_input(&target_session_id, &text)
        .map_err(internal_error)?;
    Ok(())
}

fn tmux_list_panes(
    state: &AppState,
    context: &TmuxTokenContext,
    format: &str,
) -> Result<Vec<String>, TmuxHttpError> {
    if !ensure_tmux_context_alive(state, context)? {
        return Err(TmuxHttpError {
            status: 401,
            message: "Session is no longer active".to_string(),
        });
    }
    let snapshot = load_workspace_snapshot(state, &context.project_id).map_err(internal_error)?;
    let tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == context.tab_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Caller tab not found".to_string(),
        })?;
    let tab = &snapshot.tabs[tab_index];

    let mut panes = Vec::new();
    collect_tab_panes(
        &tab.root,
        &snapshot.sessions,
        tab_index,
        &context.tab_id,
        &context.session_id,
        &mut panes,
    );
    let lines = panes
        .iter()
        .map(|pane| render_tmux_format(format, pane))
        .collect::<Vec<_>>();
    Ok(lines)
}

fn tmux_kill_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    target: Option<&str>,
) -> Result<(), TmuxHttpError> {
    if !ensure_tmux_context_alive(state, context)? {
        return Err(TmuxHttpError {
            status: 401,
            message: "Session is no longer active".to_string(),
        });
    }
    let (session_ids, mut snapshot) = {
        let mut snapshot =
            load_workspace_snapshot(state, &context.project_id).map_err(internal_error)?;
        let Some(tab) = snapshot.tabs.iter().find(|tab| tab.id == context.tab_id) else {
            return Err(TmuxHttpError {
                status: 404,
                message: "Caller tab not found".to_string(),
            });
        };
        let target_stack_id = resolve_target_pane_id(tab, target, &context.pane_id)?;
        let session_ids = close_pane_in_snapshot(&mut snapshot, &context.tab_id, &target_stack_id)
            .map_err(internal_error)?;
        (session_ids, snapshot)
    };

    for session_id in &session_ids {
        terminate_if_running(state, session_id).map_err(internal_error)?;
    }
    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);
    Ok(())
}

fn tmux_display_message(
    state: &AppState,
    context: &TmuxTokenContext,
    format: &str,
) -> Result<String, TmuxHttpError> {
    if !ensure_tmux_context_alive(state, context)? {
        return Err(TmuxHttpError {
            status: 401,
            message: "Session is no longer active".to_string(),
        });
    }
    let snapshot = load_workspace_snapshot(state, &context.project_id).map_err(internal_error)?;
    let tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == context.tab_id)
        .unwrap_or(0);
    let mut panes = Vec::new();
    if let Some(tab) = snapshot.tabs.get(tab_index) {
        collect_tab_panes(
            &tab.root,
            &snapshot.sessions,
            tab_index,
            &context.tab_id,
            &context.session_id,
            &mut panes,
        );
    }
    let pane = panes
        .into_iter()
        .find(|pane| pane.pane_id == context.pane_id)
        .unwrap_or(PaneListing {
            pane_id: context.pane_id.clone(),
            pane_index: 0,
            pane_title: "pane".to_string(),
            pane_current_command: "terminal".to_string(),
            window_index: tab_index,
            session_name: context.session_id.clone(),
            window_id: context.tab_id.clone(),
        });
    Ok(render_tmux_format(format, &pane))
}

fn resolve_target_pane_id(
    tab: &WorkspaceTab,
    target: Option<&str>,
    fallback: &str,
) -> Result<String, TmuxHttpError> {
    let Some(target) = target else {
        return Ok(fallback.to_string());
    };

    let token = extract_target_token(target);
    if token.is_empty() {
        return Ok(fallback.to_string());
    }

    let normalized = token.strip_prefix('%').unwrap_or(token.as_str());
    if stack_exists(&tab.root, normalized) {
        return Ok(normalized.to_string());
    }

    let stack_ids = collect_stack_ids(&tab.root);

    if let Ok(index) = normalized.parse::<usize>() {
        if let Some(stack_id) = stack_ids.get(index) {
            return Ok(stack_id.clone());
        }
        if index > 0 {
            let one_based = index - 1;
            if let Some(stack_id) = stack_ids.get(one_based) {
                return Ok(stack_id.clone());
            }
        }
    }

    Err(TmuxHttpError {
        status: 404,
        message: format!("Target pane '{target}' not found in caller tab"),
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct NormalizedRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl NormalizedRect {
    fn area(self) -> f64 {
        self.width * self.height
    }
}

#[derive(Clone, Debug)]
struct PaneRect {
    pane_id: String,
    created_by: PaneCreatedBy,
    rect: NormalizedRect,
}

fn split_into_ai_workspace(
    state: &AppState,
    project_id: &str,
    tab: &mut WorkspaceTab,
    caller_pane_id: &str,
    explicit_target_pane_id: Option<&str>,
    requested_direction: Option<&str>,
    source_pane_id: String,
) -> Result<(String, String), TmuxHttpError> {
    let ai_workspace_exists = find_ai_workspace_zone(&tab.root).is_some();

    if !ai_workspace_exists {
        let direction =
            resolve_initial_ai_workspace_direction(state, project_id, &tab.id, requested_direction);
        let new_pane_id =
            wrap_root_with_ai_workspace(tab, &direction, Some(source_pane_id.to_string()));
        return Ok((new_pane_id, direction));
    }

    let split_target_pane_id = if let Some(target_pane_id) = explicit_target_pane_id {
        if is_ai_pane(&tab.root, target_pane_id) {
            target_pane_id.to_string()
        } else {
            select_largest_ai_pane(&tab.root)
                .map(|pane| pane.pane_id)
                .unwrap_or_else(|| caller_pane_id.to_string())
        }
    } else {
        select_largest_ai_pane(&tab.root)
            .map(|pane| pane.pane_id)
            .unwrap_or_else(|| caller_pane_id.to_string())
    };

    let target_rect = find_pane_rect(&tab.root, &split_target_pane_id)
        .map(|pane| pane.rect)
        .unwrap_or(NormalizedRect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
    let direction = resolve_ai_workspace_split_direction(requested_direction, target_rect);

    if !split_stack_node(
        &mut tab.root,
        &split_target_pane_id,
        &direction,
        &mut tab.next_pane_ordinal,
        PaneCreatedBy::Ai,
    ) {
        return Err(TmuxHttpError {
            status: 404,
            message: "Target pane not found".to_string(),
        });
    }

    let new_pane_id = collect_stack_ids(&tab.root)
        .into_iter()
        .find(|pane_id| {
            matches!(
                find_pane_rect(&tab.root, pane_id),
                Some(PaneRect {
                    created_by: PaneCreatedBy::Ai,
                    ..
                })
            ) && source_pane_matches(&tab.root, pane_id, &split_target_pane_id)
        })
        .ok_or_else(|| TmuxHttpError {
            status: 500,
            message: "Failed to locate new pane".to_string(),
        })?;

    Ok((new_pane_id, direction))
}

fn source_pane_matches(node: &models::LayoutNode, target_pane_id: &str, source_pane_id: &str) -> bool {
    match node {
        models::LayoutNode::Stack {
            id,
            source_pane_id: current_source,
            ..
        } => id == target_pane_id && current_source.as_deref() == Some(source_pane_id),
        models::LayoutNode::Split { children, .. } => children
            .iter()
            .any(|child| source_pane_matches(child, target_pane_id, source_pane_id)),
    }
}

fn resolve_initial_ai_workspace_direction(
    state: &AppState,
    project_id: &str,
    tab_id: &str,
    requested_direction: Option<&str>,
) -> String {
    if let Some(direction) = normalize_split_direction(requested_direction) {
        return direction;
    }

    let viewport = state
        .tab_viewports
        .lock()
        .ok()
        .and_then(|cache| cache.get(&tab_viewport_key(project_id, tab_id)).copied());

    match viewport {
        Some(viewport) if viewport.height > viewport.width => "vertical".to_string(),
        _ => "horizontal".to_string(),
    }
}

fn resolve_ai_workspace_split_direction(
    requested_direction: Option<&str>,
    rect: NormalizedRect,
) -> String {
    if let Some(direction) = normalize_split_direction(requested_direction) {
        return direction;
    }

    if rect.width >= rect.height {
        "horizontal".to_string()
    } else {
        "vertical".to_string()
    }
}

fn normalize_split_direction(direction: Option<&str>) -> Option<String> {
    match direction {
        Some("horizontal") | Some("h") => Some("horizontal".to_string()),
        Some("vertical") | Some("v") => Some("vertical".to_string()),
        Some(_) => Some("horizontal".to_string()),
        None => None,
    }
}

fn find_ai_workspace_zone(node: &models::LayoutNode) -> Option<&models::LayoutNode> {
    match node {
        models::LayoutNode::Split {
            zone_kind: SplitZoneKind::AiWorkspace,
            ..
        } => Some(node),
        models::LayoutNode::Split { children, .. } => {
            children.iter().find_map(find_ai_workspace_zone)
        }
        _ => None,
    }
}

fn is_ai_pane(node: &models::LayoutNode, target_pane_id: &str) -> bool {
    match node {
        models::LayoutNode::Stack { id, created_by, .. } => {
            id == target_pane_id && *created_by == PaneCreatedBy::Ai
        }
        models::LayoutNode::Split { children, .. } => {
            children.iter().any(|child| is_ai_pane(child, target_pane_id))
        }
    }
}

fn select_largest_ai_pane(node: &models::LayoutNode) -> Option<PaneRect> {
    let mut panes = Vec::new();
    collect_pane_rects(
        node,
        NormalizedRect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
        &mut panes,
    );

    panes
        .into_iter()
        .filter(|pane| pane.created_by == PaneCreatedBy::Ai)
        .max_by(|left, right| left.rect.area().total_cmp(&right.rect.area()))
}

fn find_pane_rect(node: &models::LayoutNode, target_pane_id: &str) -> Option<PaneRect> {
    let mut panes = Vec::new();
    collect_pane_rects(
        node,
        NormalizedRect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
        &mut panes,
    );
    panes.into_iter().find(|pane| pane.pane_id == target_pane_id)
}

fn collect_pane_rects(
    node: &models::LayoutNode,
    rect: NormalizedRect,
    panes: &mut Vec<PaneRect>,
) {
    match node {
        models::LayoutNode::Stack { id, created_by, .. } => panes.push(PaneRect {
            pane_id: id.clone(),
            created_by: created_by.clone(),
            rect,
        }),
        models::LayoutNode::Split {
            direction,
            sizes,
            children,
            ..
        } => {
            let mut offset = 0.0;
            let child_count = children.len().max(1);
            for (index, child) in children.iter().enumerate() {
                let ratio = sizes
                    .get(index)
                    .copied()
                    .unwrap_or((100 / child_count) as u16) as f64
                    / 100.0;
                let child_rect = if direction == "vertical" {
                    NormalizedRect {
                        x: rect.x,
                        y: rect.y + (rect.height * offset),
                        width: rect.width,
                        height: rect.height * ratio,
                    }
                } else {
                    NormalizedRect {
                        x: rect.x + (rect.width * offset),
                        y: rect.y,
                        width: rect.width * ratio,
                        height: rect.height,
                    }
                };
                collect_pane_rects(child, child_rect, panes);
                offset += ratio;
            }
        }
    }
}

fn collect_stack_ids(node: &models::LayoutNode) -> Vec<String> {
    let mut stack_ids = Vec::new();
    collect_stack_ids_into(node, &mut stack_ids);
    stack_ids
}

fn collect_stack_ids_into(node: &models::LayoutNode, stack_ids: &mut Vec<String>) {
    match node {
        models::LayoutNode::Stack { id, .. } => stack_ids.push(id.clone()),
        models::LayoutNode::Split { children, .. } => {
            for child in children {
                collect_stack_ids_into(child, stack_ids);
            }
        }
    }
}

fn extract_target_token(target: &str) -> String {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let after_session = trimmed
        .rsplit_once(':')
        .map(|(_, tail)| tail)
        .unwrap_or(trimmed);
    let after_window = after_session
        .rsplit_once('.')
        .map(|(_, tail)| tail)
        .unwrap_or(after_session);
    after_window.trim().to_string()
}

fn active_session_for_pane(node: &models::LayoutNode, target_pane_id: &str) -> Option<String> {
    match node {
        models::LayoutNode::Stack {
            id,
            active_item_id,
            items,
            ..
        } if id == target_pane_id => items
            .iter()
            .find(|item| item.id == *active_item_id)
            .and_then(|item| item.session_id.clone())
            .or_else(|| items.iter().find_map(|item| item.session_id.clone())),
        models::LayoutNode::Split { children, .. } => {
            for child in children {
                if let Some(session_id) = active_session_for_pane(child, target_pane_id) {
                    return Some(session_id);
                }
            }
            None
        }
        _ => None,
    }
}

fn collect_tab_panes(
    node: &models::LayoutNode,
    sessions: &[TerminalSession],
    tab_index: usize,
    window_id: &str,
    caller_session_id: &str,
    panes: &mut Vec<PaneListing>,
) {
    match node {
        models::LayoutNode::Stack {
            id,
            items,
            active_item_id,
            ..
        } => {
            let active_session_id = items
                .iter()
                .find(|item| item.id == *active_item_id)
                .and_then(|item| item.session_id.clone())
                .or_else(|| items.iter().find_map(|item| item.session_id.clone()));
            let active_session = active_session_id
                .as_ref()
                .and_then(|session_id| sessions.iter().find(|session| session.id == *session_id));
            panes.push(PaneListing {
                pane_id: id.clone(),
                pane_index: panes.len(),
                pane_title: active_session
                    .map(|session| session.title.clone())
                    .unwrap_or_else(|| "pane".to_string()),
                pane_current_command: active_session
                    .map(|session| session.program.clone())
                    .unwrap_or_else(|| "terminal".to_string()),
                window_index: tab_index,
                session_name: caller_session_id.to_string(),
                window_id: window_id.to_string(),
            });
        }
        models::LayoutNode::Split { children, .. } => {
            for child in children {
                collect_tab_panes(
                    child,
                    sessions,
                    tab_index,
                    window_id,
                    caller_session_id,
                    panes,
                );
            }
        }
    }
}

fn render_tmux_format(format: &str, pane: &PaneListing) -> String {
    let pane_id = format!("%{}", pane.pane_id);
    let mut result = format.to_string();
    result = result.replace("#{pane_id}", &pane_id);
    result = result.replace("#{pane_index}", &pane.pane_index.to_string());
    result = result.replace("#{pane_title}", &pane.pane_title);
    result = result.replace("#{pane_current_command}", &pane.pane_current_command);
    result = result.replace("#{window_index}", &pane.window_index.to_string());
    result = result.replace("#{session_name}", &pane.session_name);
    result = result.replace("#{window_id}", &pane.window_id);
    result = result.replace("#D", &pane_id);
    result = result.replace("#I", &pane.window_index.to_string());
    result = result.replace("#S", &pane.session_name);
    result
}

fn default_tmux_shell_program() -> String {
    if cfg!(windows) {
        "powershell".to_string()
    } else {
        "bash".to_string()
    }
}

fn get_query_value(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| urlencoding::decode(v).ok().map(|s| s.into_owned()))
}

fn build_http_response(
    status: u16,
    body: String,
    content_type: &'static str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response = Response::from_string(body).with_status_code(StatusCode(status));
    if let Ok(header) = Header::from_bytes("Content-Type", content_type) {
        response = response.with_header(header);
    }
    response
}

fn internal_error(message: impl std::fmt::Display) -> TmuxHttpError {
    TmuxHttpError {
        status: 500,
        message: message.to_string(),
    }
}

fn tab_viewport_key(project_id: &str, tab_id: &str) -> String {
    format!("{project_id}:{tab_id}")
}

fn save_clipboard_image_to_temp(width: u32, height: u32, rgba: &[u8]) -> Result<String, String> {
    if width == 0 || height == 0 || rgba.is_empty() {
        return Err("Clipboard image is empty".to_string());
    }

    let paste_dir = std::env::temp_dir().join("WorkspaceTerminal").join("paste");
    fs::create_dir_all(&paste_dir).map_err(|err| err.to_string())?;

    let file_path = paste_dir.join(format!("workspace-terminal-paste-{}.png", Uuid::new_v4()));
    let file = fs::File::create(&file_path).map_err(|err| err.to_string())?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = encoder.write_header().map_err(|err| err.to_string())?;
    png_writer
        .write_image_data(rgba)
        .map_err(|err| err.to_string())?;

    Ok(file_path.to_string_lossy().to_string())
}

#[cfg(windows)]
fn read_windows_clipboard_file_paths() -> Result<Option<Vec<String>>, String> {
    const CF_HDROP_FORMAT: u32 = 15;

    unsafe {
        if OpenClipboard(0 as HANDLE) == 0 {
            return Ok(None);
        }

        let result = (|| {
            let handle = GetClipboardData(CF_HDROP_FORMAT);
            if handle.is_null() {
                return Ok(None);
            }

            let file_count = DragQueryFileW(handle as _, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
            if file_count == 0 {
                return Ok(None);
            }

            let mut paths = Vec::with_capacity(file_count as usize);
            for index in 0..file_count {
                let length = DragQueryFileW(handle as _, index, std::ptr::null_mut(), 0);
                if length == 0 {
                    continue;
                }

                let mut buffer = vec![0_u16; length as usize + 1];
                let copied = DragQueryFileW(handle as _, index, buffer.as_mut_ptr(), buffer.len() as u32);
                if copied == 0 {
                    continue;
                }

                let path = String::from_utf16_lossy(&buffer[..copied as usize]);
                if !path.is_empty() {
                    paths.push(path);
                }
            }

            if paths.is_empty() {
                Ok(None)
            } else {
                Ok(Some(paths))
            }
        })();

        CloseClipboard();
        result
    }
}

#[cfg(not(windows))]
fn read_windows_clipboard_file_paths() -> Result<Option<Vec<String>>, String> {
    Ok(None)
}

fn load_workspace_snapshot(
    state: &AppState,
    project_id: &str,
) -> Result<WorkspaceSnapshot, String> {
    let cached_snapshot = {
        let workspaces = state
            .workspaces
            .lock()
            .map_err(|_| "Workspace cache lock poisoned".to_string())?;
        workspaces.get(project_id).cloned()
    };

    if let Some(snapshot) = cached_snapshot {
        let mut snapshot = snapshot;
        refresh_snapshot_sessions(state, &mut snapshot)?;
        state
            .workspaces
            .lock()
            .map_err(|_| "Workspace cache lock poisoned".to_string())?
            .insert(project_id.to_string(), snapshot.clone());
        return Ok(snapshot);
    }

    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    let mut snapshot = db.load_workspace(project_id)?;
    drop(db);

    if snapshot.tabs.is_empty() {
        let replacement = new_workspace_tab("main".to_string());
        snapshot.active_tab_id = Some(replacement.id.clone());
        snapshot.tabs.push(replacement);
    } else {
        for tab in snapshot.tabs.iter_mut() {
            reset_tab_layout(tab);
        }
        if !snapshot
            .active_tab_id
            .as_ref()
            .is_some_and(|tab_id| snapshot.tabs.iter().any(|tab| tab.id == *tab_id))
        {
            snapshot.active_tab_id = snapshot.tabs.first().map(|tab| tab.id.clone());
        }
    }

    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    db.save_workspace_outline(&snapshot)?;
    drop(db);
    refresh_snapshot_sessions(state, &mut snapshot)?;
    state
        .workspaces
        .lock()
        .map_err(|_| "Workspace cache lock poisoned".to_string())?
        .insert(project_id.to_string(), snapshot.clone());
    Ok(snapshot)
}

fn refresh_snapshot_sessions(
    state: &AppState,
    snapshot: &mut WorkspaceSnapshot,
) -> Result<(), String> {
    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    snapshot.sessions = db.list_sessions(&snapshot.project_id)?;
    Ok(())
}

fn persist_workspace_snapshot(
    state: &AppState,
    snapshot: WorkspaceSnapshot,
) -> Result<WorkspaceSnapshot, String> {
    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    db.save_workspace_outline(&snapshot)?;
    drop(db);

    state
        .workspaces
        .lock()
        .map_err(|_| "Workspace cache lock poisoned".to_string())?
        .insert(snapshot.project_id.clone(), snapshot.clone());
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::{WorkspaceSnapshot, close_pane_in_snapshot, remove_session_from_snapshot};
    use crate::layout::{add_session_to_stack, first_stack_id, new_workspace_tab};
    use crate::models::{LayoutNode, PaneCreatedBy, PaneLaunchState};

    #[test]
    fn ended_last_root_session_resets_tab_to_launcher() {
        let mut tab = new_workspace_tab("main".to_string());
        let stack_id = first_stack_id(&tab.root).expect("root stack");
        add_session_to_stack(&mut tab.root, &stack_id, "session-1", "Terminal");

        let mut snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            active_tab_id: Some(tab.id.clone()),
            tabs: vec![tab],
            sessions: Vec::new(),
        };

        assert!(remove_session_from_snapshot(&mut snapshot, "session-1"));
        assert_eq!(snapshot.tabs.len(), 1);

        match &snapshot.tabs[0].root {
            LayoutNode::Stack {
                created_by,
                launch_state,
                items,
                ..
            } => {
                assert_eq!(created_by, &PaneCreatedBy::User);
                assert_eq!(launch_state, &PaneLaunchState::Unlaunched);
                assert_eq!(items.len(), 1);
                assert!(items[0].session_id.is_none());
            }
            LayoutNode::Split { .. } => panic!("expected root stack launcher"),
        }
    }

    #[test]
    fn ended_single_session_in_secondary_tab_closes_tab() {
        let primary_tab = new_workspace_tab("main".to_string());

        let mut secondary_tab = new_workspace_tab("tmux".to_string());
        let stack_id = first_stack_id(&secondary_tab.root).expect("root stack");
        add_session_to_stack(
            &mut secondary_tab.root,
            &stack_id,
            "session-2",
            "AI Terminal",
        );

        let mut snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            active_tab_id: Some(secondary_tab.id.clone()),
            tabs: vec![primary_tab.clone(), secondary_tab.clone()],
            sessions: Vec::new(),
        };

        assert!(remove_session_from_snapshot(&mut snapshot, "session-2"));
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].id, primary_tab.id);
        assert_eq!(
            snapshot.active_tab_id.as_deref(),
            Some(primary_tab.id.as_str())
        );
    }

    #[test]
    fn closing_root_pane_in_secondary_tab_removes_tab() {
        let primary_tab = new_workspace_tab("main".to_string());

        let secondary_tab = new_workspace_tab("tmux".to_string());
        let stack_id = first_stack_id(&secondary_tab.root).expect("root stack");

        let mut snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            active_tab_id: Some(secondary_tab.id.clone()),
            tabs: vec![primary_tab.clone(), secondary_tab.clone()],
            sessions: Vec::new(),
        };

        let session_ids = close_pane_in_snapshot(&mut snapshot, &secondary_tab.id, &stack_id)
            .expect("close pane");
        assert!(session_ids.is_empty());
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].id, primary_tab.id);
    }

    #[test]
    fn closing_last_root_pane_resets_launcher() {
        let mut tab = new_workspace_tab("main".to_string());
        let stack_id = first_stack_id(&tab.root).expect("root stack");
        add_session_to_stack(&mut tab.root, &stack_id, "session-3", "Terminal");

        let mut snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            active_tab_id: Some(tab.id.clone()),
            tabs: vec![tab],
            sessions: Vec::new(),
        };

        let tab_id = snapshot.tabs[0].id.clone();
        let session_ids =
            close_pane_in_snapshot(&mut snapshot, &tab_id, &stack_id).expect("close pane");
        assert_eq!(session_ids, vec!["session-3".to_string()]);

        match &snapshot.tabs[0].root {
            LayoutNode::Stack { launch_state, .. } => {
                assert_eq!(launch_state, &PaneLaunchState::Unlaunched);
            }
            LayoutNode::Split { .. } => panic!("expected launcher root"),
        }
    }
}
