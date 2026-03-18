use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum LaunchProfile {
    #[default]
    Terminal,
    Claude,
    ClaudeUnsafe,
    Codex,
    CodexFullAuto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub color: String,
    pub icon: Option<String>,
    pub last_opened_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProjectResult {
    pub deleted_project_id: String,
    pub next_project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Starting,
    Running,
    Exited,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSession {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub program: String,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub launch_profile: LaunchProfile,
    #[serde(default)]
    pub tmux_shim_enabled: bool,
    pub cwd: String,
    pub status: SessionStatus,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackItem {
    pub id: String,
    pub kind: String,
    pub session_id: Option<String>,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PaneCreatedBy {
    #[default]
    User,
    Ai,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PaneLaunchState {
    #[default]
    Unlaunched,
    Launched,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SplitZoneKind {
    #[default]
    Default,
    AiWorkspace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LayoutNode {
    #[serde(rename = "split")]
    Split {
        id: String,
        direction: String,
        #[serde(default)]
        zone_kind: SplitZoneKind,
        sizes: Vec<u16>,
        children: Vec<LayoutNode>,
    },
    #[serde(rename = "stack")]
    Stack {
        id: String,
        #[serde(default)]
        pane_ordinal: u32,
        #[serde(default)]
        pane_label: String,
        #[serde(default)]
        created_by: PaneCreatedBy,
        #[serde(default)]
        launch_state: PaneLaunchState,
        #[serde(default)]
        source_pane_id: Option<String>,
        active_item_id: String,
        items: Vec<StackItem>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTab {
    pub id: String,
    pub title: String,
    pub root: LayoutNode,
    #[serde(default)]
    pub next_pane_ordinal: u32,
    #[serde(default)]
    pub active_pane_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    pub project_id: String,
    pub active_tab_id: Option<String>,
    pub tabs: Vec<WorkspaceTab>,
    pub sessions: Vec<TerminalSession>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOutputEvent {
    pub session_id: String,
    pub chunk: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChangedEvent {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExitEvent {
    pub session_id: String,
    pub exit_code: Option<i32>,
}
