#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod db;
mod embedded_tmux {
    include!(concat!(env!("OUT_DIR"), "/embedded_tmux.rs"));
}
mod layout;
mod models;
mod sessions;

use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
};

use anyhow::anyhow;
use arboard::Clipboard;
use db::{Database, now_iso};
use layout::{
    ClosePaneResult, CloseSessionResult, add_session_to_stack, close_session_in_layout,
    close_stack_node, collect_session_ids, find_stack_id_for_session, first_stack_id,
    new_stack_node, new_workspace_tab, normalize_tab, reset_tab_layout, set_active_stack_item,
    split_stack_node, split_stack_node_with_options, stack_exists, wrap_root_with_split,
    SplitInsertion,
};
use models::{
    DeleteProjectResult, LaunchProfile, PaneCreatedBy, Project, ProjectWorkspaceSnapshot,
    SessionSidebarStatus, SessionStatus, TerminalSession, WorkspaceChangedEvent,
    WorkspaceSession, WorkspaceSessionCreatedBy, WorkspaceSnapshot, WorkspaceTab,
};
use serde::{Deserialize, Serialize};
use sessions::{
    SessionCaptureOptions, SessionCreateOptions, SessionManager, SessionPipeOptions,
    SessionShellKind,
};
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
    workspace_session_id: String,
    origin_tab_id: String,
    origin_pane_id: String,
    previous_tab_id: Option<String>,
    previous_pane_id: Option<String>,
    current_tab_id: String,
    current_pane_id: String,
    launch_profile: LaunchProfile,
}

#[derive(Clone)]
struct TmuxShimState {
    port: u16,
    fallback_shim_dir: PathBuf,
    app_handle: tauri::AppHandle,
    extracted_shim_dir: Arc<Mutex<Option<PathBuf>>>,
    tokens: Arc<Mutex<HashMap<String, TmuxTokenContext>>>,
    synthetic: Arc<Mutex<TmuxSyntheticState>>,
    waits: Arc<TmuxWaitRegistry>,
}

#[derive(Default)]
struct TmuxSyntheticState {
    session_names: HashMap<String, String>,
    environment: HashMap<String, String>,
    global_options: HashMap<String, String>,
    project_options: HashMap<String, HashMap<String, String>>,
    global_window_options: HashMap<String, String>,
    window_options: HashMap<String, HashMap<String, String>>,
    key_bindings: HashMap<String, String>,
    hooks: HashMap<String, String>,
    buffers: HashMap<String, String>,
    last_buffer_name: Option<String>,
}

#[derive(Default)]
struct TmuxWaitState {
    signals: HashMap<String, usize>,
    locks: HashMap<String, bool>,
}

#[derive(Default)]
struct TmuxWaitRegistry {
    state: Mutex<TmuxWaitState>,
    condvar: Condvar,
}

#[derive(Default)]
struct TmuxRequestQueue {
    queue: Mutex<VecDeque<Request>>,
    condvar: Condvar,
}

#[derive(Clone, Copy)]
enum TmuxOptionScope {
    Session,
    Window,
}

