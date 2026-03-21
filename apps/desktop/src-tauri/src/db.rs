use std::{fs, path::PathBuf};

use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::models::{
    LaunchProfile, Project, SessionStatus, TerminalSession, WorkspaceSession,
    WorkspaceSessionCreatedBy,
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

            CREATE TABLE IF NOT EXISTS workspace_sessions (
              id TEXT PRIMARY KEY,
              project_id TEXT NOT NULL,
              name TEXT NOT NULL,
              created_by TEXT NOT NULL DEFAULT 'user',
              source_session_id TEXT,
              last_opened_at TEXT,
              created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
              id TEXT PRIMARY KEY,
              project_id TEXT NOT NULL,
              workspace_session_id TEXT,
              window_id TEXT,
              title TEXT NOT NULL,
              shell TEXT NOT NULL,
              program TEXT,
              args_json TEXT,
              launch_profile TEXT,
              tmux_shim_enabled INTEGER NOT NULL DEFAULT 0,
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
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN launch_profile TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE sessions ADD COLUMN tmux_shim_enabled INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN workspace_session_id TEXT", []);
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN window_id TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE workspace_sessions ADD COLUMN created_by TEXT NOT NULL DEFAULT 'user'",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE workspace_sessions ADD COLUMN source_session_id TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE workspace_sessions ADD COLUMN last_opened_at TEXT",
            [],
        );

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

        self.create_workspace_session(
            &id,
            Some("main".to_string()),
            WorkspaceSessionCreatedBy::User,
            None,
        )?;

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

    pub fn rename_project(&self, project_id: &str, name: &str) -> Result<Project, String> {
        self.conn
            .execute(
                "UPDATE projects SET name = ?1 WHERE id = ?2",
                params![name, project_id],
            )
            .map_err(|err| err.to_string())?;

        self.get_project(project_id)?
            .ok_or_else(|| "Project not found".to_string())
    }

    pub fn delete_project(&mut self, project_id: &str) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|err| err.to_string())?;
        tx.execute(
            "DELETE FROM sessions WHERE project_id = ?1",
            params![project_id],
        )
        .map_err(|err| err.to_string())?;
        tx.execute(
            "DELETE FROM workspace_sessions WHERE project_id = ?1",
            params![project_id],
        )
        .map_err(|err| err.to_string())?;
        tx.execute("DELETE FROM projects WHERE id = ?1", params![project_id])
            .map_err(|err| err.to_string())?;
        tx.commit().map_err(|err| err.to_string())
    }

    pub fn ensure_default_workspace_session(&self, project_id: &str) -> Result<(), String> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM workspace_sessions WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .map_err(|err| err.to_string())?;

        if count == 0 {
            self.create_workspace_session(
                project_id,
                Some("main".to_string()),
                WorkspaceSessionCreatedBy::User,
                None,
            )?;
        }

        Ok(())
    }

    pub fn list_workspace_sessions(
        &self,
        project_id: &str,
    ) -> Result<Vec<WorkspaceSession>, String> {
        self.ensure_default_workspace_session(project_id)?;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, name, created_by, source_session_id, last_opened_at, created_at
                 FROM workspace_sessions
                 WHERE project_id = ?1
                 ORDER BY created_at ASC, id ASC",
            )
            .map_err(|err| err.to_string())?;

        let rows = stmt
            .query_map(params![project_id], |row| {
                Ok(WorkspaceSession {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    name: row.get(2)?,
                    created_by: parse_workspace_session_created_by(row.get::<_, String>(3)?),
                    source_session_id: row.get(4)?,
                    last_opened_at: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|err| err.to_string())?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())
    }

    pub fn get_workspace_session(
        &self,
        project_id: &str,
        workspace_session_id: &str,
    ) -> Result<Option<WorkspaceSession>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, name, created_by, source_session_id, last_opened_at, created_at
                 FROM workspace_sessions
                 WHERE project_id = ?1 AND id = ?2",
            )
            .map_err(|err| err.to_string())?;

        let result = stmt.query_row(params![project_id, workspace_session_id], |row| {
            Ok(WorkspaceSession {
                id: row.get(0)?,
                project_id: row.get(1)?,
                name: row.get(2)?,
                created_by: parse_workspace_session_created_by(row.get::<_, String>(3)?),
                source_session_id: row.get(4)?,
                last_opened_at: row.get(5)?,
                created_at: row.get(6)?,
            })
        });

        match result {
            Ok(session) => Ok(Some(session)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    pub fn create_workspace_session(
        &self,
        project_id: &str,
        name: Option<String>,
        created_by: WorkspaceSessionCreatedBy,
        source_session_id: Option<String>,
    ) -> Result<WorkspaceSession, String> {
        let id = Uuid::new_v4().to_string();
        let created_at = now_iso();
        let session_name = name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| self.next_workspace_session_name(project_id));

        self.conn
            .execute(
                "INSERT INTO workspace_sessions (id, project_id, name, created_by, source_session_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    project_id,
                    session_name,
                    workspace_session_created_by_to_str(&created_by),
                    source_session_id,
                    created_at
                ],
            )
            .map_err(|err| err.to_string())?;

        self.get_workspace_session(project_id, &id)?
            .ok_or_else(|| "Workspace session not found".to_string())
    }

    pub fn rename_workspace_session(
        &self,
        project_id: &str,
        workspace_session_id: &str,
        name: &str,
    ) -> Result<WorkspaceSession, String> {
        self.conn
            .execute(
                "UPDATE workspace_sessions SET name = ?1 WHERE project_id = ?2 AND id = ?3",
                params![name, project_id, workspace_session_id],
            )
            .map_err(|err| err.to_string())?;

        self.get_workspace_session(project_id, workspace_session_id)?
            .ok_or_else(|| "Workspace session not found".to_string())
    }

    pub fn touch_workspace_session(
        &self,
        project_id: &str,
        workspace_session_id: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE workspace_sessions SET last_opened_at = ?1 WHERE project_id = ?2 AND id = ?3",
                params![now_iso(), project_id, workspace_session_id],
            )
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn delete_workspace_session(
        &mut self,
        project_id: &str,
        workspace_session_id: &str,
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|err| err.to_string())?;
        tx.execute(
            "DELETE FROM sessions WHERE project_id = ?1 AND workspace_session_id = ?2",
            params![project_id, workspace_session_id],
        )
        .map_err(|err| err.to_string())?;
        tx.execute(
            "DELETE FROM workspace_sessions WHERE project_id = ?1 AND id = ?2",
            params![project_id, workspace_session_id],
        )
        .map_err(|err| err.to_string())?;
        tx.commit().map_err(|err| err.to_string())?;
        self.ensure_default_workspace_session(project_id)?;
        Ok(())
    }

    pub fn list_sessions(&self, project_id: &str) -> Result<Vec<TerminalSession>, String> {
        self.list_sessions_for_project(project_id)
    }

    pub fn list_sessions_for_project(&self, project_id: &str) -> Result<Vec<TerminalSession>, String> {
        self.list_sessions_query(
            "SELECT id, project_id, workspace_session_id, window_id, title, COALESCE(program, shell), args_json, launch_profile, tmux_shim_enabled, cwd, status, started_at, ended_at, exit_code
             FROM sessions WHERE project_id = ?1 ORDER BY COALESCE(started_at, ended_at) DESC",
            params![project_id],
        )
    }

    pub fn list_sessions_for_workspace_session(
        &self,
        workspace_session_id: &str,
    ) -> Result<Vec<TerminalSession>, String> {
        self.list_sessions_query(
            "SELECT id, project_id, workspace_session_id, window_id, title, COALESCE(program, shell), args_json, launch_profile, tmux_shim_enabled, cwd, status, started_at, ended_at, exit_code
             FROM sessions WHERE workspace_session_id = ?1 ORDER BY COALESCE(started_at, ended_at) DESC",
            params![workspace_session_id],
        )
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<TerminalSession>, String> {
        Ok(self
            .list_sessions_query(
                "SELECT id, project_id, workspace_session_id, window_id, title, COALESCE(program, shell), args_json, launch_profile, tmux_shim_enabled, cwd, status, started_at, ended_at, exit_code
                 FROM sessions WHERE id = ?1",
                params![session_id],
            )?
            .into_iter()
            .next())
    }

    fn list_sessions_query<P>(&self, sql: &str, params: P) -> Result<Vec<TerminalSession>, String>
    where
        P: rusqlite::Params,
    {
        let mut stmt = self.conn.prepare(sql).map_err(|err| err.to_string())?;

        let rows = stmt
            .query_map(params, |row| {
                let args_json: Option<String> = row.get(6)?;
                let launch_profile_str: Option<String> = row.get(7)?;
                let tmux_shim_enabled: i64 = row.get(8)?;
                let status_str: String = row.get(10)?;
                Ok(TerminalSession {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    workspace_session_id: row.get(2)?,
                    window_id: row.get(3)?,
                    title: row.get(4)?,
                    program: row.get(5)?,
                    args: args_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()
                        .map_err(|err| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(err),
                            )
                        })?,
                    launch_profile: parse_launch_profile(launch_profile_str.as_deref()),
                    tmux_shim_enabled: tmux_shim_enabled != 0,
                    cwd: row.get(9)?,
                    status: match status_str.as_str() {
                        "running" => SessionStatus::Running,
                        "exited" => SessionStatus::Exited,
                        "failed" => SessionStatus::Failed,
                        _ => SessionStatus::Starting,
                    },
                    started_at: row.get(11)?,
                    ended_at: row.get(12)?,
                    exit_code: row.get(13)?,
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
                INSERT INTO sessions (id, project_id, workspace_session_id, window_id, title, shell, program, args_json, launch_profile, tmux_shim_enabled, cwd, status, started_at, ended_at, exit_code)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                ON CONFLICT(id) DO UPDATE SET
                  workspace_session_id = excluded.workspace_session_id,
                  window_id = excluded.window_id,
                  title = excluded.title,
                  shell = excluded.shell,
                  program = excluded.program,
                  args_json = excluded.args_json,
                  launch_profile = excluded.launch_profile,
                  tmux_shim_enabled = excluded.tmux_shim_enabled,
                  cwd = excluded.cwd,
                  status = excluded.status,
                  started_at = excluded.started_at,
                  ended_at = excluded.ended_at,
                  exit_code = excluded.exit_code
                "#,
                params![
                    session.id,
                    session.project_id,
                    session.workspace_session_id,
                    session.window_id,
                    session.title,
                    session.program,
                    session.program,
                    args_json,
                    launch_profile_to_str(&session.launch_profile),
                    if session.tmux_shim_enabled { 1_i64 } else { 0_i64 },
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

    fn next_workspace_session_name(&self, project_id: &str) -> String {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM workspace_sessions WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        format!("session-{}", count + 1)
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

fn launch_profile_to_str(profile: &LaunchProfile) -> &'static str {
    match profile {
        LaunchProfile::Terminal => "terminal",
        LaunchProfile::Claude => "claude",
        LaunchProfile::ClaudeUnsafe => "claudeUnsafe",
        LaunchProfile::Codex => "codex",
        LaunchProfile::CodexFullAuto => "codexFullAuto",
    }
}

fn parse_launch_profile(raw: Option<&str>) -> LaunchProfile {
    match raw.unwrap_or("terminal") {
        "claude" => LaunchProfile::Claude,
        "claudeUnsafe" => LaunchProfile::ClaudeUnsafe,
        "codex" => LaunchProfile::Codex,
        "codexFullAuto" => LaunchProfile::CodexFullAuto,
        _ => LaunchProfile::Terminal,
    }
}

fn workspace_session_created_by_to_str(created_by: &WorkspaceSessionCreatedBy) -> &'static str {
    match created_by {
        WorkspaceSessionCreatedBy::User => "user",
        WorkspaceSessionCreatedBy::Ai => "ai",
    }
}

fn parse_workspace_session_created_by(raw: String) -> WorkspaceSessionCreatedBy {
    match raw.as_str() {
        "ai" => WorkspaceSessionCreatedBy::Ai,
        _ => WorkspaceSessionCreatedBy::User,
    }
}
