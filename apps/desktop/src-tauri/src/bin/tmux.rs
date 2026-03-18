use std::env;
use std::path::PathBuf;
use std::process::{Command, exit};

use serde_json::json;

const NOOP_COMMANDS: &[&str] = &[
    "select-pane",
    "selectp",
    "resize-pane",
    "resizep",
    "set-option",
    "set",
    "set-window-option",
    "setw",
    "bind-key",
    "bind",
    "unbind-key",
    "unbind",
    "set-environment",
    "setenv",
    "source-file",
    "source",
];

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    };
    exit(code);
}

fn run() -> Result<i32, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        println!("tmux workspace-terminal-shim 1.0");
        return Ok(0);
    }

    if args[0] == "-V" || args[0] == "--version" {
        println!("tmux workspace-terminal-shim 1.0");
        return Ok(0);
    }

    let url = match env::var("WORKSPACE_TERMINAL_TMUX_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return fallback_real_tmux(&args),
    };
    let token = match env::var("WORKSPACE_TERMINAL_TMUX_TOKEN") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return fallback_real_tmux(&args),
    };

    let subcmd = args[0].as_str();
    if NOOP_COMMANDS.contains(&subcmd) {
        return Ok(0);
    }

    match subcmd {
        "split-window" | "splitw" => handle_split_window(&url, &token, &args[1..]),
        "new-window" | "neww" => handle_new_window(&url, &token, &args[1..]),
        "send-keys" | "send" => handle_send_keys(&url, &token, &args[1..]),
        "list-panes" | "lsp" => handle_list_panes(&url, &token, &args[1..]),
        "kill-pane" | "killp" | "kill-window" | "killw" => {
            handle_kill_pane(&url, &token, &args[1..])
        }
        "display-message" | "display" => handle_display_message(&url, &token, &args[1..]),
        "has-session" | "has" => handle_has_session(&url, &token),
        unsupported => Err(format!(
            "workspace-terminal-tmux: unsupported command '{unsupported}'"
        )),
    }
}

fn fallback_real_tmux(args: &[String]) -> Result<i32, String> {
    let self_path = env::current_exe().map_err(|err| err.to_string())?;
    let self_dir = self_path
        .parent()
        .ok_or_else(|| "workspace-terminal-tmux: failed to resolve shim dir".to_string())?;

    if let Some(real_tmux) = find_real_tmux(self_dir) {
        let status = Command::new(real_tmux)
            .args(args)
            .status()
            .map_err(|err| err.to_string())?;
        return Ok(status.code().unwrap_or(1));
    }

    Err("workspace-terminal-tmux: real tmux not found".to_string())
}

fn find_real_tmux(shim_dir: &std::path::Path) -> Option<PathBuf> {
    let path_var = env::var("PATH").ok()?;
    for entry in env::split_paths(&path_var) {
        if entry == shim_dir {
            continue;
        }
        let candidate_exe = entry.join("tmux.exe");
        if candidate_exe.is_file() {
            return Some(candidate_exe);
        }
        let candidate = entry.join("tmux");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn handle_split_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut direction: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut name: Option<String> = None;
    let mut target: Option<String> = None;
    let mut print_info = false;
    let mut command_parts: Vec<String> = Vec::new();

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-h" => {
                direction = Some("horizontal".to_string());
                index += 1;
            }
            "-v" => {
                direction = Some("vertical".to_string());
                index += 1;
            }
            "-c" => {
                if index + 1 < args.len() {
                    cwd = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-n" => {
                if index + 1 < args.len() {
                    name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-P" => {
                print_info = true;
                index += 1;
            }
            "-F" | "-l" | "-p" | "-e" => {
                index += 2;
            }
            "-d" => {
                index += 1;
            }
            "--" => {
                command_parts.extend(args[index + 1..].iter().cloned());
                break;
            }
            _ if arg.starts_with('-') => {
                index += 1;
            }
            _ => {
                command_parts.extend(args[index..].iter().cloned());
                break;
            }
        }
    }

    let command = if command_parts.is_empty() {
        None
    } else {
        Some(command_parts.join(" "))
    };

    let value = json!({
      "direction": direction,
      "cwd": cwd,
      "name": name,
      "target": target,
      "command": command
    });
    let response = post_json(url, token, "/v1/tmux/split-window", &value)?;

    if print_info {
        if let Some(pane_id) = response.get("paneId").and_then(|value| value.as_str()) {
            if pane_id.starts_with('%') {
                println!("{pane_id}");
            } else {
                println!("%{pane_id}");
            }
        }
    }

    Ok(0)
}