impl TmuxShimState {
    fn new(port: u16, fallback_shim_dir: PathBuf, app_handle: tauri::AppHandle) -> Self {
        Self {
            port,
            fallback_shim_dir,
            app_handle,
            extracted_shim_dir: Arc::new(Mutex::new(None)),
            tokens: Arc::new(Mutex::new(HashMap::new())),
            synthetic: Arc::new(Mutex::new(TmuxSyntheticState::default())),
            waits: Arc::new(TmuxWaitRegistry::default()),
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

    fn update_session_focus(
        &self,
        session_id: &str,
        tab_id: String,
        pane_id: String,
    ) -> Result<(), String> {
        let mut tokens = self
            .tokens
            .lock()
            .map_err(|_| "Tmux token lock poisoned".to_string())?;
        for context in tokens.values_mut() {
            if context.session_id == session_id {
                if context.current_tab_id != tab_id {
                    context.previous_tab_id = Some(context.current_tab_id.clone());
                    context.previous_pane_id = None;
                } else if context.current_pane_id != pane_id {
                    context.previous_pane_id = Some(context.current_pane_id.clone());
                }
                context.current_tab_id = tab_id.clone();
                context.current_pane_id = pane_id.clone();
            }
        }
        Ok(())
    }

    fn switch_workspace_session(
        &self,
        session_id: &str,
        workspace_session_id: String,
        tab_id: String,
        pane_id: String,
    ) -> Result<(), String> {
        let mut tokens = self
            .tokens
            .lock()
            .map_err(|_| "Tmux token lock poisoned".to_string())?;
        for context in tokens.values_mut() {
            if context.session_id == session_id {
                context.workspace_session_id = workspace_session_id.clone();
                context.origin_tab_id = tab_id.clone();
                context.origin_pane_id = pane_id.clone();
                context.previous_tab_id = None;
                context.previous_pane_id = None;
                context.current_tab_id = tab_id.clone();
                context.current_pane_id = pane_id.clone();
            }
        }
        Ok(())
    }

    fn session_name(&self, workspace_session_id: &str, fallback: &str) -> Result<String, String> {
        let synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        Ok(synthetic
            .session_names
            .get(workspace_session_id)
            .cloned()
            .unwrap_or_else(|| fallback.to_string()))
    }

    fn rename_session(&self, workspace_session_id: &str, name: String) -> Result<(), String> {
        let mut synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        synthetic
            .session_names
            .insert(workspace_session_id.to_string(), name);
        Ok(())
    }

    fn remove_session_name(&self, workspace_session_id: &str) -> Result<(), String> {
        let mut synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        synthetic.session_names.remove(workspace_session_id);
        Ok(())
    }

    fn set_key_binding(&self, key: &str, command: Option<String>) -> Result<(), String> {
        let mut synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        if let Some(command) = command {
            synthetic.key_bindings.insert(key.to_string(), command);
        } else {
            synthetic.key_bindings.remove(key);
        }
        Ok(())
    }

    fn key_binding_entries(&self, table: Option<&str>) -> Result<Vec<(String, String)>, String> {
        let synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        let mut entries = synthetic
            .key_bindings
            .iter()
            .filter(|(key, _)| {
                table.is_none_or(|table_name| {
                    key.split_once("::")
                        .map(|(entry_table, _)| entry_table == table_name)
                        .unwrap_or(false)
                })
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    fn set_hook(&self, key: &str, command: Option<String>) -> Result<(), String> {
        let mut synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        if let Some(command) = command {
            synthetic.hooks.insert(key.to_string(), command);
        } else {
            synthetic.hooks.remove(key);
        }
        Ok(())
    }

    fn hook_entries(&self, prefix: Option<&str>) -> Result<Vec<(String, String)>, String> {
        let synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        let mut entries = synthetic
            .hooks
            .iter()
            .filter(|(key, _)| prefix.is_none_or(|prefix| key.starts_with(prefix)))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    fn set_buffer(
        &self,
        name: Option<String>,
        value: String,
        append: bool,
    ) -> Result<String, String> {
        let mut synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        let buffer_name = name
            .filter(|entry| !entry.trim().is_empty())
            .unwrap_or_else(|| {
                synthetic
                    .last_buffer_name
                    .clone()
                    .unwrap_or_else(|| next_tmux_buffer_name(&synthetic.buffers))
            });
        if append {
            synthetic
                .buffers
                .entry(buffer_name.clone())
                .and_modify(|existing| existing.push_str(&value))
                .or_insert(value);
        } else {
            synthetic.buffers.insert(buffer_name.clone(), value);
        }
        synthetic.last_buffer_name = Some(buffer_name.clone());
        Ok(buffer_name)
    }

    fn buffer_entries(&self) -> Result<Vec<(String, String)>, String> {
        let synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        let mut entries = synthetic
            .buffers
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    fn buffer_value(&self, name: Option<&str>) -> Result<Option<(String, String)>, String> {
        let synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        if let Some(name) = name {
            return Ok(synthetic
                .buffers
                .get(name)
                .cloned()
                .map(|value| (name.to_string(), value)));
        }
        let selected_name = synthetic
            .last_buffer_name
            .clone()
            .or_else(|| synthetic.buffers.keys().min().cloned());
        Ok(selected_name.and_then(|buffer_name| {
            synthetic
                .buffers
                .get(&buffer_name)
                .cloned()
                .map(|value| (buffer_name, value))
        }))
    }

    fn delete_buffer(&self, name: Option<&str>, delete_all: bool) -> Result<(), String> {
        let mut synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        if delete_all {
            synthetic.buffers.clear();
            synthetic.last_buffer_name = None;
            return Ok(());
        }

        let Some(name) = name else {
            if let Some(last_name) = synthetic.last_buffer_name.clone() {
                synthetic.buffers.remove(&last_name);
                if synthetic.last_buffer_name.as_deref() == Some(last_name.as_str()) {
                    synthetic.last_buffer_name = synthetic.buffers.keys().min().cloned();
                }
            }
            return Ok(());
        };

        synthetic.buffers.remove(name);
        if synthetic.last_buffer_name.as_deref() == Some(name) {
            synthetic.last_buffer_name = synthetic.buffers.keys().min().cloned();
        }
        Ok(())
    }

    fn wait_for(&self, name: &str) -> Result<(), String> {
        let mut state = self
            .waits
            .state
            .lock()
            .map_err(|_| "Tmux wait registry lock poisoned".to_string())?;
        loop {
            if let Some(count) = state.signals.get_mut(name) {
                if *count > 0 {
                    *count -= 1;
                    if *count == 0 {
                        state.signals.remove(name);
                    }
                    return Ok(());
                }
            }
            state = self
                .waits
                .condvar
                .wait(state)
                .map_err(|_| "Tmux wait registry lock poisoned".to_string())?;
        }
    }

    fn signal_wait_for(&self, name: &str) -> Result<(), String> {
        let mut state = self
            .waits
            .state
            .lock()
            .map_err(|_| "Tmux wait registry lock poisoned".to_string())?;
        *state.signals.entry(name.to_string()).or_insert(0) += 1;
        self.waits.condvar.notify_all();
        Ok(())
    }

    fn lock_wait_for(&self, name: &str) -> Result<(), String> {
        let mut state = self
            .waits
            .state
            .lock()
            .map_err(|_| "Tmux wait registry lock poisoned".to_string())?;
        loop {
            let locked = state.locks.get(name).copied().unwrap_or(false);
            if !locked {
                state.locks.insert(name.to_string(), true);
                return Ok(());
            }
            state = self
                .waits
                .condvar
                .wait(state)
                .map_err(|_| "Tmux wait registry lock poisoned".to_string())?;
        }
    }

    fn unlock_wait_for(&self, name: &str) -> Result<(), String> {
        let mut state = self
            .waits
            .state
            .lock()
            .map_err(|_| "Tmux wait registry lock poisoned".to_string())?;
        state.locks.remove(name);
        self.waits.condvar.notify_all();
        Ok(())
    }

    fn set_environment(
        &self,
        key: &str,
        value: Option<String>,
        unset: bool,
    ) -> Result<(), String> {
        let mut synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        if unset {
            synthetic.environment.remove(key);
        } else if let Some(value) = value {
            synthetic.environment.insert(key.to_string(), value);
        } else {
            synthetic
                .environment
                .entry(key.to_string())
                .or_insert_with(String::new);
        }
        Ok(())
    }

    fn environment_entries(&self) -> Result<Vec<(String, String)>, String> {
        let synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        let mut entries = synthetic
            .environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    fn environment_value(&self, key: &str) -> Result<Option<String>, String> {
        let synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        Ok(synthetic.environment.get(key).cloned())
    }

    fn set_option(
        &self,
        scope: TmuxOptionScope,
        project_id: &str,
        tab_id: Option<&str>,
        key: &str,
        value: Option<String>,
        unset: bool,
        append: bool,
        only_if_unset: bool,
        global: bool,
    ) -> Result<(), String> {
        let mut synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;

        let effective_exists = match scope {
            TmuxOptionScope::Session => {
                if !global {
                    synthetic
                        .project_options
                        .get(project_id)
                        .and_then(|map| map.get(key))
                        .is_some()
                        || synthetic.global_options.contains_key(key)
                } else {
                    synthetic.global_options.contains_key(key)
                }
            }
            TmuxOptionScope::Window => {
                let tab_id = tab_id.unwrap_or_default();
                if !global {
                    synthetic
                        .window_options
                        .get(tab_id)
                        .and_then(|map| map.get(key))
                        .is_some()
                        || synthetic.global_window_options.contains_key(key)
                } else {
                    synthetic.global_window_options.contains_key(key)
                }
            }
        };

        if only_if_unset && effective_exists {
            return Ok(());
        }

        let target_map = match scope {
            TmuxOptionScope::Session => {
                if global {
                    &mut synthetic.global_options
                } else {
                    synthetic
                        .project_options
                        .entry(project_id.to_string())
                        .or_default()
                }
            }
            TmuxOptionScope::Window => {
                if global {
                    &mut synthetic.global_window_options
                } else {
                    synthetic
                        .window_options
                        .entry(tab_id.unwrap_or_default().to_string())
                        .or_default()
                }
            }
        };

        if unset {
            target_map.remove(key);
            return Ok(());
        }

        let value = value.unwrap_or_default();
        if append {
            target_map
                .entry(key.to_string())
                .and_modify(|existing| existing.push_str(&value))
                .or_insert(value);
        } else {
            target_map.insert(key.to_string(), value);
        }
        Ok(())
    }

    fn option_entries(
        &self,
        scope: TmuxOptionScope,
        project_id: &str,
        tab_id: Option<&str>,
        global_only: bool,
    ) -> Result<Vec<(String, String)>, String> {
        let synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        let mut merged = HashMap::new();

        match scope {
            TmuxOptionScope::Session => {
                for (key, value) in &synthetic.global_options {
                    merged.insert(key.clone(), value.clone());
                }
                if !global_only {
                    if let Some(local) = synthetic.project_options.get(project_id) {
                        for (key, value) in local {
                            merged.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            TmuxOptionScope::Window => {
                for (key, value) in &synthetic.global_window_options {
                    merged.insert(key.clone(), value.clone());
                }
                if !global_only {
                    if let Some(local) = tab_id.and_then(|id| synthetic.window_options.get(id)) {
                        for (key, value) in local {
                            merged.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
        }

        let mut entries = merged.into_iter().collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    fn option_value(
        &self,
        scope: TmuxOptionScope,
        project_id: &str,
        tab_id: Option<&str>,
        key: &str,
        global_only: bool,
    ) -> Result<Option<String>, String> {
        let synthetic = self
            .synthetic
            .lock()
            .map_err(|_| "Tmux synthetic state lock poisoned".to_string())?;
        Ok(match scope {
            TmuxOptionScope::Session => {
                if !global_only {
                    synthetic
                        .project_options
                        .get(project_id)
                        .and_then(|map| map.get(key))
                        .cloned()
                        .or_else(|| synthetic.global_options.get(key).cloned())
                } else {
                    synthetic.global_options.get(key).cloned()
                }
            }
            TmuxOptionScope::Window => {
                if !global_only {
                    tab_id
                        .and_then(|id| synthetic.window_options.get(id))
                        .and_then(|map| map.get(key))
                        .cloned()
                        .or_else(|| synthetic.global_window_options.get(key).cloned())
                } else {
                    synthetic.global_window_options.get(key).cloned()
                }
            }
        })
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
        .retain(|_, snapshot| snapshot.project_id != project_id);

    Ok(DeleteProjectResult {
        deleted_project_id: project_id,
        next_project_id,
    })
}

#[tauri::command]
fn open_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectWorkspaceSnapshot, String> {
    {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.touch_project(&project_id)?;
    }
    load_project_snapshot(&state, &project_id)
}

#[tauri::command]
fn open_session(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: String,
) -> Result<WorkspaceSnapshot, String> {
    {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.touch_project(&project_id)?;
        db.touch_workspace_session(&project_id, &workspace_session_id)?;
    }

    load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)
}

#[tauri::command]
fn create_workspace_session(
    state: State<'_, AppState>,
    project_id: String,
    name: Option<String>,
    created_by: Option<WorkspaceSessionCreatedBy>,
    source_session_id: Option<String>,
) -> Result<WorkspaceSession, String> {
    let created_session = {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.create_workspace_session(
            &project_id,
            name,
            created_by.unwrap_or(WorkspaceSessionCreatedBy::User),
            source_session_id,
        )?
    };

    emit_workspace_session_changed(&state.tmux.app_handle, &project_id, None);
    Ok(created_session)
}

#[tauri::command]
fn rename_workspace_session(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: String,
    name: String,
) -> Result<WorkspaceSession, String> {
    if name.trim().is_empty() {
        return Err("Session name is required".to_string());
    }

    let updated = {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.rename_workspace_session(&project_id, &workspace_session_id, name.trim())?
    };

    emit_workspace_session_changed(
        &state.tmux.app_handle,
        &project_id,
        Some(workspace_session_id),
    );
    Ok(updated)
}

#[tauri::command]
fn delete_workspace_session(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: String,
) -> Result<ProjectWorkspaceSnapshot, String> {
    let session_ids = {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.list_sessions_for_workspace_session(&workspace_session_id)?
            .into_iter()
            .map(|session| session.id)
            .collect::<Vec<_>>()
    };

    for session_id in &session_ids {
        terminate_if_running(&state, session_id)?;
    }

    {
        let mut db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.delete_workspace_session(&project_id, &workspace_session_id)?;
    }

    state
        .workspaces
        .lock()
        .map_err(|_| "Workspace cache lock poisoned".to_string())?
        .remove(&workspace_session_id);

    emit_workspace_session_changed(&state.tmux.app_handle, &project_id, None);
    load_project_snapshot(&state, &project_id)
}

#[tauri::command]
fn create_window(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: String,
    title: Option<String>,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)?;
    create_window_in_snapshot(&mut snapshot, title);
    persist_workspace_snapshot(&state, snapshot)
}

fn create_window_in_snapshot(snapshot: &mut WorkspaceSnapshot, title: Option<String>) {
    let window =
        new_workspace_tab(title.unwrap_or_else(|| format!("window-{}", snapshot.tabs.len() + 1)));
    snapshot.active_tab_id = Some(window.id.clone());
    snapshot.tabs.push(window);
}

fn close_window_in_snapshot(
    snapshot: &mut WorkspaceSnapshot,
    window_id: &str,
) -> Result<Vec<String>, String> {
    let mut session_ids = Vec::new();
    let Some(window_index) = snapshot.tabs.iter().position(|tab| tab.id == window_id) else {
        return Err("Window not found".to_string());
    };

    collect_session_ids(&snapshot.tabs[window_index].root, &mut session_ids);
    snapshot.tabs.remove(window_index);

    if snapshot.active_tab_id.as_deref() == Some(window_id) {
        snapshot.active_tab_id = snapshot
            .tabs
            .get(window_index.saturating_sub(1))
            .or_else(|| snapshot.tabs.first())
            .map(|tab| tab.id.clone());
    }

    Ok(session_ids)
}

fn rename_window_in_snapshot(
    snapshot: &mut WorkspaceSnapshot,
    window_id: &str,
    title: String,
) -> Result<(), String> {
    let window = snapshot
        .tabs
        .iter_mut()
        .find(|tab| tab.id == window_id)
        .ok_or_else(|| "Window not found".to_string())?;
    window.title = title;
    Ok(())
}

fn set_active_window_in_snapshot(
    snapshot: &mut WorkspaceSnapshot,
    window_id: &str,
) -> Result<(), String> {
    if !snapshot.tabs.iter().any(|tab| tab.id == window_id) {
        return Err("Window not found".to_string());
    }
    snapshot.active_tab_id = Some(window_id.to_string());
    Ok(())
}

fn map_window_error_to_tab(error: String) -> String {
    if error == "Window not found" {
        "Tab not found".to_string()
    } else {
        error
    }
}

#[tauri::command]
fn close_window(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: String,
    window_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let (session_ids, mut snapshot) = {
        let mut snapshot =
            load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)?;
        let session_ids = close_window_in_snapshot(&mut snapshot, &window_id)?;
        (session_ids, snapshot)
    };

    for session_id in &session_ids {
        terminate_if_running(&state, session_id)?;
    }

    refresh_snapshot_sessions(&state, &mut snapshot)?;
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn rename_window(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: String,
    window_id: String,
    title: String,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)?;
    rename_window_in_snapshot(&mut snapshot, &window_id, title)?;
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn set_active_window(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: String,
    window_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)?;
    set_active_window_in_snapshot(&mut snapshot, &window_id)?;
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn set_active_pane(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: String,
    window_id: String,
    pane_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)?;
    let tab = snapshot
        .tabs
        .iter_mut()
        .find(|tab| tab.id == window_id)
        .ok_or_else(|| "Window not found".to_string())?;
    if !stack_exists(&tab.root, &pane_id) {
        return Err("Pane not found".to_string());
    }

    tab.active_pane_id = Some(pane_id);
    snapshot.active_tab_id = Some(window_id);
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn get_session_sidebar_status(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionSidebarStatus, String> {
    let session = {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.get_session(&session_id)?
            .ok_or_else(|| "Session not found".to_string())?
    };

    state.sessions.get_sidebar_status(&session)
}

#[tauri::command]
fn move_pane(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: String,
    window_id: String,
    source_pane_id: String,
    target_pane_id: String,
    placement: String,
) -> Result<WorkspaceSnapshot, String> {
    let mut snapshot = load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)?;
    move_pane_in_snapshot(
        &mut snapshot,
        &window_id,
        &source_pane_id,
        &target_pane_id,
        &placement,
    )?;
    refresh_snapshot_sessions(&state, &mut snapshot)?;
    persist_workspace_snapshot(&state, snapshot)
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
    workspace_session_id: Option<String>,
    title: Option<String>,
) -> Result<WorkspaceSnapshot, String> {
    let session_id =
        workspace_session_id.unwrap_or(default_workspace_session_id(&state, &project_id)?);
    create_window(state, project_id, session_id, title)
}

#[tauri::command]
fn close_tab(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: Option<String>,
    tab_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let session_id =
        workspace_session_id.unwrap_or(default_workspace_session_id(&state, &project_id)?);
    close_window(state, project_id, session_id, tab_id).map_err(map_window_error_to_tab)
}

#[tauri::command]
fn rename_tab(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: Option<String>,
    tab_id: String,
    title: String,
) -> Result<WorkspaceSnapshot, String> {
    let session_id =
        workspace_session_id.unwrap_or(default_workspace_session_id(&state, &project_id)?);
    rename_window(state, project_id, session_id, tab_id, title).map_err(map_window_error_to_tab)
}

#[tauri::command]
fn set_active_tab(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: Option<String>,
    tab_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let session_id =
        workspace_session_id.unwrap_or(default_workspace_session_id(&state, &project_id)?);
    set_active_window(state, project_id, session_id, tab_id).map_err(map_window_error_to_tab)
}

#[tauri::command]
fn split_pane(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: Option<String>,
    tab_id: String,
    stack_id: String,
    direction: String,
) -> Result<WorkspaceSnapshot, String> {
    let session_id =
        workspace_session_id.unwrap_or(default_workspace_session_id(&state, &project_id)?);
    let mut snapshot = load_workspace_session_snapshot(&state, &project_id, &session_id)?;

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
    workspace_session_id: Option<String>,
    tab_id: String,
    stack_id: String,
    session_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let workspace_session_id =
        workspace_session_id.unwrap_or(default_workspace_session_id(&state, &project_id)?);
    let mut snapshot =
        load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)?;
    close_session_in_snapshot(&mut snapshot, &tab_id, &stack_id, &session_id)?;

    terminate_if_running(&state, &session_id)?;
    refresh_snapshot_sessions(&state, &mut snapshot)?;
    persist_workspace_snapshot(&state, snapshot)
}

#[tauri::command]
fn close_pane(
    state: State<'_, AppState>,
    project_id: String,
    workspace_session_id: Option<String>,
    tab_id: String,
    stack_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let (session_ids, mut snapshot) = {
        let workspace_session_id =
            workspace_session_id.unwrap_or(default_workspace_session_id(&state, &project_id)?);
        let mut snapshot =
            load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)?;
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
    workspace_session_id: Option<String>,
    tab_id: String,
    stack_id: String,
    item_id: String,
) -> Result<WorkspaceSnapshot, String> {
    let workspace_session_id =
        workspace_session_id.unwrap_or(default_workspace_session_id(&state, &project_id)?);
    let mut snapshot =
        load_workspace_session_snapshot(&state, &project_id, &workspace_session_id)?;

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
    workspace_session_id: String,
    window_id: Option<String>,
    stack_id: Option<String>,
    title: Option<String>,
    program: Option<String>,
    args: Option<Vec<String>>,
    cwd: Option<String>,
    launch_profile: Option<LaunchProfile>,
) -> Result<WorkspaceSnapshot, String> {
    let (resolved_window_id, resolved_stack_id) =
        ensure_session_spawn_target(&state, &project_id, &workspace_session_id, window_id, stack_id)?;
    let (snapshot, _session) = spawn_session_in_stack(
        app,
        &state,
        SessionSpawnRequest {
            project_id,
            workspace_session_id,
            tab_id: resolved_window_id,
            stack_id: resolved_stack_id,
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
    workspace_session_id: String,
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
        let snapshot =
            load_workspace_session_snapshot(state, &request.project_id, &request.workspace_session_id)?;
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
        workspace_session_id: request.workspace_session_id.clone(),
        window_id: request.tab_id.clone(),
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
                workspace_session_id: request.workspace_session_id.clone(),
                origin_tab_id: request.tab_id.clone(),
                origin_pane_id: request.stack_id.clone(),
                previous_tab_id: None,
                previous_pane_id: None,
                current_tab_id: request.tab_id.clone(),
                current_pane_id: request.stack_id.clone(),
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
    env.insert(
        "WORKSPACE_TERMINAL_WORKSPACE_SESSION_ID".to_string(),
        session.workspace_session_id.clone(),
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
            open_project,
            open_session,
            create_workspace_session,
            rename_workspace_session,
            delete_workspace_session,
            create_window,
            close_window,
            rename_window,
            set_active_window,
            set_active_pane,
            open_workspace,
            create_tab,
            close_tab,
            rename_tab,
            set_active_tab,
            split_pane,
            close_stack_session,
            close_pane,
            move_pane,
            create_session,
            write_session_input,
            resize_session,
            report_tab_viewport,
            read_clipboard_payload,
            get_session_sidebar_status,
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

fn move_pane_in_snapshot(
    snapshot: &mut WorkspaceSnapshot,
    window_id: &str,
    source_pane_id: &str,
    target_pane_id: &str,
    placement: &str,
) -> Result<(), String> {
    let tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == window_id)
        .ok_or_else(|| "Window not found".to_string())?;

    if !stack_exists(&snapshot.tabs[tab_index].root, source_pane_id) {
        return Err("Source pane not found".to_string());
    }
    if !stack_exists(&snapshot.tabs[tab_index].root, target_pane_id) {
        return Err("Target pane not found".to_string());
    }

    if source_pane_id == target_pane_id {
        return Ok(());
    }

    match placement {
        "swap" => {
            let source_node = clone_stack_node(&snapshot.tabs[tab_index].root, source_pane_id)
                .ok_or_else(|| "Source pane not found".to_string())?;
            let target_node = clone_stack_node(&snapshot.tabs[tab_index].root, target_pane_id)
                .ok_or_else(|| "Target pane not found".to_string())?;
            let placeholder_id = Uuid::new_v4().to_string();
            let mut placeholder = source_node.clone();
            match &mut placeholder {
                models::LayoutNode::Stack { id, .. } => {
                    *id = placeholder_id.clone();
                }
                models::LayoutNode::Split { .. } => {
                    return Err("Only pane stacks can be moved".to_string());
                }
            }

            let tab = &mut snapshot.tabs[tab_index];
            let _ = replace_stack_node(&mut tab.root, source_pane_id, &placeholder);
            let _ = replace_stack_node(&mut tab.root, target_pane_id, &source_node);
            let _ = replace_stack_node(&mut tab.root, &placeholder_id, &target_node);
            tab.active_pane_id = Some(source_pane_id.to_string());
            normalize_tab(tab);
        }
        "left" | "right" | "top" | "bottom" => {
            let detached = detach_pane_from_snapshot(snapshot, window_id, source_pane_id)?;
            let tab = snapshot
                .tabs
                .iter_mut()
                .find(|tab| tab.id == window_id)
                .ok_or_else(|| "Window not found".to_string())?;
            let (direction, insertion) = match placement {
                "left" => ("horizontal", SplitInsertion::Before),
                "right" => ("horizontal", SplitInsertion::After),
                "top" => ("vertical", SplitInsertion::Before),
                "bottom" => ("vertical", SplitInsertion::After),
                _ => unreachable!(),
            };
            let mut detached_opt = Some(detached);
            if !insert_existing_stack_node(
                &mut tab.root,
                target_pane_id,
                direction,
                insertion,
                Some(50),
                &mut detached_opt,
            ) {
                return Err("Target pane not found".to_string());
            }
            tab.active_pane_id = Some(source_pane_id.to_string());
            normalize_tab(tab);
        }
        _ => return Err("Unsupported pane placement".to_string()),
    }

    snapshot.active_tab_id = Some(window_id.to_string());
    ensure_valid_active_tab(snapshot);
    Ok(())
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
            session_id: None,
        },
    );
}

fn emit_workspace_session_changed(
    app_handle: &tauri::AppHandle,
    project_id: &str,
    session_id: Option<String>,
) {
    let _ = app_handle.emit(
        "workspace-changed",
        WorkspaceChangedEvent {
            project_id: project_id.to_string(),
            session_id,
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
    #[serde(default)]
    detached: bool,
    #[serde(default)]
    before: bool,
    #[serde(default)]
    full_span: bool,
    size: Option<u16>,
    #[serde(default)]
    size_is_percentage: bool,
    env: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct NewWindowRequest {
    command: Option<String>,
    cwd: Option<String>,
    name: Option<String>,
    target: Option<String>,
    #[serde(default)]
    detached: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct NewSessionRequest {
    name: Option<String>,
    window_name: Option<String>,
    command: Option<String>,
    cwd: Option<String>,
    target: Option<String>,
    #[serde(default)]
    detached: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AttachSessionRequest {
    target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SwitchClientRequest {
    target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct KillSessionRequest {
    target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BindKeyRequest {
    table: Option<String>,
    key: String,
    command: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SetHookRequest {
    target: Option<String>,
    name: String,
    command: Option<String>,
    #[serde(default)]
    global: bool,
    #[serde(default)]
    unset: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SetBufferRequest {
    name: Option<String>,
    value: String,
    #[serde(default)]
    append: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DeleteBufferRequest {
    name: Option<String>,
    #[serde(default)]
    delete_all: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LoadBufferRequest {
    name: Option<String>,
    path: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SaveBufferRequest {
    name: Option<String>,
    path: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PasteBufferRequest {
    name: Option<String>,
    target: Option<String>,
    #[serde(default)]
    delete_after: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WaitForRequest {
    name: String,
    mode: Option<String>,
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

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct KillWindowRequest {
    target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SelectPaneRequest {
    target: Option<String>,
    direction: Option<String>,
    #[serde(default)]
    last: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SelectWindowRequest {
    target: Option<String>,
    mode: Option<String>,
    #[serde(default)]
    toggle_if_current: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RenameSessionRequest {
    target: Option<String>,
    name: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RenameWindowRequest {
    target: Option<String>,
    name: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SetEnvironmentRequest {
    name: String,
    value: Option<String>,
    #[serde(default)]
    unset: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SetOptionRequest {
    target: Option<String>,
    key: String,
    value: Option<String>,
    #[serde(default)]
    unset: bool,
    #[serde(default)]
    append: bool,
    #[serde(default)]
    only_if_unset: bool,
    #[serde(default)]
    global: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RespawnPaneRequest {
    target: Option<String>,
    command: Option<String>,
    cwd: Option<String>,
    #[serde(default)]
    kill_existing: bool,
    env: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RespawnWindowRequest {
    target: Option<String>,
    command: Option<String>,
    cwd: Option<String>,
    #[serde(default)]
    kill_existing: bool,
    env: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BreakPaneRequest {
    source: Option<String>,
    target: Option<String>,
    name: Option<String>,
    #[serde(default)]
    detached: bool,
    #[serde(default)]
    before: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct JoinPaneRequest {
    source: Option<String>,
    target: Option<String>,
    direction: Option<String>,
    #[serde(default)]
    detached: bool,
    #[serde(default)]
    before: bool,
    size: Option<u16>,
    #[serde(default)]
    size_is_percentage: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SwapPaneRequest {
    source: Option<String>,
    target: Option<String>,
    #[serde(default)]
    detached: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SwapWindowRequest {
    source: Option<String>,
    target: Option<String>,
    #[serde(default)]
    detached: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RotateWindowRequest {
    target: Option<String>,
    direction: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MoveWindowRequest {
    source: Option<String>,
    target: Option<String>,
    #[serde(default)]
    before: bool,
    #[serde(default)]
    after: bool,
    #[serde(default)]
    detached: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PipePaneRequest {
    target: Option<String>,
    command: Option<String>,
    #[serde(default)]
    pipe_output: bool,
    #[serde(default)]
    pipe_input: bool,
    #[serde(default)]
    only_if_none: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CapturePaneRequest {
    target: Option<String>,
    #[serde(default)]
    include_escape: bool,
    #[serde(default)]
    join_lines: bool,
    start_line: Option<i32>,
    end_line: Option<i32>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ResizePaneRequest {
    target: Option<String>,
    direction: Option<String>,
    adjustment: Option<u16>,
    width: Option<u16>,
    height: Option<u16>,
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

#[derive(Debug, Clone)]
struct WindowListing {
    window_id: String,
    window_index: usize,
    window_name: String,
    window_active: bool,
    session_name: String,
}

#[derive(Clone, Debug)]
struct SessionListing {
    session_id: String,
    session_name: String,
    session_windows: usize,
    session_attached: bool,
}

#[derive(Clone, Debug)]
struct ClientListing {
    client_name: String,
    client_pid: u32,
    client_tty: String,
    session_name: String,
    window_id: String,
    pane_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeEdge {
    Backward,
    Forward,
}

fn start_tmux_server(
    db: Arc<Mutex<Database>>,
    sessions: SessionManager,
    workspaces: Arc<Mutex<HashMap<String, WorkspaceSnapshot>>>,
    tab_viewports: Arc<Mutex<HashMap<String, TabViewport>>>,
    app_handle: tauri::AppHandle,
) -> Result<TmuxShimState, String> {
    const TMUX_HTTP_WORKER_COUNT: usize = 4;

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

    let runtime_state = Arc::new(AppState {
        db,
        sessions,
        workspaces,
        tab_viewports,
        tmux: tmux.clone(),
    });
    let request_queue = Arc::new(TmuxRequestQueue::default());

    for _ in 0..TMUX_HTTP_WORKER_COUNT {
        let runtime_state = Arc::clone(&runtime_state);
        let request_queue = Arc::clone(&request_queue);
        thread::spawn(move || loop {
            let request = {
                let mut queue = match request_queue.queue.lock() {
                    Ok(queue) => queue,
                    Err(_) => return,
                };
                while queue.is_empty() {
                    queue = match request_queue.condvar.wait(queue) {
                        Ok(queue) => queue,
                        Err(_) => return,
                    };
                }
                match queue.pop_front() {
                    Some(request) => request,
                    None => continue,
                }
            };
            handle_tmux_http_request(request, runtime_state.as_ref());
        });
    }

    thread::spawn(move || {
        for request in server.incoming_requests() {
            let mut queue = match request_queue.queue.lock() {
                Ok(queue) => queue,
                Err(_) => return,
            };
            queue.push_back(request);
            request_queue.condvar.notify_one();
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
            let _ = tmux_has_session(state, &context, get_query_value(query, "target").as_deref())?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/new-session") => {
            let payload: NewSessionRequest = parse_json_body(request)?;
            let result = tmux_new_session(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/attach-session") => {
            let payload: AttachSessionRequest = parse_json_body(request)?;
            let result = tmux_attach_session(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/switch-client") => {
            let payload: SwitchClientRequest = parse_json_body(request)?;
            let result = tmux_switch_client(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/kill-session") => {
            let payload: KillSessionRequest = parse_json_body(request)?;
            tmux_kill_session(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
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
        (&Method::Get, "/v1/tmux/list-windows") => {
            let format =
                get_query_value(query, "format").unwrap_or_else(|| "#{window_id}".to_string());
            let lines = tmux_list_windows(state, &context, &format)?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Get, "/v1/tmux/list-sessions") => {
            let format =
                get_query_value(query, "format").unwrap_or_else(|| "#{session_id}".to_string());
            let lines = tmux_list_sessions(state, &context, &format)?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Get, "/v1/tmux/list-clients") => {
            let format =
                get_query_value(query, "format").unwrap_or_else(|| "#{client_tty}".to_string());
            let lines = tmux_list_clients(state, &context, &format)?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Post, "/v1/tmux/rename-session") => {
            let payload: RenameSessionRequest = parse_json_body(request)?;
            tmux_rename_session(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/bind-key") => {
            let payload: BindKeyRequest = parse_json_body(request)?;
            tmux_bind_key(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/unbind-key") => {
            let payload: BindKeyRequest = parse_json_body(request)?;
            tmux_unbind_key(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Get, "/v1/tmux/list-keys") => {
            let table = get_query_value(query, "table");
            let lines = tmux_list_keys(state, &context, table.as_deref())?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Post, "/v1/tmux/set-environment") => {
            let payload: SetEnvironmentRequest = parse_json_body(request)?;
            tmux_set_environment(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Get, "/v1/tmux/show-environment") => {
            let name = get_query_value(query, "name");
            let value_only = query_flag(query, "valueOnly");
            let lines = tmux_show_environment(state, &context, name.as_deref(), value_only)?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Post, "/v1/tmux/set-option") => {
            let payload: SetOptionRequest = parse_json_body(request)?;
            tmux_set_option(state, &context, TmuxOptionScope::Session, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/set-window-option") => {
            let payload: SetOptionRequest = parse_json_body(request)?;
            tmux_set_option(state, &context, TmuxOptionScope::Window, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Get, "/v1/tmux/show-options") => {
            let target = get_query_value(query, "target");
            let key = get_query_value(query, "key");
            let global_only = query_flag(query, "global");
            let value_only = query_flag(query, "valueOnly");
            let lines = tmux_show_options(
                state,
                &context,
                TmuxOptionScope::Session,
                target.as_deref(),
                key.as_deref(),
                global_only,
                value_only,
            )?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Get, "/v1/tmux/show-window-options") => {
            let target = get_query_value(query, "target");
            let key = get_query_value(query, "key");
            let global_only = query_flag(query, "global");
            let value_only = query_flag(query, "valueOnly");
            let lines = tmux_show_options(
                state,
                &context,
                TmuxOptionScope::Window,
                target.as_deref(),
                key.as_deref(),
                global_only,
                value_only,
            )?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Post, "/v1/tmux/set-hook") => {
            let payload: SetHookRequest = parse_json_body(request)?;
            tmux_set_hook(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Get, "/v1/tmux/show-hooks") => {
            let target = get_query_value(query, "target");
            let global_only = query_flag(query, "global");
            let lines = tmux_show_hooks(state, &context, target.as_deref(), global_only)?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Post, "/v1/tmux/set-buffer") => {
            let payload: SetBufferRequest = parse_json_body(request)?;
            let result = tmux_set_buffer(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Get, "/v1/tmux/show-buffer") => {
            let name = get_query_value(query, "name");
            let text = tmux_show_buffer(state, &context, name.as_deref())?;
            Ok((200, text, "text/plain; charset=utf-8"))
        }
        (&Method::Get, "/v1/tmux/list-buffers") => {
            let format =
                get_query_value(query, "format").unwrap_or_else(|| "#{buffer_name}".to_string());
            let lines = tmux_list_buffers(state, &context, &format)?;
            Ok((200, lines.join("\n"), "text/plain; charset=utf-8"))
        }
        (&Method::Post, "/v1/tmux/delete-buffer") => {
            let payload: DeleteBufferRequest = parse_json_body(request)?;
            tmux_delete_buffer(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/load-buffer") => {
            let payload: LoadBufferRequest = parse_json_body(request)?;
            let result = tmux_load_buffer(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/save-buffer") => {
            let payload: SaveBufferRequest = parse_json_body(request)?;
            tmux_save_buffer(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/paste-buffer") => {
            let payload: PasteBufferRequest = parse_json_body(request)?;
            tmux_paste_buffer(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/wait-for") => {
            let payload: WaitForRequest = parse_json_body(request)?;
            tmux_wait_for(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/rename-window") => {
            let payload: RenameWindowRequest = parse_json_body(request)?;
            tmux_rename_window(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/kill-pane") => {
            let payload: KillPaneRequest = parse_json_body(request)?;
            tmux_kill_pane(state, &context, payload.target.as_deref())?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/kill-window") => {
            let payload: KillWindowRequest = parse_json_body(request)?;
            tmux_kill_window(state, &context, payload.target.as_deref())?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/select-pane") => {
            let payload: SelectPaneRequest = parse_json_body(request)?;
            tmux_select_pane(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/select-window") => {
            let payload: SelectWindowRequest = parse_json_body(request)?;
            tmux_select_window(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/respawn-pane") => {
            let payload: RespawnPaneRequest = parse_json_body(request)?;
            let result = tmux_respawn_pane(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/respawn-window") => {
            let payload: RespawnWindowRequest = parse_json_body(request)?;
            let result = tmux_respawn_window(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/break-pane") => {
            let payload: BreakPaneRequest = parse_json_body(request)?;
            let result = tmux_break_pane(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/join-pane") | (&Method::Post, "/v1/tmux/move-pane") => {
            let payload: JoinPaneRequest = parse_json_body(request)?;
            let result = tmux_join_pane(state, &context, payload)?;
            Ok((
                200,
                serde_json::to_string(&result).map_err(internal_error)?,
                "application/json",
            ))
        }
        (&Method::Post, "/v1/tmux/swap-pane") => {
            let payload: SwapPaneRequest = parse_json_body(request)?;
            tmux_swap_pane(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/swap-window") => {
            let payload: SwapWindowRequest = parse_json_body(request)?;
            tmux_swap_window(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/rotate-window") => {
            let payload: RotateWindowRequest = parse_json_body(request)?;
            tmux_rotate_window(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/move-window") => {
            let payload: MoveWindowRequest = parse_json_body(request)?;
            tmux_move_window(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/pipe-pane") => {
            let payload: PipePaneRequest = parse_json_body(request)?;
            tmux_pipe_pane(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/resize-pane") => {
            let payload: ResizePaneRequest = parse_json_body(request)?;
            tmux_resize_pane(state, &context, payload)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
        }
        (&Method::Post, "/v1/tmux/capture-pane") => {
            let payload: CapturePaneRequest = parse_json_body(request)?;
            let text = tmux_capture_pane(state, &context, payload)?;
            Ok((200, text, "text/plain; charset=utf-8"))
        }
        (&Method::Get, "/v1/tmux/display-message") => {
            let format =
                get_query_value(query, "format").unwrap_or_else(|| "#{pane_id}".to_string());
            let text = tmux_display_message(state, &context, &format)?;
            Ok((200, text, "text/plain; charset=utf-8"))
        }
        (&Method::Post, "/v1/tmux/refresh-client") => {
            let _ = normalize_tmux_context(state, &context)?;
            Ok((200, "{\"ok\":true}".to_string(), "application/json"))
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

fn normalize_tmux_context(
    state: &AppState,
    context: &TmuxTokenContext,
) -> Result<TmuxTokenContext, TmuxHttpError> {
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    )
    .map_err(internal_error)?;
    let session_alive = snapshot.sessions.iter().any(|session| {
        session.id == context.session_id
            && matches!(
                session.status,
                SessionStatus::Running | SessionStatus::Starting
            )
    });
    if !session_alive {
        return Err(TmuxHttpError {
            status: 401,
            message: "Session is no longer active".to_string(),
        });
    }

    let current_tab = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == context.current_tab_id)
        .or_else(|| snapshot.tabs.iter().find(|tab| tab.id == context.origin_tab_id))
        .or_else(|| snapshot.tabs.first());
    let Some(tab) = current_tab else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Caller tab not found".to_string(),
        });
    };

    let current_pane_id = if stack_exists(&tab.root, &context.current_pane_id) {
        context.current_pane_id.clone()
    } else if stack_exists(&tab.root, &context.origin_pane_id) {
        context.origin_pane_id.clone()
    } else {
        first_stack_id(&tab.root).ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Caller pane not found".to_string(),
        })?
    };

    if tab.id != context.current_tab_id || current_pane_id != context.current_pane_id {
        state
            .tmux
            .update_session_focus(&context.session_id, tab.id.clone(), current_pane_id.clone())
            .map_err(internal_error)?;
    }

    Ok(TmuxTokenContext {
        session_id: context.session_id.clone(),
        project_id: context.project_id.clone(),
        workspace_session_id: context.workspace_session_id.clone(),
        origin_tab_id: context.origin_tab_id.clone(),
        origin_pane_id: context.origin_pane_id.clone(),
        previous_tab_id: context
            .previous_tab_id
            .as_ref()
            .filter(|tab_id| snapshot.tabs.iter().any(|tab| tab.id == tab_id.as_str()))
            .cloned(),
        previous_pane_id: context
            .previous_pane_id
            .as_ref()
            .filter(|pane_id| stack_exists(&tab.root, pane_id))
            .cloned(),
        current_tab_id: tab.id.clone(),
        current_pane_id,
        launch_profile: context.launch_profile.clone(),
    })
}

fn load_workspace_session_record(
    state: &AppState,
    project_id: &str,
    workspace_session_id: &str,
) -> Result<WorkspaceSession, String> {
    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    db.get_workspace_session(project_id, workspace_session_id)?
        .ok_or_else(|| "Workspace session not found".to_string())
}

fn tmux_session_name(
    state: &AppState,
    project_id: &str,
    workspace_session_id: &str,
) -> Result<String, String> {
    let workspace_session = load_workspace_session_record(state, project_id, workspace_session_id)?;
    state
        .tmux
        .session_name(workspace_session_id, &workspace_session.name)
}

fn extract_session_target_token(target: &str) -> String {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    trimmed
        .split_once(':')
        .map(|(head, _)| head)
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn resolve_workspace_session_target(
    state: &AppState,
    context: &TmuxTokenContext,
    target: Option<&str>,
) -> Result<WorkspaceSession, TmuxHttpError> {
    let token = target
        .map(extract_session_target_token)
        .unwrap_or_else(|| context.workspace_session_id.clone());
    if token.trim().is_empty() {
        return load_workspace_session_record(state, &context.project_id, &context.workspace_session_id)
            .map_err(internal_error);
    }

    let normalized = token
        .trim()
        .trim_start_matches('=')
        .trim_start_matches('$');

    let db = state.db.lock().map_err(internal_error)?;
    let sessions = db
        .list_workspace_sessions(&context.project_id)
        .map_err(internal_error)?;
    drop(db);

    if let Some(workspace_session) = sessions.iter().find(|entry| {
        entry.id == normalized || entry.name == normalized || entry.id == token || entry.name == token
    }) {
        return Ok(workspace_session.clone());
    }

    if let Ok(index) = normalized.parse::<usize>() {
        if let Some(workspace_session) = sessions.get(index) {
            return Ok(workspace_session.clone());
        }
        if index > 0 {
            if let Some(workspace_session) = sessions.get(index - 1) {
                return Ok(workspace_session.clone());
            }
        }
    }

    Err(TmuxHttpError {
        status: 404,
        message: format!("Target session '{}' not found in current project", token),
    })
}

fn ensure_tmux_session_runtime_target(
    state: &AppState,
    project_id: &str,
    workspace_session_id: &str,
    window_name: Option<String>,
    launch_profile: LaunchProfile,
) -> Result<(WorkspaceSnapshot, String, String), String> {
    let mut snapshot = load_workspace_session_snapshot(state, project_id, workspace_session_id)?;
    if snapshot.tabs.is_empty() {
        let window = new_workspace_tab(window_name.unwrap_or_else(|| "main".to_string()));
        snapshot.active_tab_id = Some(window.id.clone());
        snapshot.tabs.push(window);
        snapshot = persist_workspace_snapshot(state, snapshot)?;
    }

    let window_id = snapshot
        .active_tab_id
        .clone()
        .or_else(|| snapshot.tabs.first().map(|tab| tab.id.clone()))
        .ok_or_else(|| "Window not found".to_string())?;
    let tab = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == window_id)
        .ok_or_else(|| "Window not found".to_string())?;
    let fallback_pane_id =
        first_stack_id(&tab.root).ok_or_else(|| "Pane not found in target window".to_string())?;
    let pane_id = current_tmux_pane_id(tab, &fallback_pane_id);

    let needs_spawn = !session_record_for_pane(&snapshot, &window_id, &pane_id)
        .map(|session| matches!(session.status, SessionStatus::Starting | SessionStatus::Running))
        .unwrap_or(false);

    if needs_spawn {
        let child_launch = build_tmux_child_launch_spec(state, None)?;
        let (next_snapshot, _) = spawn_session_in_stack(
            state.tmux.app_handle.clone(),
            state,
            SessionSpawnRequest {
                project_id: project_id.to_string(),
                workspace_session_id: workspace_session_id.to_string(),
                tab_id: window_id.clone(),
                stack_id: pane_id.clone(),
                title: Some("AI Terminal".to_string()),
                program: Some(child_launch.program),
                args: child_launch.args,
                cwd: None,
                launch_profile,
                env_overrides: Some(child_launch.env_overrides),
                shell_kind: child_launch.shell_kind,
            },
        )?;
        snapshot = next_snapshot;
    }

    Ok((snapshot, window_id, pane_id))
}

fn build_tmux_session_response(
    state: &AppState,
    workspace_session: &WorkspaceSession,
    snapshot: &WorkspaceSnapshot,
    window_id: &str,
    pane_id: &str,
    attached: bool,
) -> Result<serde_json::Value, TmuxHttpError> {
    let session_name =
        tmux_session_name(state, &workspace_session.project_id, &workspace_session.id)
            .map_err(internal_error)?;
    let window_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == window_id)
        .unwrap_or(0);
    let window_name = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == window_id)
        .map(|tab| tab.title.clone())
        .unwrap_or_else(|| "window".to_string());
    let pane_index = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == window_id)
        .map(|tab| {
            let mut panes = Vec::new();
            collect_tab_panes(
                &tab.root,
                &snapshot.sessions,
                window_index,
                window_id,
                &session_name,
                &mut panes,
            );
            panes
                .iter()
                .position(|pane| pane.pane_id == pane_id)
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let session = session_record_for_pane(snapshot, window_id, pane_id);
    let pane_title = session
        .as_ref()
        .map(|entry| entry.title.clone())
        .unwrap_or_else(|| "pane".to_string());
    let pane_current_command = session
        .as_ref()
        .map(|entry| entry.program.clone())
        .unwrap_or_else(|| "terminal".to_string());

    Ok(serde_json::json!({
        "sessionId": format!("${}", workspace_session.id),
        "sessionName": session_name,
        "sessionWindows": snapshot.tabs.len(),
        "sessionAttached": attached,
        "windowId": window_id,
        "windowIndex": window_index,
        "windowName": window_name,
        "paneId": pane_id,
        "paneIndex": pane_index,
        "paneTitle": pane_title,
        "paneCurrentCommand": pane_current_command
    }))
}

fn tmux_has_session(
    state: &AppState,
    context: &TmuxTokenContext,
    target: Option<&str>,
) -> Result<WorkspaceSession, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    resolve_workspace_session_target(state, &context, target)
}

fn tmux_new_session(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: NewSessionRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    if payload.target.as_deref().is_some_and(|target| !target.trim().is_empty()) {
        return Err(TmuxHttpError {
            status: 400,
            message: "tmux session grouping (-t for new-session) is not supported".to_string(),
        });
    }

    let workspace_session = {
        let db = state.db.lock().map_err(internal_error)?;
        db.create_workspace_session(
            &context.project_id,
            payload.name.clone(),
            WorkspaceSessionCreatedBy::Ai,
            Some(context.workspace_session_id.clone()),
        )
        .map_err(internal_error)?
    };

    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &workspace_session.id,
    )
    .map_err(internal_error)?;
    let window = new_workspace_tab(
        payload
            .window_name
            .clone()
            .unwrap_or_else(|| "main".to_string()),
    );
    let window_id = window.id.clone();
    let pane_id = first_stack_id(&window.root).ok_or_else(|| TmuxHttpError {
        status: 500,
        message: "Failed to create initial pane".to_string(),
    })?;
    snapshot.active_tab_id = Some(window_id.clone());
    snapshot.tabs.push(window);
    let _ = persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;

    let child_launch =
        build_tmux_child_launch_spec(state, payload.command.clone()).map_err(internal_error)?;
    let child_title = if payload.command.is_some() {
        payload
            .window_name
            .clone()
            .unwrap_or_else(|| "tmux-command".to_string())
    } else {
        "AI Terminal".to_string()
    };
    let (snapshot, _) = spawn_session_in_stack(
        state.tmux.app_handle.clone(),
        state,
        SessionSpawnRequest {
            project_id: context.project_id.clone(),
            workspace_session_id: workspace_session.id.clone(),
            tab_id: window_id.clone(),
            stack_id: pane_id.clone(),
            title: Some(child_title),
            program: Some(child_launch.program),
            args: child_launch.args,
            cwd: payload.cwd.clone(),
            launch_profile: context.launch_profile.clone(),
            env_overrides: Some(child_launch.env_overrides),
            shell_kind: child_launch.shell_kind,
        },
    )
    .map_err(internal_error)?;

    {
        let db = state.db.lock().map_err(internal_error)?;
        db.touch_project(&context.project_id).map_err(internal_error)?;
        db.touch_workspace_session(&context.project_id, &workspace_session.id)
            .map_err(internal_error)?;
    }

    if !payload.detached {
        state
            .tmux
            .switch_workspace_session(
                &context.session_id,
                workspace_session.id.clone(),
                window_id.clone(),
                pane_id.clone(),
            )
            .map_err(internal_error)?;
    }

    emit_workspace_session_changed(&state.tmux.app_handle, &context.project_id, None);

    build_tmux_session_response(
        state,
        &workspace_session,
        &snapshot,
        &window_id,
        &pane_id,
        !payload.detached,
    )
}

fn tmux_attach_session(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: AttachSessionRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let workspace_session =
        resolve_workspace_session_target(state, &context, payload.target.as_deref())?;
    let (snapshot, window_id, pane_id) = ensure_tmux_session_runtime_target(
        state,
        &context.project_id,
        &workspace_session.id,
        Some("main".to_string()),
        context.launch_profile.clone(),
    )
    .map_err(internal_error)?;

    {
        let db = state.db.lock().map_err(internal_error)?;
        db.touch_project(&context.project_id).map_err(internal_error)?;
        db.touch_workspace_session(&context.project_id, &workspace_session.id)
            .map_err(internal_error)?;
    }

    state
        .tmux
        .switch_workspace_session(
            &context.session_id,
            workspace_session.id.clone(),
            window_id.clone(),
            pane_id.clone(),
        )
        .map_err(internal_error)?;

    emit_workspace_session_changed(
        &state.tmux.app_handle,
        &context.project_id,
        Some(workspace_session.id.clone()),
    );

    build_tmux_session_response(state, &workspace_session, &snapshot, &window_id, &pane_id, true)
}

fn tmux_switch_client(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SwitchClientRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    tmux_attach_session(
        state,
        context,
        AttachSessionRequest {
            target: payload.target,
        },
    )
}

fn tmux_kill_session(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: KillSessionRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let workspace_session =
        resolve_workspace_session_target(state, &context, payload.target.as_deref())?;
    let session_ids = {
        let db = state.db.lock().map_err(internal_error)?;
        db.list_sessions_for_workspace_session(&workspace_session.id)
            .map_err(internal_error)?
            .into_iter()
            .map(|session| session.id)
            .collect::<Vec<_>>()
    };

    for session_id in &session_ids {
        terminate_if_running(state, session_id).map_err(internal_error)?;
    }

    state
        .workspaces
        .lock()
        .map_err(internal_error)?
        .remove(&workspace_session.id);
    state
        .tmux
        .remove_session_name(&workspace_session.id)
        .map_err(internal_error)?;

    let remaining_sessions = {
        let mut db = state.db.lock().map_err(internal_error)?;
        db.delete_workspace_session(&context.project_id, &workspace_session.id)
            .map_err(internal_error)?;
        db.list_workspace_sessions(&context.project_id)
            .map_err(internal_error)?
    };

    if workspace_session.id == context.workspace_session_id
        && !session_ids.iter().any(|session_id| session_id == &context.session_id)
    {
        if let Some(next_session) = remaining_sessions.first() {
            let (_, window_id, pane_id) = ensure_tmux_session_runtime_target(
                state,
                &context.project_id,
                &next_session.id,
                Some("main".to_string()),
                context.launch_profile.clone(),
            )
            .map_err(internal_error)?;
            state
                .tmux
                .switch_workspace_session(
                    &context.session_id,
                    next_session.id.clone(),
                    window_id,
                    pane_id,
                )
                .map_err(internal_error)?;
        }
    }

    emit_workspace_session_changed(&state.tmux.app_handle, &context.project_id, None);
    Ok(())
}

fn tmux_split_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SplitWindowRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;

    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    )
    .map_err(internal_error)?;
    let Some(tab) = snapshot
        .tabs
        .iter_mut()
        .find(|tab| tab.id == context.current_tab_id)
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Caller tab not found".to_string(),
        });
    };
    let current_pane_id = current_tmux_pane_id(tab, &context.current_pane_id);
    let target_pane_id =
        resolve_target_pane_id(tab, payload.target.as_deref(), &current_pane_id)?;
    let direction = resolve_tmux_split_direction(payload.direction.as_deref());
    let insertion = if payload.before {
        SplitInsertion::Before
    } else {
        SplitInsertion::After
    };
    let size_percentage = resolve_tmux_split_size_percentage(
        state,
        &context.project_id,
        &context.current_tab_id,
        tab,
        payload.size,
        payload.size_is_percentage,
        &direction,
        &target_pane_id,
        payload.full_span,
    );
    let new_pane_id = split_tmux_pane(
        tab,
        &target_pane_id,
        &direction,
        payload.full_span,
        insertion,
        size_percentage,
    )?;
    if !payload.detached {
        tab.active_pane_id = Some(new_pane_id.clone());
        state
            .tmux
            .update_session_focus(&context.session_id, context.current_tab_id.clone(), new_pane_id.clone())
            .map_err(internal_error)?;
    }

    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;

    let child_launch =
        build_tmux_child_launch_spec(state, payload.command.clone()).map_err(internal_error)?;
    let child_env = merge_env_maps(Some(child_launch.env_overrides), payload.env.clone());
    let (_snapshot, session) = spawn_session_in_stack(
        state.tmux.app_handle.clone(),
        state,
        SessionSpawnRequest {
            project_id: context.project_id.clone(),
            workspace_session_id: context.workspace_session_id.clone(),
            tab_id: context.current_tab_id.clone(),
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
            env_overrides: child_env,
            shell_kind: child_launch.shell_kind,
        },
    )
    .map_err(internal_error)?;

    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);

    let listing = tmux_pane_listing(state, &context, &new_pane_id).map_err(internal_error)?;

    Ok(serde_json::json!({
        "paneId": new_pane_id,
        "sessionId": session.id,
        "direction": direction,
        "paneIndex": listing.pane_index,
        "paneTitle": listing.pane_title,
        "paneCurrentCommand": listing.pane_current_command,
        "windowIndex": listing.window_index,
        "sessionName": listing.session_name,
        "windowId": listing.window_id
    }))
}

fn tmux_new_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: NewWindowRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;

    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    )
    .map_err(internal_error)?;
    let anchor_window_id = resolve_target_window_id(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        context.previous_tab_id.as_deref(),
    )?;
    let insert_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == anchor_window_id)
        .map(|index| index + 1)
        .unwrap_or(snapshot.tabs.len());
    let mut tab = new_workspace_tab(
        payload
            .name
            .clone()
            .unwrap_or_else(|| format!("tmux-{}", now_iso())),
    );
    tab.root = new_stack_node(1, PaneCreatedBy::Ai, Some(context.current_pane_id.clone()));
    tab.next_pane_ordinal = 2;
    tab.active_pane_id = first_stack_id(&tab.root);
    let new_tab_id = tab.id.clone();
    let new_pane_id = tab
        .active_pane_id
        .clone()
        .unwrap_or_else(|| context.current_pane_id.clone());

    snapshot.tabs.insert(insert_index, tab);
    if !payload.detached {
        snapshot.active_tab_id = Some(new_tab_id.clone());
        state
            .tmux
            .update_session_focus(&context.session_id, new_tab_id.clone(), new_pane_id.clone())
            .map_err(internal_error)?;
    }
    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;

    let child_launch =
        build_tmux_child_launch_spec(state, payload.command.clone()).map_err(internal_error)?;
    let (_snapshot, session) = spawn_session_in_stack(
        state.tmux.app_handle.clone(),
        state,
        SessionSpawnRequest {
            project_id: context.project_id.clone(),
            workspace_session_id: context.workspace_session_id.clone(),
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
        "sessionId": session.id,
        "windowId": new_tab_id,
        "windowIndex": insert_index,
        "sessionName": tmux_session_name(
            state,
            &context.project_id,
            &context.workspace_session_id,
        )
        .map_err(internal_error)?
    }))
}

fn tmux_send_keys(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SendKeysRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    )
    .map_err(internal_error)?;
    let Some(tab) = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == context.current_tab_id)
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Caller tab not found".to_string(),
        });
    };
    let target_pane =
        resolve_target_pane_id(tab, payload.target.as_deref(), &context.current_pane_id)?;
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
    let context = normalize_tmux_context(state, context)?;
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    )
    .map_err(internal_error)?;
    let session_name = tmux_session_name(state, &context.project_id, &context.workspace_session_id)
        .map_err(internal_error)?;
    let tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == context.current_tab_id)
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
        &context.current_tab_id,
        &session_name,
        &mut panes,
    );
    let lines = panes
        .iter()
        .map(|pane| render_tmux_format(format, pane))
        .collect::<Vec<_>>();
    Ok(lines)
}

fn tmux_list_windows(
    state: &AppState,
    context: &TmuxTokenContext,
    format: &str,
) -> Result<Vec<String>, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    )
    .map_err(internal_error)?;
    let session_name = tmux_session_name(state, &context.project_id, &context.workspace_session_id)
        .map_err(internal_error)?;
    let listings = snapshot
        .tabs
        .iter()
        .enumerate()
        .map(|(window_index, tab)| WindowListing {
            window_id: tab.id.clone(),
            window_index,
            window_name: tab.title.clone(),
            window_active: snapshot.active_tab_id.as_deref() == Some(tab.id.as_str()),
            session_name: session_name.clone(),
        })
        .collect::<Vec<_>>();
    Ok(listings
        .iter()
        .map(|window| render_tmux_window_format(format, window))
        .collect())
}

fn tmux_list_sessions(
    state: &AppState,
    context: &TmuxTokenContext,
    format: &str,
) -> Result<Vec<String>, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let db = state
        .db
        .lock()
        .map_err(internal_error)?;
    let workspace_sessions = db
        .list_workspace_sessions(&context.project_id)
        .map_err(internal_error)?;
    drop(db);

    Ok(workspace_sessions
        .into_iter()
        .map(|workspace_session| {
            let runtime_snapshot = load_workspace_session_snapshot(
                state,
                &context.project_id,
                &workspace_session.id,
            )
            .map_err(internal_error)?;
            let session_name = state
                .tmux
                .session_name(&workspace_session.id, &workspace_session.name)
                .map_err(internal_error)?;
            Ok(render_tmux_session_format(
                format,
                &SessionListing {
                    session_id: format!("${}", workspace_session.id),
                    session_name,
                    session_windows: runtime_snapshot.tabs.len(),
                    session_attached: workspace_session.id == context.workspace_session_id,
                },
            ))
        })
        .collect::<Result<Vec<_>, TmuxHttpError>>()?)
}

fn tmux_list_clients(
    state: &AppState,
    context: &TmuxTokenContext,
    format: &str,
) -> Result<Vec<String>, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let session_name = tmux_session_name(state, &context.project_id, &context.workspace_session_id)
        .map_err(internal_error)?;
    let listing = ClientListing {
        client_name: "workspace-terminal-client".to_string(),
        client_pid: std::process::id(),
        client_tty: "workspace-terminal".to_string(),
        session_name,
        window_id: context.current_tab_id.clone(),
        pane_id: context.current_pane_id.clone(),
    };
    Ok(vec![render_tmux_client_format(format, &listing)])
}

fn tmux_rename_session(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: RenameSessionRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let name = payload.name.trim();
    if name.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "Session name is required".to_string(),
        });
    }
    let workspace_session =
        resolve_workspace_session_target(state, &context, payload.target.as_deref())?;
    {
        let db = state.db.lock().map_err(internal_error)?;
        db.rename_workspace_session(&context.project_id, &workspace_session.id, name)
            .map_err(internal_error)?;
    }
    state
        .tmux
        .rename_session(&workspace_session.id, name.to_string())
        .map_err(internal_error)?;
    emit_workspace_session_changed(
        &state.tmux.app_handle,
        &context.project_id,
        Some(workspace_session.id),
    );
    Ok(())
}

fn tmux_set_environment(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SetEnvironmentRequest,
) -> Result<(), TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    let key = payload.name.trim();
    if key.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "Environment variable name is required".to_string(),
        });
    }
    state
        .tmux
        .set_environment(key, payload.value, payload.unset)
        .map_err(internal_error)?;
    Ok(())
}

fn tmux_show_environment(
    state: &AppState,
    context: &TmuxTokenContext,
    key: Option<&str>,
    value_only: bool,
) -> Result<Vec<String>, TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    if let Some(key) = key.map(str::trim).filter(|key| !key.is_empty()) {
        let Some(value) = state.tmux.environment_value(key).map_err(internal_error)? else {
            return Err(TmuxHttpError {
                status: 404,
                message: format!("Environment variable '{key}' not found"),
            });
        };
        return Ok(vec![if value_only {
            value
        } else {
            format!("{key}={value}")
        }]);
    }

    Ok(state
        .tmux
        .environment_entries()
        .map_err(internal_error)?
        .into_iter()
        .map(|(entry_key, entry_value)| {
            if value_only {
                entry_value
            } else {
                format!("{entry_key}={entry_value}")
            }
        })
        .collect())
}

fn tmux_set_option(
    state: &AppState,
    context: &TmuxTokenContext,
    scope: TmuxOptionScope,
    payload: SetOptionRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let key = payload.key.trim();
    if key.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "Option name is required".to_string(),
        });
    }

    let target_tab_id = if matches!(scope, TmuxOptionScope::Window) {
        Some(resolve_target_window_id(
            &load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?,
            payload.target.as_deref(),
            &context.current_tab_id,
            context.previous_tab_id.as_deref(),
        )?)
    } else {
        None
    };

    state
        .tmux
        .set_option(
            scope,
            &context.project_id,
            target_tab_id.as_deref(),
            key,
            payload.value,
            payload.unset,
            payload.append,
            payload.only_if_unset,
            payload.global,
        )
        .map_err(internal_error)?;
    Ok(())
}

fn tmux_show_options(
    state: &AppState,
    context: &TmuxTokenContext,
    scope: TmuxOptionScope,
    target: Option<&str>,
    key: Option<&str>,
    global_only: bool,
    value_only: bool,
) -> Result<Vec<String>, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let target_tab_id = if matches!(scope, TmuxOptionScope::Window) {
        Some(resolve_target_window_id(
            &load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?,
            target,
            &context.current_tab_id,
            context.previous_tab_id.as_deref(),
        )?)
    } else {
        None
    };

    if let Some(key) = key.map(str::trim).filter(|key| !key.is_empty()) {
        let Some(value) = state
            .tmux
            .option_value(
                scope,
                &context.project_id,
                target_tab_id.as_deref(),
                key,
                global_only,
            )
            .map_err(internal_error)?
        else {
            return Err(TmuxHttpError {
                status: 404,
                message: format!("Option '{key}' not found"),
            });
        };
        return Ok(vec![if value_only {
            value
        } else {
            format!("{key} {value}")
        }]);
    }

    Ok(state
        .tmux
        .option_entries(scope, &context.project_id, target_tab_id.as_deref(), global_only)
        .map_err(internal_error)?
        .into_iter()
        .map(|(entry_key, entry_value)| {
            if value_only {
                entry_value
            } else {
                format!("{entry_key} {entry_value}")
            }
        })
        .collect())
}

fn tmux_bind_key(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: BindKeyRequest,
) -> Result<(), TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    let key = payload.key.trim();
    let command = payload.command.as_deref().unwrap_or("").trim();
    if key.is_empty() || command.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "bind-key requires a key and command".to_string(),
        });
    }
    state
        .tmux
        .set_key_binding(&tmux_binding_key(payload.table.as_deref(), key), Some(command.to_string()))
        .map_err(internal_error)?;
    Ok(())
}

fn tmux_unbind_key(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: BindKeyRequest,
) -> Result<(), TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    let key = payload.key.trim();
    if key.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "unbind-key requires a key".to_string(),
        });
    }
    state
        .tmux
        .set_key_binding(&tmux_binding_key(payload.table.as_deref(), key), None)
        .map_err(internal_error)?;
    Ok(())
}

fn tmux_list_keys(
    state: &AppState,
    context: &TmuxTokenContext,
    table: Option<&str>,
) -> Result<Vec<String>, TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    Ok(state
        .tmux
        .key_binding_entries(table)
        .map_err(internal_error)?
        .into_iter()
        .map(|(entry_key, command)| render_tmux_key_binding_line(&entry_key, &command))
        .collect())
}

fn tmux_set_hook(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SetHookRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let hook_name = payload.name.trim();
    if hook_name.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "Hook name is required".to_string(),
        });
    }

    let hook_key = tmux_hook_key(
        &context.project_id,
        &context.workspace_session_id,
        payload.target.as_deref(),
        payload.global,
        hook_name,
    );
    let command = if payload.unset {
        None
    } else {
        Some(
            payload
                .command
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_string(),
        )
    };
    if command.as_deref().is_some_and(str::is_empty) {
        return Err(TmuxHttpError {
            status: 400,
            message: "Hook command is required".to_string(),
        });
    }
    state
        .tmux
        .set_hook(&hook_key, command)
        .map_err(internal_error)?;
    Ok(())
}

fn tmux_show_hooks(
    state: &AppState,
    context: &TmuxTokenContext,
    target: Option<&str>,
    global_only: bool,
) -> Result<Vec<String>, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let prefix = if global_only {
        Some("global::".to_string())
    } else if let Some(target) = target.filter(|entry| !entry.trim().is_empty()) {
        Some(format!("target:{}::", target.trim()))
    } else {
        Some(format!("session:{}::", context.workspace_session_id))
    };
    Ok(state
        .tmux
        .hook_entries(prefix.as_deref())
        .map_err(internal_error)?
        .into_iter()
        .map(|(key, command)| render_tmux_hook_line(&key, &command))
        .collect())
}

fn tmux_set_buffer(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SetBufferRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    let buffer_name = state
        .tmux
        .set_buffer(payload.name, payload.value, payload.append)
        .map_err(internal_error)?;
    Ok(serde_json::json!({ "bufferName": buffer_name }))
}

fn tmux_show_buffer(
    state: &AppState,
    context: &TmuxTokenContext,
    name: Option<&str>,
) -> Result<String, TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    let Some((_, value)) = state
        .tmux
        .buffer_value(name)
        .map_err(internal_error)?
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Buffer not found".to_string(),
        });
    };
    Ok(value)
}

fn tmux_list_buffers(
    state: &AppState,
    context: &TmuxTokenContext,
    format: &str,
) -> Result<Vec<String>, TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    Ok(state
        .tmux
        .buffer_entries()
        .map_err(internal_error)?
        .into_iter()
        .map(|(name, value)| render_tmux_buffer_format(format, &name, &value))
        .collect())
}

fn tmux_delete_buffer(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: DeleteBufferRequest,
) -> Result<(), TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    state
        .tmux
        .delete_buffer(payload.name.as_deref(), payload.delete_all)
        .map_err(internal_error)?;
    Ok(())
}

fn tmux_load_buffer(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: LoadBufferRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    let path = payload.path.trim();
    if path.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "Buffer file path is required".to_string(),
        });
    }
    let content = fs::read_to_string(path).map_err(|err| TmuxHttpError {
        status: 400,
        message: format!("Failed to read buffer file: {err}"),
    })?;
    let buffer_name = state
        .tmux
        .set_buffer(payload.name, content, false)
        .map_err(internal_error)?;
    Ok(serde_json::json!({ "bufferName": buffer_name }))
}

fn tmux_save_buffer(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SaveBufferRequest,
) -> Result<(), TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    let path = payload.path.trim();
    if path.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "Buffer file path is required".to_string(),
        });
    }
    let Some((_, value)) = state
        .tmux
        .buffer_value(payload.name.as_deref())
        .map_err(internal_error)?
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Buffer not found".to_string(),
        });
    };
    fs::write(path, value).map_err(|err| TmuxHttpError {
        status: 400,
        message: format!("Failed to save buffer file: {err}"),
    })?;
    Ok(())
}

fn tmux_paste_buffer(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: PasteBufferRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let Some((buffer_name, value)) = state
        .tmux
        .buffer_value(payload.name.as_deref())
        .map_err(internal_error)?
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Buffer not found".to_string(),
        });
    };
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    )
    .map_err(internal_error)?;
    let (tab_id, pane_id) = resolve_target_pane_ref(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        &context.current_pane_id,
        context.previous_tab_id.as_deref(),
    )?;
    let tab = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == tab_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target window not found".to_string(),
        })?;
    let Some(target_session_id) = active_session_for_pane(&tab.root, &pane_id) else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Target pane session not found".to_string(),
        });
    };
    state
        .sessions
        .ensure_tmux_child_ready(&target_session_id)
        .map_err(internal_error)?;
    state
        .sessions
        .write_input(&target_session_id, &value)
        .map_err(internal_error)?;
    if payload.delete_after {
        state
            .tmux
            .delete_buffer(Some(&buffer_name), false)
            .map_err(internal_error)?;
    }
    Ok(())
}

fn tmux_wait_for(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: WaitForRequest,
) -> Result<(), TmuxHttpError> {
    let _ = normalize_tmux_context(state, context)?;
    let channel = payload.name.trim();
    if channel.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "wait-for requires a channel name".to_string(),
        });
    }
    match payload.mode.as_deref() {
        Some("signal") => state.tmux.signal_wait_for(channel).map_err(internal_error),
        Some("lock") => state.tmux.lock_wait_for(channel).map_err(internal_error),
        Some("unlock") => state.tmux.unlock_wait_for(channel).map_err(internal_error),
        _ => state.tmux.wait_for(channel).map_err(internal_error),
    }
}

