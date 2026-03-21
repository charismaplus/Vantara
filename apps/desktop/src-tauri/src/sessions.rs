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
    models::{
        LaunchProfile, SessionExitEvent, SessionOutputEvent, SessionSidebarStatus,
        SessionStatus, SessionStatusProvider, TerminalSession,
    },
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
    sidebar_statuses: Arc<Mutex<HashMap<String, SessionSidebarStatus>>>,
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

        let initial_sidebar_status = build_initial_sidebar_status(&session);

        {
            let db_guard = db
                .lock()
                .map_err(|_| "Database lock poisoned".to_string())?;
            db_guard.upsert_session(&session)?;
        }

        self.sidebar_statuses
            .lock()
            .map_err(|_| "Session status lock poisoned".to_string())?
            .insert(session.id.clone(), initial_sidebar_status.clone());

        let session_id = session.id.clone();
        let exit_session_id = session.id.clone();
        let runtimes = self.runtimes.clone();
        let reader_runtimes = self.runtimes.clone();
        let sidebar_statuses = self.sidebar_statuses.clone();
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
        let reader_statuses = sidebar_statuses.clone();
        thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let chunk = String::from_utf8_lossy(&buffer[..read]).to_string();
                        let mut output_tail = None;
                        if let Ok(mut runtimes) = reader_runtimes.lock() {
                            if let Some(runtime) = runtimes.get_mut(&reader_session_id) {
                                runtime.last_output_at = Instant::now();
                                push_output_tail(&mut runtime.output_tail, &chunk);
                                output_tail = Some(runtime.output_tail.clone());
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
                        if let Some(output_tail) = output_tail {
                            if let Some(status) =
                                update_status_from_output(&reader_statuses, &reader_session_id, &output_tail)
                            {
                                let _ = reader_app.emit("session-status-changed", status);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let status_app = app.clone();
        let wait_app = app;
        let wait_db = db;
        let wait_statuses = sidebar_statuses;
        thread::spawn(move || {
            let exit_code = wait_for_exit(child);
            let status = if exit_code == Some(0) {
                SessionStatus::Exited
            } else {
                SessionStatus::Failed
            };

            if let Ok(db_guard) = wait_db.lock() {
                let _ = db_guard.update_session_exit(&exit_session_id, status.clone(), exit_code);
            }
            let _ = runtimes.lock().map(|mut map| map.remove(&exit_session_id));
            if let Some(next_status) =
                update_status_state(&wait_statuses, &exit_session_id, status.clone())
            {
                let _ = wait_app.emit("session-status-changed", next_status);
            }

            if let Some(state) = wait_app.try_state::<crate::AppState>() {
                let _ = crate::handle_runtime_session_exit(state.inner(), &exit_session_id);
            }

            let event = SessionExitEvent {
                session_id: exit_session_id.clone(),
                exit_code,
            };
            let _ = wait_app.emit("session-exit", event);
            if let Ok(mut statuses) = wait_statuses.lock() {
                statuses.remove(&exit_session_id);
            }
        });

        let _ = status_app.emit("session-status-changed", initial_sidebar_status);

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
        self.sidebar_statuses
            .lock()
            .map_err(|_| "Session status lock poisoned".to_string())?
            .remove(session_id);
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
        {
            let mut statuses = self
                .sidebar_statuses
                .lock()
                .map_err(|_| "Session status lock poisoned".to_string())?;
            for (session_id, _) in &runtimes {
                statuses.remove(session_id);
            }
        }

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

    pub fn get_sidebar_status(
        &self,
        session: &TerminalSession,
    ) -> Result<SessionSidebarStatus, String> {
        let cached = self
            .sidebar_statuses
            .lock()
            .map_err(|_| "Session status lock poisoned".to_string())?
            .get(&session.id)
            .cloned();
        let mut status = cached.unwrap_or_else(|| build_initial_sidebar_status(session));
        status.state = session.status.clone();
        Ok(status)
    }
}

fn build_initial_sidebar_status(session: &TerminalSession) -> SessionSidebarStatus {
    SessionSidebarStatus {
        session_id: session.id.clone(),
        launch_profile: session.launch_profile.clone(),
        provider: provider_for_launch_profile(&session.launch_profile),
        state: session.status.clone(),
        model_label: None,
        mode_label: mode_label_for_launch_profile(&session.launch_profile),
        context_percent: None,
        usage5h_percent: None,
        usage5h_reset_at: None,
        usage7d_percent: None,
        usage7d_reset_at: None,
    }
}

fn provider_for_launch_profile(launch_profile: &LaunchProfile) -> SessionStatusProvider {
    match launch_profile {
        LaunchProfile::Terminal => SessionStatusProvider::Terminal,
        LaunchProfile::Claude | LaunchProfile::ClaudeUnsafe => SessionStatusProvider::Claude,
        LaunchProfile::Codex | LaunchProfile::CodexFullAuto => SessionStatusProvider::Codex,
    }
}

fn mode_label_for_launch_profile(launch_profile: &LaunchProfile) -> Option<String> {
    match launch_profile {
        LaunchProfile::Codex => Some("Interactive".to_string()),
        LaunchProfile::CodexFullAuto => Some("Full Auto".to_string()),
        LaunchProfile::Terminal | LaunchProfile::Claude | LaunchProfile::ClaudeUnsafe => None,
    }
}

fn update_status_state(
    statuses: &Arc<Mutex<HashMap<String, SessionSidebarStatus>>>,
    session_id: &str,
    state: SessionStatus,
) -> Option<SessionSidebarStatus> {
    let mut statuses = statuses.lock().ok()?;
    let status = statuses.get_mut(session_id)?;
    if status.state == state {
        return None;
    }
    status.state = state;
    Some(status.clone())
}

fn update_status_from_output(
    statuses: &Arc<Mutex<HashMap<String, SessionSidebarStatus>>>,
    session_id: &str,
    output_tail: &str,
) -> Option<SessionSidebarStatus> {
    let mut statuses = statuses.lock().ok()?;
    let status = statuses.get_mut(session_id)?;
    let mut next_status = status.clone();
    if !apply_output_to_sidebar_status(&mut next_status, output_tail) || *status == next_status {
        return None;
    }
    *status = next_status.clone();
    Some(next_status)
}

fn apply_output_to_sidebar_status(status: &mut SessionSidebarStatus, output_tail: &str) -> bool {
    let clean = strip_ansi(output_tail);
    match status.provider {
        SessionStatusProvider::Terminal => false,
        SessionStatusProvider::Codex => apply_codex_status(status, &clean),
        SessionStatusProvider::Claude => apply_claude_status(status, &clean),
    }
}

fn apply_codex_status(status: &mut SessionSidebarStatus, clean_output: &str) -> bool {
    let mut changed = false;
    if let Some(model_label) = extract_field_after_label(clean_output, "model:") {
        if status.model_label.as_deref() != Some(model_label.as_str()) {
            status.model_label = Some(model_label);
            changed = true;
        }
    }

    if let Some(context_percent) = extract_percent_before_suffix(clean_output, "% left") {
        if status.context_percent != Some(context_percent) {
            status.context_percent = Some(context_percent);
            changed = true;
        }
    }

    changed
}

fn apply_claude_status(status: &mut SessionSidebarStatus, clean_output: &str) -> bool {
    let mut changed = false;

    if let Some(model_label) = extract_last_model_token(clean_output, "claude-") {
        if status.model_label.as_deref() != Some(model_label.as_str()) {
            status.model_label = Some(model_label);
            changed = true;
        }
    }

    if let Some(context_percent) = extract_context_percent(clean_output) {
        if status.context_percent != Some(context_percent) {
            status.context_percent = Some(context_percent);
            changed = true;
        }
    }

    if let Some(usage5h_percent) = extract_percent_after_label(clean_output, "5h") {
        if status.usage5h_percent != Some(usage5h_percent) {
            status.usage5h_percent = Some(usage5h_percent);
            changed = true;
        }
    }

    if let Some(usage7d_percent) = extract_percent_after_label(clean_output, "7d") {
        if status.usage7d_percent != Some(usage7d_percent) {
            status.usage7d_percent = Some(usage7d_percent);
            changed = true;
        }
    }

    changed
}

fn extract_field_after_label(clean_output: &str, label: &str) -> Option<String> {
    clean_output
        .lines()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            let lower = trimmed.to_ascii_lowercase();
            let label_lower = label.to_ascii_lowercase();
            lower
                .starts_with(&label_lower)
                .then(|| trimmed[label.len()..].trim().to_string())
        })
        .filter(|value| !value.is_empty())
}

fn extract_percent_before_suffix(clean_output: &str, suffix: &str) -> Option<u8> {
    clean_output
        .match_indices(suffix)
        .filter_map(|(index, _)| extract_trailing_percent(&clean_output[..index]))
        .last()
}

fn extract_percent_after_label(clean_output: &str, label: &str) -> Option<u8> {
    let lower_label = label.to_ascii_lowercase();
    clean_output.lines().rev().find_map(|line| {
        let lower_line = line.to_ascii_lowercase();
        lower_line
            .find(&lower_label)
            .and_then(|start| extract_first_percent(&line[start + label.len()..]))
    })
}

fn extract_context_percent(clean_output: &str) -> Option<u8> {
    clean_output.lines().rev().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if lower.contains("context") {
            extract_first_percent(line)
        } else {
            None
        }
    })
}

fn extract_last_model_token(clean_output: &str, prefix: &str) -> Option<String> {
    clean_output
        .split(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | '(' | ')' | '[' | ']'))
        .filter(|token| token.starts_with(prefix))
        .map(|token| token.trim_end_matches(|ch: char| ".:;!?".contains(ch)).to_string())
        .last()
}

fn extract_first_percent(input: &str) -> Option<u8> {
    let bytes = input.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] == b'%' {
            if let Some(percent) = extract_trailing_percent(&input[..index]) {
                return Some(percent);
            }
        }
    }
    None
}

fn extract_trailing_percent(input: &str) -> Option<u8> {
    let digits = input
        .chars()
        .rev()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u8>().ok().filter(|value| *value <= 100)
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
