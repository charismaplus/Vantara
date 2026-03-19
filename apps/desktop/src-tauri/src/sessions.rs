#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    process::{Child as ProcessChild, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use portable_pty::{Child, ChildKiller, CommandBuilder, PtySize, native_pty_system};
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    db::{Database, now_iso},
    models::{SessionExitEvent, SessionOutputEvent, SessionStatus, TerminalSession},
};

const TMUX_SHELL_READY_PHASE1_TIMEOUT: Duration = Duration::from_millis(1_500);
const TMUX_SHELL_READY_PHASE2_TIMEOUT: Duration = Duration::from_millis(2_000);
const TMUX_SHELL_READY_POLL: Duration = Duration::from_millis(25);
const TMUX_SHELL_READY_SETTLE: Duration = Duration::from_millis(100);
const OUTPUT_TAIL_CHAR_LIMIT: usize = 32_768;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionShellKind {
    #[default]
    Default,
    TmuxGitBashInteractive,
    TmuxGitBashCommand,
}

impl SessionShellKind {
    fn requires_tmux_shell_ready_guard(self) -> bool {
        matches!(self, Self::TmuxGitBashInteractive)
    }
}

#[derive(Default)]
pub struct SessionCreateOptions {
    pub extra_env: Option<HashMap<String, String>>,
    pub shell_kind: SessionShellKind,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SessionCaptureOptions {
    pub include_escape: bool,
    pub join_lines: bool,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
}

#[derive(Default)]
pub struct SessionPipeOptions {
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub pipe_output: bool,
    pub pipe_input: bool,
    pub only_if_none: bool,
}

struct SessionPipeRuntime {
    child: ProcessChild,
    stdin: Option<ChildStdin>,
    pipe_output: bool,
}

struct SessionRuntime {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    process_id: Option<u32>,
    shell_kind: SessionShellKind,
    shell_ready: bool,
    started_at: Instant,
    last_output_at: Instant,
    output_tail: String,
    pipe: Option<SessionPipeRuntime>,
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
        options: SessionCreateOptions,
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

        let mut cmd = build_command(&session.program, session.args.as_deref());
        cmd.cwd(session.cwd.clone());
        apply_runtime_env(&mut cmd, options.extra_env.as_ref());
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| err.to_string())?;
        let process_id = child.process_id();
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
        let reader_runtimes = self.runtimes.clone();
        let now = Instant::now();
        let shell_kind = options.shell_kind;