fn tmux_rename_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: RenameWindowRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    if payload.name.trim().is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "Window name is required".to_string(),
        });
    }

    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let target_window_id = resolve_target_window_id(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        context.previous_tab_id.as_deref(),
    )?;
    let tab = snapshot
        .tabs
        .iter_mut()
        .find(|tab| tab.id == target_window_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target window not found".to_string(),
        })?;
    tab.title = payload.name.trim().to_string();

    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);
    Ok(())
}

fn tmux_respawn_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: RespawnPaneRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let (tab_id, pane_id) = resolve_target_pane_ref(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        &context.current_pane_id,
        context.previous_tab_id.as_deref(),
    )?;
    let existing_session = session_record_for_pane(&snapshot, &tab_id, &pane_id);

    if matches!(
        existing_session.as_ref().map(|session| &session.status),
        Some(SessionStatus::Running | SessionStatus::Starting)
    ) && !payload.kill_existing
    {
        return Err(TmuxHttpError {
            status: 409,
            message: "Pane is still running; use -k to respawn".to_string(),
        });
    }

    let launch_profile = existing_session
        .as_ref()
        .map(|session| session.launch_profile.clone())
        .unwrap_or_else(|| context.launch_profile.clone());
    let (program, args, shell_kind, env_overrides, title, cwd) = if let Some(command) = payload.command.clone() {
        let launch_spec = build_tmux_child_launch_spec(state, Some(command)).map_err(internal_error)?;
        (
            Some(launch_spec.program),
            launch_spec.args,
            launch_spec.shell_kind,
            merge_env_maps(Some(launch_spec.env_overrides), payload.env.clone()),
            existing_session
                .as_ref()
                .map(|session| session.title.clone())
                .or_else(|| Some("tmux-respawn".to_string())),
            payload
                .cwd
                .clone()
                .or_else(|| existing_session.as_ref().map(|session| session.cwd.clone())),
        )
    } else if let Some(session) = existing_session.as_ref() {
        (
            Some(session.program.clone()),
            session.args.clone(),
            SessionShellKind::Default,
            payload.env.clone(),
            Some(session.title.clone()),
            payload.cwd.clone().or_else(|| Some(session.cwd.clone())),
        )
    } else {
        return Err(TmuxHttpError {
            status: 400,
            message: "Pane has no prior session to respawn; provide a command".to_string(),
        });
    };

    let (_snapshot, session) = spawn_session_in_stack(
        state.tmux.app_handle.clone(),
        state,
        SessionSpawnRequest {
            project_id: context.project_id.clone(),
            workspace_session_id: context.workspace_session_id.clone(),
            tab_id: tab_id.clone(),
            stack_id: pane_id.clone(),
            title,
            program,
            args,
            cwd,
            launch_profile,
            env_overrides,
            shell_kind,
        },
    )
    .map_err(internal_error)?;

    if let Some(previous) = existing_session {
        let _ = terminate_if_running(state, &previous.id);
    }

    state
        .tmux
        .update_session_focus(&context.session_id, tab_id.clone(), pane_id.clone())
        .map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);

    Ok(serde_json::json!({
        "paneId": pane_id,
        "tabId": tab_id,
        "sessionId": session.id,
    }))
}