fn handle_new_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut cwd: Option<String> = None;
    let mut name: Option<String> = None;
    let mut command_parts: Vec<String> = Vec::new();

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-c" => {
                if index + 1 < args.len() {
                    cwd = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-n" => {
                if index + 1 < args.len() {
                    name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-t" | "-F" => {
                index += 2;
            }
            "-d" => {
                index += 1;
            }
            "--" => {
                command_parts.extend(args[index + 1..].iter().cloned());
                break;
            }
            _ if arg.starts_with('-') => {
                index += 1;
            }
            _ => {
                command_parts.extend(args[index..].iter().cloned());
                break;
            }
        }
    }

    let command = if command_parts.is_empty() {
        None
    } else {
        Some(command_parts.join(" "))
    };

    let value = json!({
      "cwd": cwd,
      "name": name,
      "command": command
    });
    let _ = post_json(url, token, "/v1/tmux/new-window", &value)?;
    Ok(0)
}

fn handle_send_keys(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut key_parts: Vec<String> = Vec::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-l" => {
                index += 1;
            }
            _ => {
                key_parts.push(args[index].clone());
                index += 1;
            }
        }
    }

    let text = map_tmux_keys_to_text(&key_parts);
    let value = json!({
      "target": target,
      "text": text
    });
    let _ = post_json(url, token, "/v1/tmux/send-keys", &value)?;
    Ok(0)
}

fn handle_list_panes(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut format = "#{pane_id}".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-F" => {
                if index + 1 < args.len() {
                    format = args[index + 1].clone();
                }
                index += 2;
            }
            "-a" => {
                index += 1;
            }
            "-t" => {
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }

    let encoded = urlencoding::encode(&format);
    let body = get_text(url, token, &format!("/v1/tmux/list-panes?format={encoded}"))?;
    if !body.is_empty() {
        println!("{body}");
    }
    Ok(0)
}

fn handle_kill_pane(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({ "target": target });
    let _ = post_json(url, token, "/v1/tmux/kill-pane", &value)?;
    Ok(0)
}

fn handle_display_message(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut print_mode = false;
    let mut format: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-p" => {
                print_mode = true;
                index += 1;
            }
            "-F" => {
                if index + 1 < args.len() {
                    format = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-t" => {
                index += 2;
            }
            arg if !arg.starts_with('-') && format.is_none() => {
                format = Some(arg.to_string());
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    if print_mode {
        let format = format.unwrap_or_else(|| "#{pane_id}".to_string());
        let encoded = urlencoding::encode(&format);
        let body = get_text(
            url,
            token,
            &format!("/v1/tmux/display-message?format={encoded}"),
        )?;
        println!("{body}");
    }

    Ok(0)
}

fn handle_has_session(url: &str, token: &str) -> Result<i32, String> {
    let _ = get_text(url, token, "/v1/tmux/has-session")?;
    Ok(0)
}

fn map_tmux_keys_to_text(keys: &[String]) -> String {
    let mut output = String::new();
    for key in keys {
        match key.as_str() {
            "Enter" => output.push('\r'),
            "Tab" => output.push('\t'),
            "Space" => output.push(' '),
            "BSpace" => output.push('\u{007f}'),
            "Escape" => output.push('\u{001b}'),
            "C-c" => output.push('\u{0003}'),
            "C-d" => output.push('\u{0004}'),
            "C-l" => output.push('\u{000c}'),
            "C-z" => output.push('\u{001a}'),
            other => output.push_str(other),
        }
    }
    output
}

fn post_json(
    base_url: &str,
    token: &str,
    path: &str,
    value: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let response = ureq::post(&format!("{base_url}{path}"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .send_json(value.clone())
        .map_err(|err| err.to_string())?;

    response
        .into_json::<serde_json::Value>()
        .map_err(|err| err.to_string())
}

fn get_text(base_url: &str, token: &str, path: &str) -> Result<String, String> {
    ureq::get(&format!("{base_url}{path}"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|err| err.to_string())?
        .into_string()
        .map_err(|err| err.to_string())
}
