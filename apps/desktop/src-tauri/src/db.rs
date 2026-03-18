use std::{fs, path::PathBuf};

use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::{
    layout::{new_workspace_tab, normalize_tab, reset_tab_layout},
    models::{Project, SessionStatus, TerminalSession, WorkspaceSnapshot, WorkspaceTab},
};

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn new(base_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&base_dir).map_err(|err| err.to_string())?;
        let db_path = base_dir.join("workspace-terminal.sqlite3");
        let conn = Connection::open(db_path).map_err(|err| err.to_string())?;

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
              id TEXT PRIMARY KEY,
              name TEXT NOT NULL,
              path TEXT NOT NULL UNIQUE,
              color TEXT NOT NULL,
              icon TEXT,
              last_opened_at TEXT,
              created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS workspaces (
              project_id TEXT PRIMARY KEY,
              active_tab_id TEXT,
              tabs_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
              id TEXT PRIMARY KEY,
              project_id TEXT NOT NULL,
              title TEXT NOT NULL,
              shell TEXT NOT NULL,
              program TEXT,
              args_json TEXT,
              cwd TEXT NOT NULL,
              status TEXT NOT NULL,
              started_at TEXT,
              ended_at TEXT,
              exit_code INTEGER
            );
            "#,
        )
        .map_err(|err| err.to_string())?;

        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN program TEXT", []);
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN args_json TEXT", []);

        Ok(Self { conn })
    }

    pub fn list_projects(&self) -> Result<Vec<Project>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, path, color, icon, last_opened_at, created_at
                 FROM projects ORDER BY COALESCE(last_opened_at, created_at) DESC, name ASC",
            )
            .map_err(|err| err.to_string())?;

        let rows = stmt
            .query_map([], |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    path: row.get(2)?,
                    color: row.get(3)?,
                    icon: row.get(4)?,
                    last_opened_at: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|err| err.to_string())?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())
    }

    pub fn create_project(&self, name: &str, path: &str) -> Result<Project, String> {
        let id = Uuid::new_v4().to_string();
        let created_at = now_iso();
        let color = "#4f8cff".to_string();

        self.conn
            .execute(
                "INSERT INTO projects (id, name, path, color, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, name, path, color, created_at],
            )
            .map_err(|err| err.to_string())?;

        let default_tab = new_workspace_tab("main".to_string());
        let active_tab_id = default_tab.id.clone();

        let tabs = serde_json::to_string(&vec![default_tab]).map_err(|err| err.to_string())?;
        self.conn
            .execute(
                "INSERT INTO workspaces (project_id, active_tab_id, tabs_json) VALUES (?1, ?2, ?3)",
                params![id, active_tab_id, tabs],
            )
            .map_err(|err| err.to_string())?;

        self.get_project(&id)?
            .ok_or_else(|| "Failed to reload project".to_string())
    }

    pub fn get_project(&self, project_id: &str) -> Result<Option<Project>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, path, color, icon, last_opened_at, created_at
                 FROM projects WHERE id = ?1",
            )
            .map_err(|err| err.to_string())?;

        let result = stmt.query_row(params![project_id], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
                color: row.get(3)?,
                icon: row.get(4)?,
                last_opened_at: row.get(5)?,
                created_at: row.get(6)?,
            })
        });

        match result {
            Ok(project) => Ok(Some(project)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    pub fn touch_project(&self, project_id: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE projects SET last_opened_at = ?1 WHERE id = ?2",
                params![now_iso(), project_id],
            )
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn load_workspace(&self, project_id: &str) -> Result<WorkspaceSnapshot, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT active_tab_id, tabs_json FROM workspaces WHERE project_id = ?1")
            .map_err(|err| err.to_string())?;

        let (active_tab_id, tabs_json): (Option<String>, String) = stmt
            .query_row(params![project_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|err| err.to_string())?;

        let mut tabs: Vec<WorkspaceTab> =
            serde_json::from_str(&tabs_json).map_err(|err| err.to_string())?;
        let mut normalized = false;

        for tab in tabs.iter_mut() {
            normalized |= normalize_tab(tab);
        }

        let active_tab_id = match active_tab_id {
            Some(ref tab_id) if tabs.iter().any(|tab| tab.id == *tab_id) => Some(tab_id.clone()),
            _ => tabs.first().map(|tab| tab.id.clone()),
        };

        if normalized {
            let normalized_snapshot = WorkspaceSnapshot {
                project_id: project_id.to_string(),
                active_tab_id: active_tab_id.clone(),
                tabs: tabs.clone(),
                sessions: self.list_sessions(project_id)?,
            };
            self.save_workspace(&normalized_snapshot)?;
        }
        let sessions = self.list_sessions(project_id)?;

        Ok(WorkspaceSnapshot {
            project_id: project_id.to_string(),
            active_tab_id,
            tabs,
            sessions,
        })
    }

    pub fn save_workspace(&self, snapshot: &WorkspaceSnapshot) -> Result<(), String> {
        let tabs_json = serde_json::to_string(&snapshot.tabs).map_err(|err| err.to_string())?;
        self.conn
            .execute(
                "UPDATE workspaces SET active_tab_id = ?1, tabs_json = ?2 WHERE project_id = ?3",
                params![snapshot.active_tab_id, tabs_json, snapshot.project_id],
            )
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn save_workspace_outline(&self, snapshot: &WorkspaceSnapshot) -> Result<(), String> {
        let mut tabs = snapshot.tabs.clone();
        if tabs.is_empty() {
            tabs.push(new_workspace_tab("main".to_string()));
        } else {
            for tab in tabs.iter_mut() {
                reset_tab_layout(tab);
            }
        }

        let active_tab_id = match &snapshot.active_tab_id {
            Some(tab_id) if tabs.iter().any(|tab| tab.id == *tab_id) => Some(tab_id.clone()),
            _ => tabs.first().map(|tab| tab.id.clone()),
        };

        let tabs_json = serde_json::to_string(&tabs).map_err(|err| err.to_string())?;
        self.conn
            .execute(
                "UPDATE workspaces SET active_tab_id = ?1, tabs_json = ?2 WHERE project_id = ?3",
                params![active_tab_id, tabs_json, snapshot.project_id],
            )
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn list_sessions(&self, project_id: &str) -> Result<Vec<TerminalSession>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, title, COALESCE(program, shell), args_json, cwd, status, started_at, ended_at, exit_code
                 FROM sessions WHERE project_id = ?1 ORDER BY COALESCE(started_at, ended_at) DESC",
            )
            .map_err(|err| err.to_string())?;

        let rows = stmt
            .query_map(params![project_id], |row| {
                let status_str: String = row.get(6)?;
                let args_json: Option<String> = row.get(4)?;
                Ok(TerminalSession {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    title: row.get(2)?,
                    program: row.get(3)?,
                    args: args_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()
                        .map_err(|err| {
                            rusqlite::Error::FromSqlConversionFailure(
                                4,
                                rusqlite::types::Type::Text,
                                Box::new(err),
                            )
                        })?,
                    cwd: row.get(5)?,
                    status: match status_str.as_str() {
                        "running" => SessionStatus::Running,
                        "exited" => SessionStatus::Exited,
                        "failed" => SessionStatus::Failed,
                        _ => SessionStatus::Starting,
                    },
                    started_at: row.get(7)?,
                    ended_at: row.get(8)?,
                    exit_code: row.get(9)?,
                })
            })
            .map_err(|err| err.to_string())?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())
    }

    pub fn upsert_session(&self, session: &TerminalSession) -> Result<(), String> {
        let args_json = session
            .args
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|err| err.to_string())?;

        self.conn
            .execute(
                r#"
                INSERT INTO sessions (id, project_id, title, shell, program, args_json, cwd, status, started_at, ended_at, exit_code)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(id) DO UPDATE SET
                  title = excluded.title,
                  shell = excluded.shell,
                  program = excluded.program,
                  args_json = excluded.args_json,
                  cwd = excluded.cwd,
                  status = excluded.status,
                  started_at = excluded.started_at,
                  ended_at = excluded.ended_at,
                  exit_code = excluded.exit_code
                "#,
                params![
                    session.id,
                    session.project_id,
                    session.title,
                    session.program,
                    session.program,
                    args_json,
                    session.cwd,
                    status_to_str(&session.status),
                    session.started_at,
                    session.ended_at,
                    session.exit_code
                ],
            )
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn update_session_exit(
        &self,
        session_id: &str,
        status: SessionStatus,
        exit_code: Option<i32>,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE sessions SET status = ?1, ended_at = ?2, exit_code = ?3 WHERE id = ?4",
                params![status_to_str(&status), now_iso(), exit_code, session_id],
            )
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn mark_stale_sessions(&self) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE sessions
                 SET status = ?1, ended_at = ?2
                 WHERE status IN ('starting', 'running')",
                params![status_to_str(&SessionStatus::Failed), now_iso()],
            )
            .map_err(|err| err.to_string())?;
        Ok(())
    }
}

pub fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    secs.to_string()
}

pub fn status_to_str(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Starting => "starting",
        SessionStatus::Running => "running",
        SessionStatus::Exited => "exited",
        SessionStatus::Failed => "failed",
    }
}