fn tmux_respawn_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: RespawnWindowRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let target_window_id = resolve_target_window_id(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        context.previous_tab_id.as_deref(),
    )?;
    let target_tab = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == target_window_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target window not found".to_string(),
        })?;
    let pane_ids = collect_stack_ids(&target_tab.root);
    let mut spawned = Vec::new();

    for pane_id in pane_ids {
        let existing_session = session_record_for_pane(&snapshot, &target_window_id, &pane_id);
        if matches!(
            existing_session.as_ref().map(|session| &session.status),
            Some(SessionStatus::Running | SessionStatus::Starting)
        ) && !payload.kill_existing
        {
            return Err(TmuxHttpError {
                status: 409,
                message: "Window has running panes; use -k to respawn".to_string(),
            });
        }

        let command_override = if target_tab.active_pane_id.as_deref() == Some(pane_id.as_str()) {
            payload.command.clone()
        } else {
            None
        };
        let launch_profile = existing_session
            .as_ref()
            .map(|session| session.launch_profile.clone())
            .unwrap_or_else(|| context.launch_profile.clone());

        let Some((program, args, shell_kind, env_overrides, title, cwd)) = (if let Some(command) = command_override {
            let launch_spec = build_tmux_child_launch_spec(state, Some(command)).map_err(internal_error)?;
            Some((
                Some(launch_spec.program),
                launch_spec.args,
                launch_spec.shell_kind,
                merge_env_maps(Some(launch_spec.env_overrides), payload.env.clone()),
                existing_session
                    .as_ref()
                    .map(|session| session.title.clone())
                    .or_else(|| Some("tmux-respawn".to_string())),
                payload
                    .cwd
                    .clone()
                    .or_else(|| existing_session.as_ref().map(|session| session.cwd.clone())),
            ))
        } else {
            existing_session.as_ref().map(|session| {
                (
                    Some(session.program.clone()),
                    session.args.clone(),
                    SessionShellKind::Default,
                    payload.env.clone(),
                    Some(session.title.clone()),
                    payload.cwd.clone().or_else(|| Some(session.cwd.clone())),
                )
            })
        }) else {
            continue;
        };

        let (_snapshot, session) = spawn_session_in_stack(
            state.tmux.app_handle.clone(),
            state,
            SessionSpawnRequest {
                project_id: context.project_id.clone(),
                workspace_session_id: context.workspace_session_id.clone(),
                tab_id: target_window_id.clone(),
                stack_id: pane_id.clone(),
                title,
                program,
                args,
                cwd,
                launch_profile,
                env_overrides,
                shell_kind,
            },
        )
        .map_err(internal_error)?;
        if let Some(previous) = existing_session {
            let _ = terminate_if_running(state, &previous.id);
        }
        spawned.push(serde_json::json!({
            "paneId": pane_id,
            "sessionId": session.id,
        }));
    }

    if spawned.is_empty() {
        return Err(TmuxHttpError {
            status: 400,
            message: "Window has no panes that can be respawned".to_string(),
        });
    }

    Ok(serde_json::json!({
        "tabId": target_window_id,
        "panes": spawned
    }))
}