        self.runtimes
            .lock()
            .map_err(|_| "Session lock poisoned".to_string())?
            .insert(
                session.id.clone(),
                SessionRuntime {
                    master: pair.master,
                    writer,
                    killer,
                    process_id,
                    shell_kind,
                    shell_ready: !shell_kind.requires_tmux_shell_ready_guard(),
                    started_at: now,
                    last_output_at: now,
                    output_tail: String::new(),
                    pipe: None,
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
                        if let Ok(mut runtimes) = reader_runtimes.lock() {
                            if let Some(runtime) = runtimes.get_mut(&reader_session_id) {
                                runtime.last_output_at = Instant::now();
                                push_output_tail(&mut runtime.output_tail, &chunk);
                                if let Some(pipe) = runtime.pipe.as_mut() {
                                    if pipe.pipe_output {
                                        if let Some(stdin) = pipe.stdin.as_mut() {
                                            let _ = stdin.write_all(chunk.as_bytes());
                                            let _ = stdin.flush();
                                        }
                                    }
                                }
                            }
                        }
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

            if let Ok(db_guard) = wait_db.lock() {
                let _ = db_guard.update_session_exit(&exit_session_id, status, exit_code);
            }
            let _ = runtimes.lock().map(|mut map| map.remove(&exit_session_id));

            if let Some(state) = wait_app.try_state::<crate::AppState>() {
                let _ = crate::handle_runtime_session_exit(state.inner(), &exit_session_id);
            }

            let event = SessionExitEvent {
                session_id: exit_session_id.clone(),
                exit_code,
            };
            let _ = wait_app.emit("session-exit", event);
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
        let runtime = runtimes
            .remove(session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        let session_id = session_id.to_string();
        thread::spawn(move || {
            let _ = terminate_runtime(&session_id, runtime);
        });
        Ok(())
    }

    pub fn configure_pipe(&self, session_id: &str, options: SessionPipeOptions) -> Result<(), String> {
        let mut runtimes = self
            .runtimes
            .lock()
            .map_err(|_| "Session lock poisoned".to_string())?;
        let runtime = runtimes
            .get_mut(session_id)
            .ok_or_else(|| "Session not found".to_string())?;

        if options.only_if_none && runtime.pipe.is_some() {
            return Ok(());
        }

        if let Some(mut existing_pipe) = runtime.pipe.take() {
            let _ = existing_pipe.child.kill();
            let _ = existing_pipe.child.wait();
        }

        let Some(program) = options.program else {
            return Ok(());
        };

        let mut command = Command::new(program);
        if let Some(args) = options.args {
            command.args(args);
        }
        if let Some(cwd) = options.cwd {
            command.current_dir(cwd);
        }
        if let Some(env) = options.env {
            command.envs(env);
        }
        if options.pipe_output {
            command.stdin(Stdio::piped());
        }
        if options.pipe_input {
            command.stdout(Stdio::piped());
        }
        #[cfg(windows)]
        {
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().map_err(|err| err.to_string())?;
        let child_stdin = child.stdin.take();

        if options.pipe_input {
            if let Some(mut stdout) = child.stdout.take() {
                let runtimes = self.runtimes.clone();
                let session_id = session_id.to_string();
                thread::spawn(move || {
                    let mut buffer = [0_u8; 4096];
                    loop {
                        match stdout.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(read) => {
                                if let Ok(mut runtimes) = runtimes.lock() {
                                    if let Some(runtime) = runtimes.get_mut(&session_id) {
                                        let _ = runtime.writer.write_all(&buffer[..read]);
                                        let _ = runtime.writer.flush();
                                    } else {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
            }
        }

        runtime.pipe = Some(SessionPipeRuntime {
            child,
            stdin: child_stdin,
            pipe_output: options.pipe_output,
        });
        Ok(())
    }

    pub fn terminate_all(&self) -> Result<(), String> {
        let runtimes = {
            let mut guard = self
                .runtimes
                .lock()
                .map_err(|_| "Session lock poisoned".to_string())?;
            guard.drain().collect::<Vec<_>>()
        };

        let mut errors = Vec::new();
        for (session_id, runtime) in runtimes {
            if let Err(err) = terminate_runtime(&session_id, runtime) {
                errors.push(format!("{session_id}: {err}"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub fn ensure_tmux_child_ready(&self, session_id: &str) -> Result<(), String> {
        let should_wait = {
            let runtimes = self
                .runtimes
                .lock()
                .map_err(|_| "Session lock poisoned".to_string())?;
            let runtime = runtimes
                .get(session_id)
                .ok_or_else(|| "Session not found".to_string())?;
            runtime.shell_kind.requires_tmux_shell_ready_guard() && !runtime.shell_ready
        };

        if !should_wait {
            return Ok(());
        }

        {
            let mut runtimes = self
                .runtimes
                .lock()
                .map_err(|_| "Session lock poisoned".to_string())?;
            let runtime = runtimes
                .get_mut(session_id)
                .ok_or_else(|| "Session not found".to_string())?;
            if runtime.started_at.elapsed()
                >= TMUX_SHELL_READY_PHASE1_TIMEOUT + TMUX_SHELL_READY_PHASE2_TIMEOUT
            {
                runtime.shell_ready = true;
                return Ok(());
            }
        }

        wait_for_tmux_initial_prompt(&self.runtimes, session_id, TMUX_SHELL_READY_PHASE1_TIMEOUT)?;

        let primer_baseline = {
            let mut runtimes = self
                .runtimes
                .lock()
                .map_err(|_| "Session lock poisoned".to_string())?;
            let runtime = runtimes
                .get_mut(session_id)
                .ok_or_else(|| "Session not found".to_string())?;

            if runtime.shell_ready || !runtime.shell_kind.requires_tmux_shell_ready_guard() {
                return Ok(());
            }

            runtime
                .writer
                .write_all(b"\r")
                .map_err(|err| err.to_string())?;
            runtime.writer.flush().map_err(|err| err.to_string())?;
            runtime.last_output_at
        };

        let ready = wait_for_tmux_prompt_after_primer(
            &self.runtimes,
            session_id,
            primer_baseline,
            TMUX_SHELL_READY_PHASE2_TIMEOUT,
        )?;

        if ready {
            thread::sleep(TMUX_SHELL_READY_SETTLE);
            return Ok(());
        }

        let mut runtimes = self
            .runtimes
            .lock()
            .map_err(|_| "Session lock poisoned".to_string())?;
        if let Some(runtime) = runtimes.get_mut(session_id) {
            runtime.shell_ready = true;
        }
        Ok(())
    }

    pub fn capture_output(
        &self,
        session_id: &str,
        options: SessionCaptureOptions,
    ) -> Result<String, String> {
        let runtimes = self
            .runtimes
            .lock()
            .map_err(|_| "Session lock poisoned".to_string())?;
        let runtime = runtimes
            .get(session_id)
            .ok_or_else(|| "Session not found".to_string())?;

        let mut output = runtime.output_tail.clone();
        if !options.include_escape {
            output = strip_ansi(&output);
        }

        let mut lines = output.lines().map(str::to_string).collect::<Vec<_>>();
        let start_index = resolve_capture_line_index(lines.len(), options.start_line, 0);
        let end_index = resolve_capture_line_index(
            lines.len(),
            options.end_line,
            lines.len().saturating_sub(1) as i32,
        );
        if !lines.is_empty() && start_index <= end_index {
            lines = lines[start_index..=end_index].to_vec();
        } else if options.start_line.is_some() || options.end_line.is_some() {
            lines.clear();
        }

        let separator = if options.join_lines { "" } else { "\n" };
        Ok(lines.join(separator))
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

fn build_command(program: &str, args: Option<&[String]>) -> CommandBuilder {
    let resolved_program = resolve_program(program);
    if cfg!(windows) && should_wrap_with_cmd(&resolved_program) {
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.arg("/C");
        cmd.arg(&resolved_program);
        if let Some(args) = args {
            cmd.args(args);
        }
        return cmd;
    }

    let mut cmd = CommandBuilder::new(&resolved_program);
    if let Some(args) = args {
        cmd.args(args);
    }
    cmd
}

fn should_wrap_with_cmd(program: &str) -> bool {
    let lower = program.to_ascii_lowercase();
    if ["powershell.exe", "pwsh.exe", "cmd.exe", "bash.exe"].contains(&lower.as_str()) {
        return false;
    }

    let path = Path::new(program);
    !path.is_absolute() && path.extension().is_none()
}

fn apply_runtime_env(cmd: &mut CommandBuilder, extra_env: Option<&HashMap<String, String>>) {
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }

    if let Some(extra_env) = extra_env {
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
    }
}

fn wait_for_tmux_initial_prompt(
    runtimes: &Arc<Mutex<HashMap<String, SessionRuntime>>>,
    session_id: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        {
            let mut runtimes = runtimes
                .lock()
                .map_err(|_| "Session lock poisoned".to_string())?;
            let runtime = runtimes
                .get_mut(session_id)
                .ok_or_else(|| "Session not found".to_string())?;
            if runtime.shell_ready || !runtime.shell_kind.requires_tmux_shell_ready_guard() {
                return Ok(());
            }
            if looks_like_git_bash_prompt(&runtime.output_tail) {
                return Ok(());
            }
        }

        if Instant::now() >= deadline {
            return Ok(());
        }

        thread::sleep(TMUX_SHELL_READY_POLL);
    }
}

fn wait_for_tmux_prompt_after_primer(
    runtimes: &Arc<Mutex<HashMap<String, SessionRuntime>>>,
    session_id: &str,
    primer_baseline: Instant,
    timeout: Duration,
) -> Result<bool, String> {
    let deadline = Instant::now() + timeout;
    loop {
        {
            let mut runtimes = runtimes
                .lock()
                .map_err(|_| "Session lock poisoned".to_string())?;
            let runtime = runtimes
                .get_mut(session_id)
                .ok_or_else(|| "Session not found".to_string())?;

            if runtime.shell_ready || !runtime.shell_kind.requires_tmux_shell_ready_guard() {
                return Ok(true);
            }

            if runtime.last_output_at > primer_baseline
                && looks_like_git_bash_prompt(&runtime.output_tail)
            {
                runtime.shell_ready = true;
                return Ok(true);
            }
        }

        if Instant::now() >= deadline {
            return Ok(false);
        }

        thread::sleep(TMUX_SHELL_READY_POLL);
    }
}

fn push_output_tail(output_tail: &mut String, chunk: &str) {
    output_tail.push_str(chunk);
    let char_count = output_tail.chars().count();
    if char_count > OUTPUT_TAIL_CHAR_LIMIT {
        *output_tail = output_tail
            .chars()
            .skip(char_count - OUTPUT_TAIL_CHAR_LIMIT)
            .collect();
    }
}

fn resolve_capture_line_index(total_lines: usize, raw_index: Option<i32>, default_index: i32) -> usize {
    if total_lines == 0 {
        return 0;
    }

    let index = raw_index.unwrap_or(default_index);
    let normalized = if index < 0 {
        total_lines as i32 + index
    } else {
        index
    };
    normalized.clamp(0, total_lines.saturating_sub(1) as i32) as usize
}

fn looks_like_git_bash_prompt(output_tail: &str) -> bool {
    let clean = strip_ansi(output_tail);
    let line = clean
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim_end();
    line.ends_with('$') || line.ends_with('#')
}

fn strip_ansi(input: &str) -> String {
    let mut clean = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{001b}' {
            if matches!(chars.peek(), Some('[')) {
                let _ = chars.next();
                while let Some(next) = chars.next() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
                continue;
            }
            continue;
        }
        clean.push(ch);
    }

    clean
}

fn terminate_runtime(session_id: &str, mut runtime: SessionRuntime) -> Result<(), String> {
    if let Some(mut pipe) = runtime.pipe.take() {
        let _ = pipe.stdin.take();
        let _ = pipe.child.kill();
        let _ = pipe.child.wait();
    }

    let kill_error = runtime.killer.kill().err().map(|err| err.to_string());

    #[cfg(windows)]
    {
        if let Some(process_id) = runtime.process_id {
            return terminate_windows_process_tree(session_id, process_id, kill_error);
        }
    }

    if let Some(err) = kill_error {
        return Err(format!("Failed to terminate session {session_id}: {err}"));
    }

    Ok(())
}

#[cfg(windows)]
fn terminate_windows_process_tree(
    session_id: &str,
    process_id: u32,
    kill_error: Option<String>,
) -> Result<(), String> {
    let mut command = Command::new("taskkill");
    command
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/PID", &process_id.to_string(), "/T", "/F"]);
    let output = command.output().map_err(|err| {
        format!("Failed to terminate session {session_id}: taskkill could not start ({err})")
    })?;

    if output.status.success() || !windows_process_exists(process_id) {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let mut details = Vec::new();
    if let Some(err) = kill_error {
        details.push(format!("portable-pty kill error: {err}"));
    }
    if !stdout.is_empty() {
        details.push(format!("taskkill stdout: {stdout}"));
    }
    if !stderr.is_empty() {
        details.push(format!("taskkill stderr: {stderr}"));
    }

    Err(format!(
        "Failed to terminate session {session_id}: {}",
        details.join(" | ")
    ))
}

#[cfg(windows)]
fn windows_process_exists(process_id: u32) -> bool {
    let mut command = Command::new("powershell.exe");
    command
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "if (Get-Process -Id {process_id} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"
            ),
        ]);

    command
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}
