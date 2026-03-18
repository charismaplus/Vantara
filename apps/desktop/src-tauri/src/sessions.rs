use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
};

use portable_pty::{Child, ChildKiller, CommandBuilder, PtySize, native_pty_system};
use tauri::{AppHandle, Emitter};

use crate::{
    db::{Database, now_iso},
    models::{SessionExitEvent, SessionOutputEvent, SessionStatus, TerminalSession},
};

struct SessionRuntime {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

#[derive(Clone, Default)]
pub struct SessionManager {
    runtimes: Arc<Mutex<HashMap<String, SessionRuntime>>>,
}

impl SessionManager {
    pub fn create(
        &self,
        app: AppHandle,
        db: Arc<Mutex<Database>>,
        mut session: TerminalSession,
    ) -> Result<TerminalSession, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| err.to_string())?;

        let program_path = resolve_program(&session.program);
        let mut cmd = CommandBuilder::new(program_path);
        cmd.cwd(session.cwd.clone());
        if let Some(args) = &session.args {
            for arg in args {
                cmd.arg(arg);
            }
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| err.to_string())?;
        let killer = child.clone_killer();

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| err.to_string())?;
        let writer = pair.master.take_writer().map_err(|err| err.to_string())?;

        session.status = SessionStatus::Running;
        session.started_at = Some(now_iso());
        session.ended_at = None;
        session.exit_code = None;

        {
            let db_guard = db
                .lock()
                .map_err(|_| "Database lock poisoned".to_string())?;
            db_guard.upsert_session(&session)?;
        }

        let session_id = session.id.clone();
        let exit_session_id = session.id.clone();
        let runtimes = self.runtimes.clone();

        self.runtimes
            .lock()
            .map_err(|_| "Session lock poisoned".to_string())?
            .insert(
                session.id.clone(),
                SessionRuntime {
                    master: pair.master,
                    writer,
                    killer,
                },
            );

        let reader_session_id = session_id.clone();
        let reader_app = app.clone();
        thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let chunk = String::from_utf8_lossy(&buffer[..read]).to_string();
                        let _ = reader_app.emit(
                            "session-output",
                            SessionOutputEvent {
                                session_id: reader_session_id.clone(),
                                chunk,
                            },
                        );
                    }
                    Err(_) => break,
                }
            }
        });

        let wait_app = app;
        let wait_db = db;
        thread::spawn(move || {
            let exit_code = wait_for_exit(child);
            let status = if exit_code == Some(0) {
                SessionStatus::Exited
            } else {
                SessionStatus::Failed
            };

            let event = SessionExitEvent {
                session_id: exit_session_id.clone(),
                exit_code,
            };
            let _ = wait_app.emit("session-exit", event);

            if let Ok(db_guard) = wait_db.lock() {
                let _ = db_guard.update_session_exit(&exit_session_id, status, exit_code);
            }
            let _ = runtimes.lock().map(|mut map| map.remove(&exit_session_id));
        });

        Ok(session)
    }

    pub fn write_input(&self, session_id: &str, input: &str) -> Result<(), String> {
        let mut runtimes = self
            .runtimes
            .lock()
            .map_err(|_| "Session lock poisoned".to_string())?;
        let runtime = runtimes
            .get_mut(session_id)
            .ok_or_else(|| "Session not found".to_string())?;

        runtime
            .writer
            .write_all(input.as_bytes())
            .map_err(|err| err.to_string())?;
        runtime.writer.flush().map_err(|err| err.to_string())
    }

    pub fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let mut runtimes = self
            .runtimes
            .lock()
            .map_err(|_| "Session lock poisoned".to_string())?;
        let runtime = runtimes
            .get_mut(session_id)
            .ok_or_else(|| "Session not found".to_string())?;

        runtime
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| err.to_string())
    }

    pub fn terminate(&self, session_id: &str) -> Result<(), String> {
        let mut runtimes = self
            .runtimes
            .lock()
            .map_err(|_| "Session lock poisoned".to_string())?;
        let mut runtime = runtimes
            .remove(session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        runtime.killer.kill().map_err(|err| err.to_string())
    }
}

fn wait_for_exit(mut child: Box<dyn Child + Send + Sync>) -> Option<i32> {
    match child.wait() {
        Ok(status) => i32::try_from(status.exit_code()).ok(),
        Err(_) => None,
    }
}

fn resolve_program(program: &str) -> String {
    match program {
        "cmd" => "cmd.exe".to_string(),
        "pwsh" => "pwsh.exe".to_string(),
        "bash" => "bash.exe".to_string(),
        "powershell" => "powershell.exe".to_string(),
        _ => program.to_string(),
    }
}