fn tmux_break_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: BreakPaneRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let (source_tab_id, source_pane_id) = resolve_target_pane_ref(
        &snapshot,
        payload.source.as_deref(),
        &context.current_tab_id,
        &context.current_pane_id,
        context.previous_tab_id.as_deref(),
    )?;
    let pane_title = session_record_for_pane(&snapshot, &source_tab_id, &source_pane_id)
        .map(|session| session.title)
        .unwrap_or_else(|| "tmux-window".to_string());
    let detached_stack =
        detach_pane_from_snapshot(&mut snapshot, &source_tab_id, &source_pane_id).map_err(internal_error)?;
    let mut new_tab = WorkspaceTab {
        id: Uuid::new_v4().to_string(),
        title: payload.name.unwrap_or(pane_title),
        root: detached_stack,
        next_pane_ordinal: 1,
        active_pane_id: Some(source_pane_id.clone()),
    };
    normalize_tab(&mut new_tab);

    let insert_index = if let Some(target) = payload.target.as_deref() {
        let target_window_id = resolve_target_window_id(
            &snapshot,
            Some(target),
            &context.current_tab_id,
            context.previous_tab_id.as_deref(),
        )?;
        let target_index = snapshot
            .tabs
            .iter()
            .position(|tab| tab.id == target_window_id)
            .unwrap_or(snapshot.tabs.len());
        if payload.before { target_index } else { target_index + 1 }
    } else {
        snapshot.tabs.len()
    }
    .min(snapshot.tabs.len());

    let new_tab_id = new_tab.id.clone();
    snapshot.tabs.insert(insert_index, new_tab);
    if !payload.detached {
        snapshot.active_tab_id = Some(new_tab_id.clone());
        state
            .tmux
            .update_session_focus(&context.session_id, new_tab_id.clone(), source_pane_id.clone())
            .map_err(internal_error)?;
    }

    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);

    Ok(serde_json::json!({
        "tabId": new_tab_id,
        "paneId": source_pane_id
    }))
}

fn tmux_join_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: JoinPaneRequest,
) -> Result<serde_json::Value, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let (source_tab_id, source_pane_id) = resolve_target_pane_ref(
        &snapshot,
        payload.source.as_deref(),
        &context.current_tab_id,
        &context.current_pane_id,
        context.previous_tab_id.as_deref(),
    )?;
    let (target_tab_id, target_pane_id) = resolve_target_pane_ref(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        &context.current_pane_id,
        context.previous_tab_id.as_deref(),
    )?;
    if source_tab_id == target_tab_id && source_pane_id == target_pane_id {
        return Err(TmuxHttpError {
            status: 400,
            message: "Source and target panes must be different".to_string(),
        });
    }

    let detached_stack =
        detach_pane_from_snapshot(&mut snapshot, &source_tab_id, &source_pane_id).map_err(internal_error)?;
    let direction = resolve_tmux_split_direction(payload.direction.as_deref());
    let insertion = if payload.before {
        SplitInsertion::Before
    } else {
        SplitInsertion::After
    };
    let target_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == target_tab_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target window not found".to_string(),
        })?;
    let target_tab = snapshot
        .tabs
        .get_mut(target_index)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target window not found".to_string(),
        })?;
    let size_percentage = resolve_tmux_split_size_percentage(
        state,
        &context.project_id,
        &target_tab_id,
        target_tab,
        payload.size,
        payload.size_is_percentage,
        &direction,
        &target_pane_id,
        false,
    );
    let mut detached_stack_opt = Some(detached_stack);
    if !insert_existing_stack_node(
        &mut target_tab.root,
        &target_pane_id,
        &direction,
        insertion,
        size_percentage,
        &mut detached_stack_opt,
    ) {
        return Err(TmuxHttpError {
            status: 404,
            message: "Target pane not found".to_string(),
        });
    }
    target_tab.active_pane_id = Some(source_pane_id.clone());
    normalize_tab(target_tab);

    if !payload.detached {
        snapshot.active_tab_id = Some(target_tab_id.clone());
        state
            .tmux
            .update_session_focus(&context.session_id, target_tab_id.clone(), source_pane_id.clone())
            .map_err(internal_error)?;
    }

    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);

    Ok(serde_json::json!({
        "tabId": target_tab_id,
        "paneId": source_pane_id
    }))
}

fn tmux_swap_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SwapPaneRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let (source_tab_id, source_pane_id) = resolve_target_pane_ref(
        &snapshot,
        payload.source.as_deref(),
        &context.current_tab_id,
        &context.current_pane_id,
        context.previous_tab_id.as_deref(),
    )?;
    let (target_tab_id, target_pane_id) = resolve_target_pane_ref(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        &context.current_pane_id,
        context.previous_tab_id.as_deref(),
    )?;
    if source_tab_id == target_tab_id && source_pane_id == target_pane_id {
        return Ok(());
    }

    let source_tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == source_tab_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Source window not found".to_string(),
        })?;
    let target_tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == target_tab_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target window not found".to_string(),
        })?;
    let source_node = clone_stack_node(&snapshot.tabs[source_tab_index].root, &source_pane_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Source pane not found".to_string(),
        })?;
    let target_node = clone_stack_node(&snapshot.tabs[target_tab_index].root, &target_pane_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target pane not found".to_string(),
        })?;

    if source_tab_index == target_tab_index {
        let tab = &mut snapshot.tabs[source_tab_index];
        let _ = replace_stack_node(&mut tab.root, &source_pane_id, &target_node);
        let _ = replace_stack_node(&mut tab.root, &target_pane_id, &source_node);
        normalize_tab(tab);
    } else {
        let (left_index, right_index) = if source_tab_index < target_tab_index {
            (source_tab_index, target_tab_index)
        } else {
            (target_tab_index, source_tab_index)
        };
        let (left_tabs, right_tabs) = snapshot.tabs.split_at_mut(right_index);
        let left_tab = &mut left_tabs[left_index];
        let right_tab = &mut right_tabs[0];
        if source_tab_index < target_tab_index {
            let _ = replace_stack_node(&mut left_tab.root, &source_pane_id, &target_node);
            let _ = replace_stack_node(&mut right_tab.root, &target_pane_id, &source_node);
            if left_tab.active_pane_id.as_deref() == Some(source_pane_id.as_str()) {
                left_tab.active_pane_id = Some(target_pane_id.clone());
            }
            if right_tab.active_pane_id.as_deref() == Some(target_pane_id.as_str()) {
                right_tab.active_pane_id = Some(source_pane_id.clone());
            }
            normalize_tab(left_tab);
            normalize_tab(right_tab);
        } else {
            let _ = replace_stack_node(&mut left_tab.root, &target_pane_id, &source_node);
            let _ = replace_stack_node(&mut right_tab.root, &source_pane_id, &target_node);
            if left_tab.active_pane_id.as_deref() == Some(target_pane_id.as_str()) {
                left_tab.active_pane_id = Some(source_pane_id.clone());
            }
            if right_tab.active_pane_id.as_deref() == Some(source_pane_id.as_str()) {
                right_tab.active_pane_id = Some(target_pane_id.clone());
            }
            normalize_tab(left_tab);
            normalize_tab(right_tab);
        }
    }

    if !payload.detached {
        if context.current_pane_id == source_pane_id {
            snapshot.active_tab_id = Some(target_tab_id.clone());
            state
                .tmux
                .update_session_focus(&context.session_id, target_tab_id.clone(), source_pane_id)
                .map_err(internal_error)?;
        } else if context.current_pane_id == target_pane_id {
            snapshot.active_tab_id = Some(source_tab_id.clone());
            state
                .tmux
                .update_session_focus(&context.session_id, source_tab_id.clone(), target_pane_id)
                .map_err(internal_error)?;
        }
    }

    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);
    Ok(())
}

fn tmux_swap_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SwapWindowRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let source_window_id = resolve_target_window_id(
        &snapshot,
        payload.source.as_deref(),
        &context.current_tab_id,
        context.previous_tab_id.as_deref(),
    )?;
    let target_window_id = resolve_target_window_id(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        context.previous_tab_id.as_deref(),
    )?;
    if source_window_id == target_window_id {
        return Ok(());
    }

    let source_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == source_window_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Source window not found".to_string(),
        })?;
    let target_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == target_window_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target window not found".to_string(),
        })?;
    snapshot.tabs.swap(source_index, target_index);
    if !payload.detached && snapshot.active_tab_id.is_none() {
        snapshot.active_tab_id = Some(context.current_tab_id.clone());
    }
    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);
    Ok(())
}

fn tmux_rotate_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: RotateWindowRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let target_window_id = resolve_target_window_id(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        context.previous_tab_id.as_deref(),
    )?;
    let tab = snapshot
        .tabs
        .iter_mut()
        .find(|tab| tab.id == target_window_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target window not found".to_string(),
        })?;

    let stack_ids = collect_stack_ids(&tab.root);
    if stack_ids.len() < 2 {
        return Ok(());
    }
    let mut rotated_nodes = stack_ids
        .iter()
        .map(|stack_id| {
            clone_stack_node(&tab.root, stack_id).ok_or_else(|| TmuxHttpError {
                status: 404,
                message: format!("Pane '{stack_id}' not found"),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    if matches!(payload.direction.as_deref(), Some("down") | Some("backward")) {
        rotated_nodes.rotate_right(1);
    } else {
        rotated_nodes.rotate_left(1);
    }

    for (stack_id, replacement) in stack_ids.iter().zip(rotated_nodes.iter()) {
        let _ = replace_stack_node(&mut tab.root, stack_id, replacement);
    }
    normalize_tab(tab);

    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);
    Ok(())
}

fn tmux_move_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: MoveWindowRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let source_window_id = resolve_target_window_id(
        &snapshot,
        payload.source.as_deref(),
        &context.current_tab_id,
        context.previous_tab_id.as_deref(),
    )?;
    let source_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == source_window_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Source window not found".to_string(),
        })?;
    let tab = snapshot.tabs.remove(source_index);
    let insert_index = resolve_move_window_insert_index(&snapshot, payload.target.as_deref(), payload.before, payload.after)?
        .min(snapshot.tabs.len());
    snapshot.tabs.insert(insert_index, tab);
    if !payload.detached {
        snapshot.active_tab_id = Some(source_window_id.clone());
    }
    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);
    Ok(())
}

