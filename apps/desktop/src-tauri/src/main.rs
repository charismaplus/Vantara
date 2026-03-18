#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod db;
mod layout;
mod models;
mod sessions;

use std::{
    collections::HashMap,
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::anyhow;
use db::{Database, now_iso};
use layout::{
    add_session_to_stack, close_session_in_layout, close_stack_node, collect_session_ids,
    new_workspace_tab, reset_tab_layout, set_active_stack_item, split_stack_node, stack_exists,
};
use models::{PaneCreatedBy, Project, SessionStatus, TerminalSession, WorkspaceSnapshot};
use sessions::SessionManager;
use tauri::{Manager, State};
use uuid::Uuid;

struct AppState {
    db: Arc<Mutex<Database>>,
    sessions: SessionManager,
    workspaces: Arc<Mutex<HashMap<String, WorkspaceSnapshot>>>,
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
    let tab = new_workspace_tab(title.unwrap_or_else(|| format!("tab-{}", snapshot.tabs.len() + 1)));
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
        terminate_if_running(&state.sessions, &session_id)?;
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
    let Some(tab) = snapshot.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
        return Err("Tab not found".to_string());
    };

    let changed = close_session_in_layout(&mut tab.root, &stack_id, &session_id);
    if !changed {
        return Err("Session not found in stack".to_string());
    }

    terminate_if_running(&state.sessions, &session_id)?;
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
        let Some(tab) = snapshot.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return Err("Tab not found".to_string());
        };

        let Some(session_ids) = close_stack_node(&mut tab.root, &stack_id) else {
            return Err("Pane not found".to_string());
        };
        (session_ids, snapshot)
    };

    for session_id in &session_ids {
        terminate_if_running(&state.sessions, session_id)?;
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
) -> Result<WorkspaceSnapshot, String> {
    let program = program.unwrap_or_else(|| "powershell".to_string());
    let session_title = title.unwrap_or_else(|| program.clone());

    let project = {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.get_project(&project_id)?
            .ok_or_else(|| "Project not found".to_string())?
    };

    let mut snapshot = {
        let snapshot = load_workspace_snapshot(&state, &project_id)?;
        let Some(tab) = snapshot.tabs.iter().find(|tab| tab.id == tab_id) else {
            return Err("Tab not found".to_string());
        };
        if !stack_exists(&tab.root, &stack_id) {
            return Err("Target stack not found".to_string());
        }
        snapshot
    };

    let session_cwd = match cwd {
        Some(custom_cwd) => normalize_path(&custom_cwd)?,
        None => project.path.clone(),
    };
    if !Path::new(&session_cwd).is_dir() {
        return Err("Working directory does not exist".to_string());
    }

    let mut session = TerminalSession {
        id: Uuid::new_v4().to_string(),
        project_id: project_id.clone(),
        title: session_title.clone(),
        program,
        args,
        cwd: session_cwd,
        status: SessionStatus::Starting,
        started_at: Some(now_iso()),
        ended_at: None,
        exit_code: None,
    };

    session = state.sessions.create(app, state.db.clone(), session)?;
    if let Some(tab) = snapshot.tabs.iter_mut().find(|tab| tab.id == tab_id) {
        add_session_to_stack(&mut tab.root, &stack_id, &session.id, &session.title);
    }

    refresh_snapshot_sessions(&state, &mut snapshot)?;
    if let Err(err) = persist_workspace_snapshot(&state, snapshot.clone()) {
        let _ = terminate_if_running(&state.sessions, &session.id);
        return Err(err);
    }
    Ok(snapshot)
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
    state.sessions.terminate(&session_id)
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .map_err(|err| tauri::Error::Anyhow(anyhow!(err.to_string())))?;
            let db = Database::new(app_dir).map_err(|err| tauri::Error::Anyhow(anyhow!(err)))?;
            db.mark_stale_sessions()
                .map_err(|err| tauri::Error::Anyhow(anyhow!(err)))?;

            app.manage(AppState {
                db: Arc::new(Mutex::new(db)),
                sessions: SessionManager::default(),
                workspaces: Arc::new(Mutex::new(HashMap::new())),
            });
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            list_projects,
            create_project,
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
            terminate_session,
            set_active_stack_item_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn normalize_path(path: &str) -> Result<String, String> {
    let canonical = fs::canonicalize(path).map_err(|err| err.to_string())?;
    let normalized = canonical.to_string_lossy().replace('/', "\\");
    if cfg!(windows) {
        Ok(normalized.to_lowercase())
    } else {
        Ok(normalized)
    }
}

fn terminate_if_running(sessions: &SessionManager, session_id: &str) -> Result<(), String> {
    match sessions.terminate(session_id) {
        Ok(()) => Ok(()),
        Err(err) if err == "Session not found" => Ok(()),
        Err(err) => Err(err),
    }
}

fn load_workspace_snapshot(state: &AppState, project_id: &str) -> Result<WorkspaceSnapshot, String> {
    if let Some(snapshot) = state
        .workspaces
        .lock()
        .map_err(|_| "Workspace cache lock poisoned".to_string())?
        .get(project_id)
        .cloned()
    {
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

fn refresh_snapshot_sessions(state: &AppState, snapshot: &mut WorkspaceSnapshot) -> Result<(), String> {
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