fn tmux_pipe_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: PipePaneRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let (tab_id, pane_id) = resolve_target_pane_ref(
        &snapshot,
        payload.target.as_deref(),
        &context.current_tab_id,
        &context.current_pane_id,
        context.previous_tab_id.as_deref(),
    )?;
    let session = session_record_for_pane(&snapshot, &tab_id, &pane_id).ok_or_else(|| {
        TmuxHttpError {
            status: 404,
            message: "Target pane has no active session".to_string(),
        }
    })?;

    if !matches!(session.status, SessionStatus::Running | SessionStatus::Starting) {
        return Err(TmuxHttpError {
            status: 409,
            message: "Target pane is not running".to_string(),
        });
    }

    let (program, args, env) = if let Some(command) = payload.command {
        let launch_spec =
            build_tmux_child_launch_spec(state, Some(command)).map_err(internal_error)?;
        (
            Some(launch_spec.program),
            launch_spec.args,
            Some(launch_spec.env_overrides),
        )
    } else {
        (None, None, None)
    };

    let pipe_output = if program.is_some() {
        payload.pipe_output || !payload.pipe_input
    } else {
        false
    };

    state
        .sessions
        .configure_pipe(
            &session.id,
            SessionPipeOptions {
                program,
                args,
                cwd: Some(session.cwd),
                env,
                pipe_output,
                pipe_input: payload.pipe_input,
                only_if_none: payload.only_if_none,
            },
        )
        .map_err(internal_error)?;

    Ok(())
}

fn tmux_kill_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    target: Option<&str>,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let (session_ids, mut snapshot) = {
        let mut snapshot =
            load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
        let Some(tab) = snapshot
            .tabs
            .iter()
            .find(|tab| tab.id == context.current_tab_id)
        else {
            return Err(TmuxHttpError {
                status: 404,
                message: "Caller tab not found".to_string(),
            });
        };
        let target_stack_id = resolve_target_pane_id(tab, target, &context.current_pane_id)?;
        let session_ids =
            close_pane_in_snapshot(&mut snapshot, &context.current_tab_id, &target_stack_id)
                .map_err(internal_error)?;
        if let Some(next_tab) = snapshot
            .tabs
            .iter()
            .find(|entry| entry.id == context.current_tab_id)
        {
            let next_pane_id = next_tab
                .active_pane_id
                .clone()
                .or_else(|| first_stack_id(&next_tab.root))
                .unwrap_or_else(|| context.origin_pane_id.clone());
            state
                .tmux
                .update_session_focus(&context.session_id, next_tab.id.clone(), next_pane_id)
                .map_err(internal_error)?;
        }
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

fn tmux_capture_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: CapturePaneRequest,
) -> Result<String, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let Some(tab) = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == context.current_tab_id)
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Caller tab not found".to_string(),
        });
    };
    let target_pane_id =
        resolve_target_pane_id(tab, payload.target.as_deref(), &context.current_pane_id)?;
    let Some(session_id) = active_session_for_pane(&tab.root, &target_pane_id) else {
        return Ok(String::new());
    };
    state
        .sessions
        .capture_output(
            &session_id,
            SessionCaptureOptions {
                include_escape: payload.include_escape,
                join_lines: payload.join_lines,
                start_line: payload.start_line,
                end_line: payload.end_line,
            },
        )
        .map_err(internal_error)
}

fn tmux_resize_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: ResizePaneRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let mut snapshot =
        load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let Some(tab) = snapshot
        .tabs
        .iter_mut()
        .find(|tab| tab.id == context.current_tab_id)
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Caller tab not found".to_string(),
        });
    };
    let target_pane_id =
        resolve_target_pane_id(tab, payload.target.as_deref(), &context.current_pane_id)?;
    let viewport = state
        .tab_viewports
        .lock()
        .ok()
        .and_then(|cache| cache.get(&tab_viewport_key(&context.project_id, &context.current_tab_id)).copied())
        .unwrap_or(TabViewport {
            width: 160.0,
            height: 90.0,
        });
    let changed = apply_tmux_resize_to_tab(tab, &target_pane_id, viewport, &payload)?;
    if !changed {
        return Ok(());
    }
    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);
    Ok(())
}

fn tmux_kill_window(
    state: &AppState,
    context: &TmuxTokenContext,
    target: Option<&str>,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;

    let (session_ids, mut snapshot) = {
        let mut snapshot =
            load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
        let target_window_id = resolve_target_window_id(
            &snapshot,
            target,
            &context.current_tab_id,
            context.previous_tab_id.as_deref(),
        )?;
        let Some(tab_index) = snapshot.tabs.iter().position(|tab| tab.id == target_window_id) else {
            return Err(TmuxHttpError {
                status: 404,
                message: "Target window not found".to_string(),
            });
        };
        let mut session_ids = Vec::new();
        collect_session_ids(&snapshot.tabs[tab_index].root, &mut session_ids);
        snapshot.tabs.remove(tab_index);

        if snapshot.tabs.is_empty() {
            let replacement = new_workspace_tab("main".to_string());
            snapshot.active_tab_id = Some(replacement.id.clone());
            snapshot.tabs.push(replacement);
        } else if snapshot.active_tab_id.as_deref() == Some(target_window_id.as_str()) {
            snapshot.active_tab_id = Some(snapshot.tabs[tab_index.saturating_sub(1)].id.clone());
        }

        if let Some(next_tab_id) = snapshot.active_tab_id.clone() {
            if let Some(next_tab) = snapshot.tabs.iter().find(|tab| tab.id == next_tab_id) {
                let next_pane_id = next_tab
                    .active_pane_id
                    .clone()
                    .or_else(|| first_stack_id(&next_tab.root))
                    .unwrap_or_else(|| context.origin_pane_id.clone());
                state
                    .tmux
                    .update_session_focus(&context.session_id, next_tab.id.clone(), next_pane_id)
                    .map_err(internal_error)?;
            }
        }
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

fn tmux_select_pane(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SelectPaneRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;

    let mut snapshot =
        load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    let Some(tab) = snapshot
        .tabs
        .iter_mut()
        .find(|tab| tab.id == context.current_tab_id)
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Caller tab not found".to_string(),
        });
    };
    let current_pane_id = current_tmux_pane_id(tab, &context.current_pane_id);
    let target_pane_id = if payload.last {
        context
            .previous_pane_id
            .as_ref()
            .filter(|pane_id| stack_exists(&tab.root, pane_id))
            .cloned()
            .ok_or_else(|| TmuxHttpError {
                status: 404,
                message: "No last pane available".to_string(),
            })?
    } else if let Some(target) = payload.target.as_deref() {
        resolve_target_pane_id(tab, Some(target), &current_pane_id)?
    } else if let Some(direction) = payload.direction.as_deref() {
        resolve_directional_pane_id(tab, &current_pane_id, direction)?
    } else {
        current_pane_id
    };
    tab.active_pane_id = Some(target_pane_id);
    state
        .tmux
        .update_session_focus(
            &context.session_id,
            context.current_tab_id.clone(),
            tab.active_pane_id.clone().unwrap_or_else(|| context.current_pane_id.clone()),
        )
        .map_err(internal_error)?;
    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);
    Ok(())
}

fn tmux_select_window(
    state: &AppState,
    context: &TmuxTokenContext,
    payload: SelectWindowRequest,
) -> Result<(), TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;

    let mut snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    ).map_err(internal_error)?;
    if snapshot.tabs.is_empty() {
        return Err(TmuxHttpError {
            status: 404,
            message: "No windows available".to_string(),
        });
    }

    let mode = payload.mode.as_deref().unwrap_or("target");
    let target_window_id = match mode {
        "last" => resolve_last_window_id(&snapshot, &context)?,
        "next" => resolve_relative_window_id(&snapshot, &context.current_tab_id, 1)?,
        "previous" => resolve_relative_window_id(&snapshot, &context.current_tab_id, -1)?,
        _ => {
            let resolved = resolve_target_window_id(
                &snapshot,
                payload.target.as_deref(),
                &context.current_tab_id,
                context.previous_tab_id.as_deref(),
            )?;
            if payload.toggle_if_current && resolved == context.current_tab_id {
                resolve_last_window_id(&snapshot, &context)?
            } else {
                resolved
            }
        }
    };

    let Some(target_tab) = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == target_window_id)
    else {
        return Err(TmuxHttpError {
            status: 404,
            message: "Target window not found".to_string(),
        });
    };
    let target_pane_id = target_tab
        .active_pane_id
        .clone()
        .or_else(|| first_stack_id(&target_tab.root))
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target window has no panes".to_string(),
        })?;

    snapshot.active_tab_id = Some(target_window_id.clone());
    refresh_snapshot_sessions(state, &mut snapshot).map_err(internal_error)?;
    persist_workspace_snapshot(state, snapshot).map_err(internal_error)?;
    state
        .tmux
        .update_session_focus(&context.session_id, target_window_id, target_pane_id)
        .map_err(internal_error)?;
    emit_workspace_changed(&state.tmux.app_handle, &context.project_id);
    Ok(())
}

fn tmux_display_message(
    state: &AppState,
    context: &TmuxTokenContext,
    format: &str,
) -> Result<String, TmuxHttpError> {
    let context = normalize_tmux_context(state, context)?;
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    )
    .map_err(internal_error)?;
    let session_name = tmux_session_name(state, &context.project_id, &context.workspace_session_id)
        .map_err(internal_error)?;
    let tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == context.current_tab_id)
        .unwrap_or(0);
    let mut panes = Vec::new();
    if let Some(tab) = snapshot.tabs.get(tab_index) {
        collect_tab_panes(
            &tab.root,
            &snapshot.sessions,
            tab_index,
            &context.current_tab_id,
            &session_name,
            &mut panes,
        );
    }
    let pane = panes
        .into_iter()
        .find(|pane| pane.pane_id == context.current_pane_id)
        .unwrap_or(PaneListing {
            pane_id: context.current_pane_id.clone(),
            pane_index: 0,
            pane_title: "pane".to_string(),
            pane_current_command: "terminal".to_string(),
            window_index: tab_index,
            session_name,
            window_id: context.current_tab_id.clone(),
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

fn resolve_target_window_id(
    snapshot: &WorkspaceSnapshot,
    target: Option<&str>,
    current_tab_id: &str,
    previous_tab_id: Option<&str>,
) -> Result<String, TmuxHttpError> {
    let Some(target) = target else {
        return Ok(current_tab_id.to_string());
    };

    let token = extract_window_target_token(target);
    if token.is_empty() {
        return Ok(current_tab_id.to_string());
    }

    if let Some(relative) = resolve_window_target_alias(snapshot, &token, current_tab_id, previous_tab_id)? {
        return Ok(relative);
    }

    let normalized = token.strip_prefix('@').unwrap_or(token.as_str());
    if let Some(tab) = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == normalized || tab.title == normalized)
    {
        return Ok(tab.id.clone());
    }

    if let Ok(index) = normalized.parse::<usize>() {
        if let Some(tab) = snapshot.tabs.get(index) {
            return Ok(tab.id.clone());
        }
        if index > 0 {
            let one_based = index - 1;
            if let Some(tab) = snapshot.tabs.get(one_based) {
                return Ok(tab.id.clone());
            }
        }
    }

    Err(TmuxHttpError {
        status: 404,
        message: format!("Target window '{target}' not found in project workspace"),
    })
}

fn resolve_target_pane_ref(
    snapshot: &WorkspaceSnapshot,
    target: Option<&str>,
    current_tab_id: &str,
    current_pane_id: &str,
    previous_tab_id: Option<&str>,
) -> Result<(String, String), TmuxHttpError> {
    let Some(target) = target else {
        return Ok((current_tab_id.to_string(), current_pane_id.to_string()));
    };

    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Ok((current_tab_id.to_string(), current_pane_id.to_string()));
    }

    let has_window_component = trimmed.contains(':') || trimmed.contains('.');
    let target_tab_id = if has_window_component && extract_window_target_token(trimmed) != extract_target_token(trimmed) {
        resolve_target_window_id(snapshot, Some(trimmed), current_tab_id, previous_tab_id)?
    } else {
        current_tab_id.to_string()
    };
    let tab = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == target_tab_id)
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Target tab not found".to_string(),
        })?;

    let pane_target = extract_target_token(trimmed);
    let target_pane_id = if has_window_component && pane_target == extract_window_target_token(trimmed) {
        current_tmux_pane_id(tab, &first_stack_id(&tab.root).unwrap_or_else(|| current_pane_id.to_string()))
    } else {
        let fallback = if target_tab_id == current_tab_id {
            current_pane_id.to_string()
        } else {
            current_tmux_pane_id(
                tab,
                &first_stack_id(&tab.root).unwrap_or_else(|| current_pane_id.to_string()),
            )
        };
        resolve_target_pane_id(tab, Some(trimmed), &fallback)?
    };

    Ok((target_tab_id, target_pane_id))
}

fn resolve_window_target_alias(
    snapshot: &WorkspaceSnapshot,
    token: &str,
    current_tab_id: &str,
    previous_tab_id: Option<&str>,
) -> Result<Option<String>, TmuxHttpError> {
    match token {
        "!" | "{last}" | "last" => Ok(Some(resolve_last_window_id_by_value(
            snapshot,
            previous_tab_id,
        )?)),
        "+" | "{next}" | "next" => Ok(Some(resolve_relative_window_id(
            snapshot,
            current_tab_id,
            1,
        )?)),
        "-" | "{previous}" | "previous" => Ok(Some(resolve_relative_window_id(
            snapshot,
            current_tab_id,
            -1,
        )?)),
        _ => {
            if let Some(offset) = parse_relative_window_offset(token) {
                return Ok(Some(resolve_relative_window_id(
                    snapshot,
                    current_tab_id,
                    offset,
                )?));
            }
            Ok(None)
        }
    }
}

fn resolve_relative_window_id(
    snapshot: &WorkspaceSnapshot,
    current_tab_id: &str,
    offset: isize,
) -> Result<String, TmuxHttpError> {
    if snapshot.tabs.is_empty() {
        return Err(TmuxHttpError {
            status: 404,
            message: "No windows available".to_string(),
        });
    }
    let current_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == current_tab_id)
        .unwrap_or(0);
    let len = snapshot.tabs.len() as isize;
    let next_index = (current_index as isize + offset).rem_euclid(len) as usize;
    Ok(snapshot.tabs[next_index].id.clone())
}

fn resolve_last_window_id(
    snapshot: &WorkspaceSnapshot,
    context: &TmuxTokenContext,
) -> Result<String, TmuxHttpError> {
    resolve_last_window_id_by_value(snapshot, context.previous_tab_id.as_deref())
}

fn resolve_last_window_id_by_value(
    snapshot: &WorkspaceSnapshot,
    previous_tab_id: Option<&str>,
) -> Result<String, TmuxHttpError> {
    previous_tab_id
        .and_then(|previous| snapshot.tabs.iter().find(|tab| tab.id == previous))
        .map(|tab| tab.id.clone())
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "No last window available".to_string(),
        })
}

fn parse_relative_window_offset(token: &str) -> Option<isize> {
    if token.len() < 2 {
        return None;
    }
    let (sign, rest) = token.split_at(1);
    if sign != "+" && sign != "-" {
        return None;
    }
    let magnitude = rest.parse::<isize>().ok()?;
    Some(if sign == "+" { magnitude } else { -magnitude })
}

fn resolve_move_window_insert_index(
    snapshot: &WorkspaceSnapshot,
    target: Option<&str>,
    before: bool,
    after: bool,
) -> Result<usize, TmuxHttpError> {
    let Some(target) = target else {
        return Ok(snapshot.tabs.len());
    };
    let token = extract_window_target_token(target);
    if token.is_empty() {
        return Ok(snapshot.tabs.len());
    }

    if let Ok(index) = token.parse::<usize>() {
        let base = index.min(snapshot.tabs.len());
        if before {
            return Ok(base);
        }
        if after {
            return Ok((base + 1).min(snapshot.tabs.len()));
        }
        return Ok(base);
    }

    let target_window_id = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == token || tab.title == token)
        .map(|tab| tab.id.clone())
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: format!("Target window '{target}' not found in project workspace"),
        })?;
    let base = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == target_window_id)
        .unwrap_or(snapshot.tabs.len());
    if before {
        Ok(base)
    } else if after {
        Ok((base + 1).min(snapshot.tabs.len()))
    } else {
        Ok(base)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct NormalizedRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Debug)]
struct PaneRect {
    pane_id: String,
    rect: NormalizedRect,
}

fn current_tmux_pane_id(tab: &WorkspaceTab, caller_pane_id: &str) -> String {
    tab.active_pane_id
        .as_ref()
        .filter(|pane_id| stack_exists(&tab.root, pane_id))
        .cloned()
        .unwrap_or_else(|| caller_pane_id.to_string())
}

fn resolve_tmux_split_direction(direction: Option<&str>) -> String {
    match direction {
        Some("horizontal") | Some("h") => "horizontal".to_string(),
        Some("vertical") | Some("v") | None => "vertical".to_string(),
        Some(_) => "vertical".to_string(),
    }
}

fn split_tmux_pane(
    tab: &mut WorkspaceTab,
    target_pane_id: &str,
    direction: &str,
    full_span: bool,
    insertion: SplitInsertion,
    new_pane_size: Option<u16>,
) -> Result<String, TmuxHttpError> {
    if full_span {
        return Ok(wrap_root_with_split(
            tab,
            direction,
            PaneCreatedBy::Ai,
            Some(target_pane_id.to_string()),
            insertion,
            new_pane_size,
        ));
    }

    split_stack_node_with_options(
        &mut tab.root,
        target_pane_id,
        direction,
        &mut tab.next_pane_ordinal,
        PaneCreatedBy::Ai,
        Some(target_pane_id.to_string()),
        insertion,
        new_pane_size,
    )
    .ok_or_else(|| TmuxHttpError {
        status: 404,
        message: format!("Target pane '{target_pane_id}' not found"),
    })
}

fn resolve_tmux_split_size_percentage(
    state: &AppState,
    project_id: &str,
    tab_id: &str,
    tab: &mut WorkspaceTab,
    requested_size: Option<u16>,
    size_is_percentage: bool,
    direction: &str,
    target_pane_id: &str,
    full_span: bool,
) -> Option<u16> {
    let requested_size = requested_size?;
    if size_is_percentage {
        return Some(requested_size.clamp(1, 99));
    }

    let viewport = state
        .tab_viewports
        .lock()
        .ok()
        .and_then(|cache| cache.get(&tab_viewport_key(project_id, tab_id)).copied())?;
    let viewport_units = if direction == "horizontal" {
        viewport.width
    } else {
        viewport.height
    };
    if viewport_units <= 0.0 {
        return None;
    }

    let pane_ratio = if full_span {
        1.0
    } else {
        find_pane_rect(&tab.root, target_pane_id)
            .map(|pane| {
                if direction == "horizontal" {
                    pane.rect.width
                } else {
                    pane.rect.height
                }
            })
            .unwrap_or(1.0)
    };
    let target_units = viewport_units * pane_ratio;
    if target_units <= 0.0 {
        return None;
    }

    Some(
        ((requested_size as f64 / target_units) * 100.0)
            .round()
            .clamp(1.0, 99.0) as u16,
    )
}

fn apply_tmux_resize_to_tab(
    tab: &mut WorkspaceTab,
    target_pane_id: &str,
    viewport: TabViewport,
    payload: &ResizePaneRequest,
) -> Result<bool, TmuxHttpError> {
    let target_rect = find_pane_rect(&tab.root, target_pane_id).ok_or_else(|| TmuxHttpError {
        status: 404,
        message: format!("Target pane '{target_pane_id}' not found"),
    })?;
    let root_rect = NormalizedRect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };

    if let Some(width) = payload.width {
        let current_units = target_rect.rect.width * viewport.width.max(1.0);
        let delta_units = width as f64 - current_units;
        return Ok(
            try_resize_with_edges(
                &mut tab.root,
                target_pane_id,
                "horizontal",
                &[ResizeEdge::Forward, ResizeEdge::Backward],
                delta_units,
                root_rect,
                viewport,
            ),
        );
    }

    if let Some(height) = payload.height {
        let current_units = target_rect.rect.height * viewport.height.max(1.0);
        let delta_units = height as f64 - current_units;
        return Ok(
            try_resize_with_edges(
                &mut tab.root,
                target_pane_id,
                "vertical",
                &[ResizeEdge::Forward, ResizeEdge::Backward],
                delta_units,
                root_rect,
                viewport,
            ),
        );
    }

    let adjustment = payload.adjustment.unwrap_or(1) as f64;
    let (axis, edge) = match payload.direction.as_deref() {
        Some("left") | Some("l") => ("horizontal", ResizeEdge::Backward),
        Some("right") | Some("r") => ("horizontal", ResizeEdge::Forward),
        Some("up") | Some("u") => ("vertical", ResizeEdge::Backward),
        Some("down") | Some("d") => ("vertical", ResizeEdge::Forward),
        _ => return Ok(false),
    };

    Ok(try_resize_with_edges(
        &mut tab.root,
        target_pane_id,
        axis,
        &[edge],
        adjustment,
        root_rect,
        viewport,
    ))
}

fn try_resize_with_edges(
    node: &mut models::LayoutNode,
    target_pane_id: &str,
    axis: &str,
    edges: &[ResizeEdge],
    delta_units: f64,
    rect: NormalizedRect,
    viewport: TabViewport,
) -> bool {
    if delta_units.abs() < f64::EPSILON {
        return false;
    }

    for edge in edges {
        let mut candidate = node.clone();
        if resize_layout_node(
            &mut candidate,
            target_pane_id,
            axis,
            *edge,
            delta_units,
            rect,
            viewport,
        ) {
            *node = candidate;
            return true;
        }
    }
    false
}

fn resize_layout_node(
    node: &mut models::LayoutNode,
    target_pane_id: &str,
    axis: &str,
    edge: ResizeEdge,
    delta_units: f64,
    rect: NormalizedRect,
    viewport: TabViewport,
) -> bool {
    match node {
        models::LayoutNode::Stack { .. } => false,
        models::LayoutNode::Split {
            direction,
            sizes,
            children,
            ..
        } => {
            let Some(target_index) = children
                .iter()
                .position(|child| stack_exists(child, target_pane_id))
            else {
                return false;
            };

            let child_rects = split_child_rects(rect, direction, sizes, children.len());
            if let Some(child_rect) = child_rects.get(target_index).copied() {
                if resize_layout_node(
                    &mut children[target_index],
                    target_pane_id,
                    axis,
                    edge,
                    delta_units,
                    child_rect,
                    viewport,
                ) {
                    return true;
                }
            }

            if direction != axis {
                return false;
            }

            let sibling_index = match edge {
                ResizeEdge::Backward => target_index.checked_sub(1),
                ResizeEdge::Forward => {
                    if target_index + 1 < children.len() {
                        Some(target_index + 1)
                    } else {
                        None
                    }
                }
            };
            let Some(sibling_index) = sibling_index else {
                return false;
            };

            let container_units = if axis == "horizontal" {
                rect.width * viewport.width.max(1.0)
            } else {
                rect.height * viewport.height.max(1.0)
            };
            if container_units <= 0.0 {
                return false;
            }

            let delta_percent = (delta_units / container_units) * 100.0;
            adjust_split_sizes(sizes, target_index, sibling_index, delta_percent)
        }
    }
}

fn split_child_rects(
    rect: NormalizedRect,
    direction: &str,
    sizes: &[u16],
    child_count: usize,
) -> Vec<NormalizedRect> {
    let count = child_count.max(1);
    let mut result = Vec::with_capacity(child_count);
    let mut offset = 0.0;
    for index in 0..child_count {
        let ratio = sizes
            .get(index)
            .copied()
            .unwrap_or((100 / count) as u16) as f64
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
        result.push(child_rect);
        offset += ratio;
    }
    result
}

fn adjust_split_sizes(
    sizes: &mut [u16],
    target_index: usize,
    sibling_index: usize,
    delta_percent: f64,
) -> bool {
    if target_index >= sizes.len() || sibling_index >= sizes.len() || target_index == sibling_index {
        return false;
    }

    let mut delta = delta_percent.round() as i32;
    if delta == 0 {
        delta = if delta_percent.is_sign_negative() { -1 } else { 1 };
    }

    let target_size = sizes[target_index] as i32;
    let sibling_size = sizes[sibling_index] as i32;
    let max_grow = sibling_size - 1;
    let max_shrink = target_size - 1;
    let applied_delta = delta.clamp(-max_shrink, max_grow);
    if applied_delta == 0 {
        return false;
    }

    sizes[target_index] = (target_size + applied_delta).clamp(1, 99) as u16;
    sizes[sibling_index] = (sibling_size - applied_delta).clamp(1, 99) as u16;
    true
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

fn resolve_directional_pane_id(
    tab: &WorkspaceTab,
    current_pane_id: &str,
    direction: &str,
) -> Result<String, TmuxHttpError> {
    let mut panes = Vec::new();
    collect_pane_rects(
        &tab.root,
        NormalizedRect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
        &mut panes,
    );
    let current = panes
        .iter()
        .find(|pane| pane.pane_id == current_pane_id)
        .cloned()
        .ok_or_else(|| TmuxHttpError {
            status: 404,
            message: "Current pane not found".to_string(),
        })?;

    let mut best: Option<(String, f64, f64)> = None;
    for pane in panes.into_iter().filter(|pane| pane.pane_id != current_pane_id) {
        let distance = directional_distance(&current.rect, &pane.rect, direction);
        let overlap = directional_overlap(&current.rect, &pane.rect, direction);
        let Some(distance) = distance else {
            continue;
        };
        if overlap <= 0.0 {
            continue;
        }

        match &best {
            Some((_, best_distance, best_overlap))
                if distance > *best_distance + 1e-6
                    || ((distance - *best_distance).abs() <= 1e-6 && overlap <= *best_overlap) => {}
            _ => {
                best = Some((pane.pane_id, distance, overlap));
            }
        }
    }

    best.map(|(pane_id, _, _)| pane_id).ok_or_else(|| TmuxHttpError {
        status: 404,
        message: format!("No pane found in direction '{direction}'"),
    })
}

fn directional_distance(current: &NormalizedRect, candidate: &NormalizedRect, direction: &str) -> Option<f64> {
    let current_right = current.x + current.width;
    let current_bottom = current.y + current.height;
    let candidate_right = candidate.x + candidate.width;
    let candidate_bottom = candidate.y + candidate.height;
    match direction {
        "left" => Some(current.x - candidate_right).filter(|distance| *distance >= -1e-6),
        "right" => Some(candidate.x - current_right).filter(|distance| *distance >= -1e-6),
        "up" => Some(current.y - candidate_bottom).filter(|distance| *distance >= -1e-6),
        "down" => Some(candidate.y - current_bottom).filter(|distance| *distance >= -1e-6),
        _ => None,
    }
}

fn directional_overlap(current: &NormalizedRect, candidate: &NormalizedRect, direction: &str) -> f64 {
    match direction {
        "left" | "right" => axis_overlap(
            current.y,
            current.y + current.height,
            candidate.y,
            candidate.y + candidate.height,
        ),
        "up" | "down" => axis_overlap(
            current.x,
            current.x + current.width,
            candidate.x,
            candidate.x + candidate.width,
        ),
        _ => 0.0,
    }
}

fn axis_overlap(a_start: f64, a_end: f64, b_start: f64, b_end: f64) -> f64 {
    (a_end.min(b_end) - a_start.max(b_start)).max(0.0)
}

fn collect_pane_rects(
    node: &models::LayoutNode,
    rect: NormalizedRect,
    panes: &mut Vec<PaneRect>,
) {
    match node {
        models::LayoutNode::Stack { id, .. } => panes.push(PaneRect {
            pane_id: id.clone(),
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

fn clone_stack_node(node: &models::LayoutNode, target_stack_id: &str) -> Option<models::LayoutNode> {
    match node {
        models::LayoutNode::Stack { id, .. } if id == target_stack_id => Some(node.clone()),
        models::LayoutNode::Split { children, .. } => children
            .iter()
            .find_map(|child| clone_stack_node(child, target_stack_id)),
        _ => None,
    }
}

fn replace_stack_node(
    node: &mut models::LayoutNode,
    target_stack_id: &str,
    replacement: &models::LayoutNode,
) -> bool {
    match node {
        models::LayoutNode::Stack { id, .. } if id == target_stack_id => {
            *node = replacement.clone();
            true
        }
        models::LayoutNode::Split { children, .. } => children
            .iter_mut()
            .any(|child| replace_stack_node(child, target_stack_id, replacement)),
        _ => false,
    }
}

fn insert_existing_stack_node(
    node: &mut models::LayoutNode,
    target_stack_id: &str,
    direction: &str,
    insertion: SplitInsertion,
    new_child_size: Option<u16>,
    existing_stack: &mut Option<models::LayoutNode>,
) -> bool {
    match node {
        models::LayoutNode::Stack { id, .. } if id == target_stack_id => {
            let Some(existing_stack) = existing_stack.take() else {
                return false;
            };
            let current = node.clone();
            let sizes = split_sizes_for_existing_child(new_child_size, insertion);
            let children = match insertion {
                SplitInsertion::Before => vec![existing_stack, current],
                SplitInsertion::After => vec![current, existing_stack],
            };
            *node = models::LayoutNode::Split {
                id: Uuid::new_v4().to_string(),
                direction: direction.to_string(),
                zone_kind: models::SplitZoneKind::Default,
                sizes,
                children,
            };
            true
        }
        models::LayoutNode::Split { children, .. } => children.iter_mut().any(|child| {
            insert_existing_stack_node(
                child,
                target_stack_id,
                direction,
                insertion,
                new_child_size,
                existing_stack,
            )
        }),
        _ => false,
    }
}

fn split_sizes_for_existing_child(new_child_size: Option<u16>, insertion: SplitInsertion) -> Vec<u16> {
    let Some(new_child_size) = new_child_size.map(|size| size.clamp(1, 99)) else {
        return vec![50, 50];
    };
    let existing_size = 100 - new_child_size;
    match insertion {
        SplitInsertion::Before => vec![new_child_size, existing_size],
        SplitInsertion::After => vec![existing_size, new_child_size],
    }
}

fn detach_pane_from_snapshot(
    snapshot: &mut WorkspaceSnapshot,
    tab_id: &str,
    stack_id: &str,
) -> Result<models::LayoutNode, String> {
    let tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == tab_id)
        .ok_or_else(|| "Tab not found".to_string())?;
    let detached = clone_stack_node(&snapshot.tabs[tab_index].root, stack_id)
        .ok_or_else(|| "Pane not found".to_string())?;

    let close_result = {
        let tab = &mut snapshot.tabs[tab_index];
        close_stack_node(&mut tab.root, stack_id)
    };
    match close_result {
        ClosePaneResult::NotFound => Err("Pane not found".to_string()),
        ClosePaneResult::Updated(_) => {
            if let Some(tab) = snapshot.tabs.get_mut(tab_index) {
                ensure_valid_active_pane(tab);
                normalize_tab(tab);
            }
            ensure_valid_active_tab(snapshot);
            Ok(detached)
        }
        ClosePaneResult::RootRemoved(_) => {
            snapshot.tabs.remove(tab_index);
            ensure_valid_active_tab(snapshot);
            Ok(detached)
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

fn extract_window_target_token(target: &str) -> String {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let after_session = trimmed
        .rsplit_once(':')
        .map(|(_, tail)| tail)
        .unwrap_or(trimmed);
    let before_pane = after_session
        .split_once('.')
        .map(|(head, _)| head)
        .unwrap_or(after_session);
    before_pane.trim().to_string()
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

fn session_record_for_pane(
    snapshot: &WorkspaceSnapshot,
    tab_id: &str,
    pane_id: &str,
) -> Option<TerminalSession> {
    let tab = snapshot.tabs.iter().find(|tab| tab.id == tab_id)?;
    let session_id = active_session_for_pane(&tab.root, pane_id)?;
    snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .cloned()
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

fn tmux_pane_listing(
    state: &AppState,
    context: &TmuxTokenContext,
    target_pane_id: &str,
) -> Result<PaneListing, String> {
    let snapshot = load_workspace_session_snapshot(
        state,
        &context.project_id,
        &context.workspace_session_id,
    )?;
    let tab_index = snapshot
        .tabs
        .iter()
        .position(|tab| tab.id == context.current_tab_id)
        .ok_or_else(|| "Caller tab not found".to_string())?;
    let tab = snapshot
        .tabs
        .get(tab_index)
        .ok_or_else(|| "Caller tab not found".to_string())?;
    let mut panes = Vec::new();
    let session_name =
        tmux_session_name(state, &context.project_id, &context.workspace_session_id)?;
    collect_tab_panes(
        &tab.root,
        &snapshot.sessions,
        tab_index,
        &context.current_tab_id,
        &session_name,
        &mut panes,
    );
    panes
        .into_iter()
        .find(|pane| pane.pane_id == target_pane_id)
        .ok_or_else(|| format!("Pane '{target_pane_id}' not found"))
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

fn render_tmux_window_format(format: &str, window: &WindowListing) -> String {
    let mut result = format.to_string();
    result = result.replace("#{window_id}", &window.window_id);
    result = result.replace("#{window_index}", &window.window_index.to_string());
    result = result.replace("#{window_name}", &window.window_name);
    result = result.replace(
        "#{window_active}",
        if window.window_active { "1" } else { "0" },
    );
    result = result.replace("#{session_name}", &window.session_name);
    result = result.replace("#I", &window.window_index.to_string());
    result = result.replace("#W", &window.window_name);
    result = result.replace("#F", if window.window_active { "*" } else { "-" });
    result = result.replace("#S", &window.session_name);
    result
}

fn render_tmux_session_format(format: &str, session: &SessionListing) -> String {
    let mut result = format.to_string();
    result = result.replace("#{session_id}", &session.session_id);
    result = result.replace("#{session_name}", &session.session_name);
    result = result.replace("#{session_windows}", &session.session_windows.to_string());
    result = result.replace(
        "#{session_attached}",
        if session.session_attached { "1" } else { "0" },
    );
    result = result.replace("#S", &session.session_name);
    result
}

fn tmux_binding_key(table: Option<&str>, key: &str) -> String {
    format!("{}::{}", table.unwrap_or("prefix"), key.trim())
}

fn render_tmux_key_binding_line(key: &str, command: &str) -> String {
    if let Some((table, binding)) = key.split_once("::") {
        format!("bind-key -T {table} {binding} {command}")
    } else {
        format!("bind-key {key} {command}")
    }
}

fn tmux_hook_key(
    project_id: &str,
    workspace_session_id: &str,
    target: Option<&str>,
    global: bool,
    hook_name: &str,
) -> String {
    if global {
        format!("global::{hook_name}")
    } else if let Some(target) = target.filter(|entry| !entry.trim().is_empty()) {
        format!("target:{}::{hook_name}", target.trim())
    } else {
        format!("session:{project_id}:{workspace_session_id}::{hook_name}")
    }
}

fn render_tmux_hook_line(key: &str, command: &str) -> String {
    if let Some(rest) = key.strip_prefix("global::") {
        return format!("set-hook -g {rest} {command}");
    }
    if let Some(rest) = key.strip_prefix("target:") {
        if let Some((target, hook_name)) = rest.split_once("::") {
            return format!("set-hook -t {target} {hook_name} {command}");
        }
    }
    if let Some(rest) = key.strip_prefix("session:") {
        if let Some((_, hook_name)) = rest.rsplit_once("::") {
            return format!("set-hook {hook_name} {command}");
        }
    }
    format!("set-hook {key} {command}")
}

fn next_tmux_buffer_name(buffers: &HashMap<String, String>) -> String {
    let mut index = 0;
    loop {
        let candidate = format!("buffer{index}");
        if !buffers.contains_key(&candidate) {
            return candidate;
        }
        index += 1;
    }
}

fn render_tmux_buffer_format(format: &str, name: &str, value: &str) -> String {
    let sample = value.lines().next().unwrap_or(value).chars().take(32).collect::<String>();
    let mut result = format.to_string();
    result = result.replace("#{buffer_name}", name);
    result = result.replace("#{buffer_size}", &value.len().to_string());
    result = result.replace("#{buffer_sample}", &sample);
    result
}

fn render_tmux_client_format(format: &str, client: &ClientListing) -> String {
    let pane_id = if client.pane_id.starts_with('%') {
        client.pane_id.clone()
    } else {
        format!("%{}", client.pane_id)
    };
    let mut result = format.to_string();
    result = result.replace("#{client_name}", &client.client_name);
    result = result.replace("#{client_pid}", &client.client_pid.to_string());
    result = result.replace("#{client_tty}", &client.client_tty);
    result = result.replace("#{client_session}", &client.session_name);
    result = result.replace("#{session_name}", &client.session_name);
    result = result.replace("#{window_id}", &client.window_id);
    result = result.replace("#{pane_id}", &pane_id);
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

fn query_flag(query: &str, key: &str) -> bool {
    query
        .split('&')
        .filter(|segment| !segment.is_empty())
        .any(|segment| {
            if let Some((flag, value)) = segment.split_once('=') {
                flag == key && matches!(value, "1" | "true" | "yes")
            } else {
                segment == key
            }
        })
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

fn load_project_snapshot(
    state: &AppState,
    project_id: &str,
) -> Result<ProjectWorkspaceSnapshot, String> {
    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    db.ensure_default_workspace_session(project_id)?;
    let sessions = db.list_workspace_sessions(project_id)?;
    Ok(ProjectWorkspaceSnapshot {
        project_id: project_id.to_string(),
        sessions,
    })
}

fn default_workspace_session_id(state: &AppState, project_id: &str) -> Result<String, String> {
    let project_snapshot = load_project_snapshot(state, project_id)?;
    project_snapshot
        .sessions
        .first()
        .map(|session| session.id.clone())
        .ok_or_else(|| "Workspace session not found".to_string())
}

fn load_workspace_session_snapshot(
    state: &AppState,
    project_id: &str,
    workspace_session_id: &str,
) -> Result<WorkspaceSnapshot, String> {
    {
        let db = state
            .db
            .lock()
            .map_err(|_| "Database lock poisoned".to_string())?;
        db.ensure_default_workspace_session(project_id)?;
        db.get_workspace_session(project_id, workspace_session_id)?
            .ok_or_else(|| "Workspace session not found".to_string())?;
    }

    let cached_snapshot = {
        let workspaces = state
            .workspaces
            .lock()
            .map_err(|_| "Workspace cache lock poisoned".to_string())?;
        workspaces.get(workspace_session_id).cloned()
    };

    if let Some(snapshot) = cached_snapshot {
        return persist_workspace_snapshot(state, snapshot);
    }

    let snapshot = WorkspaceSnapshot {
        project_id: project_id.to_string(),
        session_id: workspace_session_id.to_string(),
        active_tab_id: None,
        tabs: Vec::new(),
        sessions: Vec::new(),
    };
    persist_workspace_snapshot(state, snapshot)
}

fn load_workspace_snapshot(
    state: &AppState,
    project_id: &str,
) -> Result<WorkspaceSnapshot, String> {
    let workspace_session_id = default_workspace_session_id(state, project_id)?;
    load_workspace_session_snapshot(state, project_id, &workspace_session_id)
}

fn refresh_snapshot_sessions(
    state: &AppState,
    snapshot: &mut WorkspaceSnapshot,
) -> Result<(), String> {
    let db = state
        .db
        .lock()
        .map_err(|_| "Database lock poisoned".to_string())?;
    snapshot.sessions = db.list_sessions_for_workspace_session(&snapshot.session_id)?;
    Ok(())
}

fn persist_workspace_snapshot(
    state: &AppState,
    mut snapshot: WorkspaceSnapshot,
) -> Result<WorkspaceSnapshot, String> {
    ensure_workspace_snapshot_has_window(&mut snapshot);

    for tab in snapshot.tabs.iter_mut() {
        normalize_tab(tab);
    }

    if !snapshot
        .active_tab_id
        .as_ref()
        .is_some_and(|tab_id| snapshot.tabs.iter().any(|tab| tab.id == *tab_id))
    {
        snapshot.active_tab_id = snapshot.tabs.first().map(|tab| tab.id.clone());
    }

    refresh_snapshot_sessions(state, &mut snapshot)?;
    state
        .workspaces
        .lock()
        .map_err(|_| "Workspace cache lock poisoned".to_string())?
        .insert(snapshot.session_id.clone(), snapshot.clone());
    Ok(snapshot)
}

fn ensure_workspace_snapshot_has_window(snapshot: &mut WorkspaceSnapshot) -> bool {
    if !snapshot.tabs.is_empty() {
        return false;
    }

    let window = new_workspace_tab("main".to_string());
    snapshot.active_tab_id = Some(window.id.clone());
    snapshot.tabs.push(window);
    true
}

fn ensure_session_spawn_target(
    state: &AppState,
    project_id: &str,
    workspace_session_id: &str,
    window_id: Option<String>,
    stack_id: Option<String>,
) -> Result<(String, String), String> {
    let mut snapshot = load_workspace_session_snapshot(state, project_id, workspace_session_id)?;
    let mut changed = false;

    if snapshot.tabs.is_empty() {
        let window = new_workspace_tab("main".to_string());
        snapshot.active_tab_id = Some(window.id.clone());
        snapshot.tabs.push(window);
        changed = true;
    }

    let resolved_window_id = match window_id {
        Some(id) => id,
        None => snapshot
            .active_tab_id
            .clone()
            .or_else(|| snapshot.tabs.first().map(|tab| tab.id.clone()))
            .ok_or_else(|| "Window not found".to_string())?,
    };

    let Some(target_window) = snapshot.tabs.iter_mut().find(|tab| tab.id == resolved_window_id) else {
        return Err("Window not found".to_string());
    };

    if snapshot.active_tab_id.as_deref() != Some(target_window.id.as_str()) {
        snapshot.active_tab_id = Some(target_window.id.clone());
        changed = true;
    }

    let resolved_stack_id = match stack_id {
        Some(id) if stack_exists(&target_window.root, &id) => id,
        Some(_) => return Err("Target pane not found".to_string()),
        None => target_window
            .active_pane_id
            .clone()
            .or_else(|| first_stack_id(&target_window.root))
            .ok_or_else(|| "Target pane not found".to_string())?,
    };

    if target_window.active_pane_id.as_deref() != Some(resolved_stack_id.as_str()) {
        target_window.active_pane_id = Some(resolved_stack_id.clone());
        changed = true;
    }

    if changed {
        persist_workspace_snapshot(state, snapshot)?;
    }

    Ok((resolved_window_id, resolved_stack_id))
}

#[cfg(test)]
mod tests {
    use super::{
        ClientListing, ResizePaneRequest, SessionListing, TabViewport, WindowListing,
        WorkspaceSnapshot, apply_tmux_resize_to_tab, close_pane_in_snapshot,
        current_tmux_pane_id, map_window_error_to_tab, query_flag,
        remove_session_from_snapshot, rename_window_in_snapshot,
        render_tmux_client_format, render_tmux_session_format, render_tmux_window_format,
        resolve_directional_pane_id, resolve_relative_window_id, resolve_target_window_id,
        resolve_tmux_split_direction, set_active_window_in_snapshot, split_tmux_pane,
        ensure_workspace_snapshot_has_window,
    };
    use crate::layout::{add_session_to_stack, first_stack_id, new_workspace_tab};
    use crate::layout::SplitInsertion;
    use crate::models::{LayoutNode, PaneCreatedBy, PaneLaunchState};

    #[test]
    fn ended_last_root_session_resets_tab_to_launcher() {
        let mut tab = new_workspace_tab("main".to_string());
        let stack_id = first_stack_id(&tab.root).expect("root stack");
        add_session_to_stack(&mut tab.root, &stack_id, "session-1", "Terminal");

        let mut snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            session_id: "workspace-session-1".to_string(),
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
            session_id: "workspace-session-1".to_string(),
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
            session_id: "workspace-session-1".to_string(),
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
            session_id: "workspace-session-1".to_string(),
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

    #[test]
    fn empty_session_snapshot_materializes_main_window() {
        let mut snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            session_id: "workspace-session-1".to_string(),
            active_tab_id: None,
            tabs: Vec::new(),
            sessions: Vec::new(),
        };

        assert!(ensure_workspace_snapshot_has_window(&mut snapshot));
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].title, "main");
        assert_eq!(
            snapshot.active_tab_id.as_deref(),
            Some(snapshot.tabs[0].id.as_str())
        );
    }

    #[test]
    fn existing_window_invariant_is_preserved() {
        let existing = new_workspace_tab("window-1".to_string());
        let existing_id = existing.id.clone();
        let mut snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            session_id: "workspace-session-1".to_string(),
            active_tab_id: Some(existing_id.clone()),
            tabs: vec![existing],
            sessions: Vec::new(),
        };

        assert!(!ensure_workspace_snapshot_has_window(&mut snapshot));
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].id, existing_id);
    }

    #[test]
    fn set_active_window_in_snapshot_rejects_unknown_window() {
        let existing = new_workspace_tab("main".to_string());
        let mut snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            session_id: "workspace-session-1".to_string(),
            active_tab_id: Some(existing.id.clone()),
            tabs: vec![existing],
            sessions: Vec::new(),
        };

        let error =
            set_active_window_in_snapshot(&mut snapshot, "missing-window").expect_err("missing");
        assert_eq!(error, "Window not found");
    }

    #[test]
    fn rename_window_in_snapshot_rejects_unknown_window() {
        let existing = new_workspace_tab("main".to_string());
        let mut snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            session_id: "workspace-session-1".to_string(),
            active_tab_id: Some(existing.id.clone()),
            tabs: vec![existing],
            sessions: Vec::new(),
        };

        let error = rename_window_in_snapshot(
            &mut snapshot,
            "missing-window",
            "renamed".to_string(),
        )
        .expect_err("missing");
        assert_eq!(error, "Window not found");
    }

    #[test]
    fn tab_error_mapping_matches_window_not_found() {
        assert_eq!(
            map_window_error_to_tab("Window not found".to_string()),
            "Tab not found".to_string()
        );
        assert_eq!(
            map_window_error_to_tab("Other error".to_string()),
            "Other error".to_string()
        );
    }

    #[test]
    fn split_tmux_pane_wraps_root_for_full_span() {
        let mut tab = new_workspace_tab("main".to_string());
        let root_stack_id = first_stack_id(&tab.root).expect("root stack");
        let child_pane_id = split_tmux_pane(
            &mut tab,
            &root_stack_id,
            "horizontal",
            false,
            SplitInsertion::After,
            None,
        )
        .expect("first child");
        let full_span_pane_id = split_tmux_pane(
            &mut tab,
            &child_pane_id,
            "vertical",
            true,
            SplitInsertion::Before,
            Some(30),
        )
        .expect("full-span pane");

        match &tab.root {
            LayoutNode::Split {
                direction,
                sizes,
                children,
                ..
            } => {
                assert_eq!(direction, "vertical");
                assert_eq!(sizes, &vec![30, 70]);
                assert_eq!(children.len(), 2);
                assert!(matches!(
                    &children[0],
                    LayoutNode::Stack {
                        id,
                        created_by: PaneCreatedBy::Ai,
                        ..
                    } if id == &full_span_pane_id
                ));
                assert_eq!(count_stacks(&children[1]), 2);
            }
            LayoutNode::Stack { .. } => panic!("expected full-span split root"),
        }
    }

    #[test]
    fn current_tmux_pane_defaults_to_active_pane() {
        let mut tab = new_workspace_tab("main".to_string());
        let root_stack_id = first_stack_id(&tab.root).expect("root stack");
        let new_pane_id = split_tmux_pane(
            &mut tab,
            &root_stack_id,
            &resolve_tmux_split_direction(Some("horizontal")),
            false,
            SplitInsertion::After,
            None,
        )
        .expect("new pane");
        tab.active_pane_id = Some(new_pane_id.clone());

        assert_eq!(current_tmux_pane_id(&tab, &root_stack_id), new_pane_id);
    }

    #[test]
    fn resolve_directional_pane_id_finds_neighbor_by_layout() {
        let mut tab = new_workspace_tab("main".to_string());
        let root_stack_id = first_stack_id(&tab.root).expect("root stack");
        let right_pane_id = split_tmux_pane(
            &mut tab,
            &root_stack_id,
            "horizontal",
            false,
            SplitInsertion::After,
            None,
        )
        .expect("right pane");

        assert_eq!(
            resolve_directional_pane_id(&tab, &root_stack_id, "right").expect("right neighbor"),
            right_pane_id
        );
        assert_eq!(
            resolve_directional_pane_id(&tab, &right_pane_id, "left").expect("left neighbor"),
            root_stack_id
        );
    }

    #[test]
    fn repeated_horizontal_tmux_split_follows_active_pane_chain() {
        let mut tab = new_workspace_tab("main".to_string());
        let root_stack_id = first_stack_id(&tab.root).expect("root stack");
        let first_new_pane_id = split_tmux_pane(
            &mut tab,
            &root_stack_id,
            "horizontal",
            false,
            SplitInsertion::After,
            None,
        )
        .expect("first pane");
        tab.active_pane_id = Some(first_new_pane_id.clone());
        let second_target = current_tmux_pane_id(&tab, &root_stack_id);
        let second_new_pane_id = split_tmux_pane(
            &mut tab,
            &second_target,
            "horizontal",
            false,
            SplitInsertion::After,
            None,
        )
        .expect("second pane");

        match &tab.root {
            LayoutNode::Split { children, .. } => {
                assert!(matches!(
                    &children[0],
                    LayoutNode::Stack { id, .. } if id == &root_stack_id
                ));
                assert!(matches!(
                    &children[1],
                    LayoutNode::Split {
                        direction,
                        children: nested_children,
                        ..
                    } if direction == "horizontal"
                        && matches!(
                            &nested_children[0],
                            LayoutNode::Stack { id, .. } if id == &first_new_pane_id
                        )
                        && matches!(
                            &nested_children[1],
                            LayoutNode::Stack { id, .. } if id == &second_new_pane_id
                        )
                ));
            }
            LayoutNode::Stack { .. } => panic!("expected chained horizontal split"),
        }
    }

    #[test]
    fn resize_pane_expands_target_horizontally() {
        let mut tab = new_workspace_tab("main".to_string());
        let root_stack_id = first_stack_id(&tab.root).expect("root stack");
        let right_pane_id = split_tmux_pane(
            &mut tab,
            &root_stack_id,
            "horizontal",
            false,
            SplitInsertion::After,
            None,
        )
        .expect("right pane");

        let changed = apply_tmux_resize_to_tab(
            &mut tab,
            &right_pane_id,
            TabViewport {
                width: 200.0,
                height: 100.0,
            },
            &ResizePaneRequest {
                direction: Some("left".to_string()),
                adjustment: Some(20),
                ..ResizePaneRequest::default()
            },
        )
        .expect("resize result");

        assert!(changed);
        match &tab.root {
            LayoutNode::Split { sizes, .. } => assert_eq!(sizes, &vec![40, 60]),
            LayoutNode::Stack { .. } => panic!("expected split root"),
        }
    }

    #[test]
    fn resolve_target_window_id_accepts_title_and_indices() {
        let first = new_workspace_tab("main".to_string());
        let second = new_workspace_tab("tmux".to_string());
        let snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            session_id: "workspace-session-1".to_string(),
            active_tab_id: Some(first.id.clone()),
            tabs: vec![first.clone(), second.clone()],
            sessions: Vec::new(),
        };

        assert_eq!(
            resolve_target_window_id(&snapshot, Some("tmux"), &first.id, None)
                .expect("window by title"),
            second.id
        );
        assert_eq!(
            resolve_target_window_id(&snapshot, Some("0"), &first.id, None)
                .expect("zero-based window index"),
            first.id
        );
        assert_eq!(
            resolve_target_window_id(&snapshot, Some("+1"), &first.id, None)
                .expect("relative next window"),
            second.id
        );
        assert_eq!(
            resolve_target_window_id(&snapshot, Some("!"), &second.id, Some(first.id.as_str()))
                .expect("last window alias"),
            first.id
        );
    }

    #[test]
    fn render_tmux_window_format_supports_core_tokens() {
        let rendered = render_tmux_window_format(
            "#{window_index}:#{window_name}:#{window_active}:#{session_name}",
            &WindowListing {
                window_id: "window-1".to_string(),
                window_index: 2,
                window_name: "build".to_string(),
                window_active: true,
                session_name: "session-1".to_string(),
            },
        );
        assert_eq!(rendered, "2:build:1:session-1");
    }

    #[test]
    fn render_tmux_session_format_supports_core_tokens() {
        let rendered = render_tmux_session_format(
            "#{session_id}:#{session_name}:#{session_windows}:#{session_attached}",
            &SessionListing {
                session_id: "$session-1".to_string(),
                session_name: "session-1".to_string(),
                session_windows: 3,
                session_attached: true,
            },
        );
        assert_eq!(rendered, "$session-1:session-1:3:1");
    }

    #[test]
    fn render_tmux_client_format_supports_core_tokens() {
        let rendered = render_tmux_client_format(
            "#{client_name}:#{client_pid}:#{client_tty}:#{client_session}:#{window_id}:#{pane_id}",
            &ClientListing {
                client_name: "workspace-terminal-client".to_string(),
                client_pid: 42,
                client_tty: "workspace-terminal".to_string(),
                session_name: "session-1".to_string(),
                window_id: "@1".to_string(),
                pane_id: "pane-1".to_string(),
            },
        );
        assert_eq!(
            rendered,
            "workspace-terminal-client:42:workspace-terminal:session-1:@1:%pane-1"
        );
    }

    #[test]
    fn query_flag_understands_bool_values() {
        assert!(query_flag("global=true&valueOnly=1", "global"));
        assert!(query_flag("global=true&valueOnly=1", "valueOnly"));
        assert!(!query_flag("global=false", "global"));
        assert!(!query_flag("", "global"));
    }

    #[test]
    fn resolve_relative_window_id_wraps_across_tabs() {
        let first = new_workspace_tab("main".to_string());
        let second = new_workspace_tab("tmux".to_string());
        let third = new_workspace_tab("logs".to_string());
        let snapshot = WorkspaceSnapshot {
            project_id: "project-1".to_string(),
            session_id: "workspace-session-1".to_string(),
            active_tab_id: Some(first.id.clone()),
            tabs: vec![first.clone(), second.clone(), third.clone()],
            sessions: Vec::new(),
        };

        assert_eq!(
            resolve_relative_window_id(&snapshot, &third.id, 1).expect("wrap next"),
            first.id
        );
        assert_eq!(
            resolve_relative_window_id(&snapshot, &first.id, -1).expect("wrap previous"),
            third.id
        );
    }

    fn count_stacks(node: &LayoutNode) -> usize {
        match node {
            LayoutNode::Stack { .. } => 1,
            LayoutNode::Split { children, .. } => children.iter().map(count_stacks).sum(),
        }
    }
}

