use std::env;
use std::path::PathBuf;
use std::process::{Command, exit};

use serde_json::json;

const NOOP_COMMANDS: &[&str] = &[];

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

    dispatch_tmux_command(&url, &token, subcmd, &args[1..])
}

fn dispatch_tmux_command(
    url: &str,
    token: &str,
    subcmd: &str,
    args: &[String],
) -> Result<i32, String> {
    match subcmd {
        "new-session" | "new" => handle_new_session(url, token, args),
        "attach-session" | "attach" => handle_attach_session(url, token, args),
        "switch-client" | "switchc" => handle_switch_client(url, token, args),
        "split-window" | "splitw" => handle_split_window(url, token, args),
        "new-window" | "neww" => handle_new_window(url, token, args),
        "kill-session" | "kill" => handle_kill_session(url, token, args),
        "rename-session" | "rename" => handle_rename_session(url, token, args),
        "rename-window" | "renamew" => handle_rename_window(url, token, args),
        "bind-key" | "bind" => handle_bind_key(url, token, args, false),
        "unbind-key" | "unbind" => handle_bind_key(url, token, args, true),
        "list-keys" | "lsk" => handle_list_keys(url, token, args),
        "send-keys" | "send" => handle_send_keys(url, token, args),
        "list-panes" | "lsp" => handle_list_panes(url, token, args),
        "list-windows" | "lsw" => handle_list_windows(url, token, args),
        "list-sessions" | "ls" => handle_list_sessions(url, token, args),
        "list-clients" | "lsc" => handle_list_clients(url, token, args),
        "set-option" | "set" => handle_set_option(url, token, args, false),
        "show-options" | "show" => handle_show_options(url, token, args, false),
        "set-window-option" | "setw" => handle_set_option(url, token, args, true),
        "show-window-options" | "showw" => handle_show_options(url, token, args, true),
        "set-environment" | "setenv" => handle_set_environment(url, token, args),
        "show-environment" | "showenv" => handle_show_environment(url, token, args),
        "set-hook" => handle_set_hook(url, token, args),
        "show-hooks" => handle_show_hooks(url, token, args),
        "set-buffer" | "setb" => handle_set_buffer(url, token, args),
        "show-buffer" | "showb" => handle_show_buffer(url, token, args),
        "list-buffers" | "lsb" => handle_list_buffers(url, token, args),
        "delete-buffer" | "deleteb" => handle_delete_buffer(url, token, args),
        "load-buffer" | "loadb" => handle_load_buffer(url, token, args),
        "save-buffer" | "saveb" => handle_save_buffer(url, token, args),
        "paste-buffer" | "pasteb" => handle_paste_buffer(url, token, args),
        "wait-for" | "wait" => handle_wait_for(url, token, args),
        "respawn-pane" | "respawnp" => handle_respawn_pane(url, token, args),
        "respawn-window" | "respawnw" => handle_respawn_window(url, token, args),
        "break-pane" | "breakp" => handle_break_pane(url, token, args),
        "join-pane" | "joinp" => handle_join_pane(url, token, args, false),
        "move-pane" | "movep" => handle_join_pane(url, token, args, true),
        "swap-pane" | "swapp" => handle_swap_pane(url, token, args),
        "swap-window" | "swapw" => handle_swap_window(url, token, args),
        "rotate-window" | "rotatew" => handle_rotate_window(url, token, args),
        "move-window" | "movew" => handle_move_window(url, token, args),
        "pipe-pane" | "pipep" => handle_pipe_pane(url, token, args),
        "capture-pane" | "capturep" => handle_capture_pane(url, token, args),
        "kill-pane" | "killp" => handle_kill_pane(url, token, args),
        "kill-window" | "killw" => handle_kill_window(url, token, args),
        "select-pane" | "selectp" => handle_select_pane(url, token, args),
        "last-pane" | "lastp" => handle_last_pane(&url, &token),
        "select-window" | "selectw" => handle_select_window(url, token, args),
        "last-window" | "last" => handle_cycle_window(url, token, "last", args),
        "next-window" | "next" => handle_cycle_window(url, token, "next", args),
        "previous-window" | "prev" => {
            handle_cycle_window(url, token, "previous", args)
        }
        "resize-pane" | "resizep" => handle_resize_pane(url, token, args),
        "display-message" | "display" => handle_display_message(url, token, args),
        "refresh-client" | "refresh" => handle_refresh_client(url, token, args),
        "has-session" | "has" => handle_has_session(url, token, args),
        "source-file" | "source" => handle_source_file(url, token, args),
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
    let mut format: Option<String> = None;
    let mut detached = false;
    let mut before = false;
    let mut full_span = false;
    let mut size: Option<u16> = None;
    let mut size_is_percentage = false;
    let mut env_overrides = serde_json::Map::new();
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
            "-F" => {
                if index + 1 < args.len() {
                    format = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-l" => {
                if index + 1 < args.len() {
                    let (parsed_size, percentage) = parse_tmux_size(&args[index + 1]);
                    size = parsed_size;
                    size_is_percentage = percentage;
                }
                index += 2;
            }
            "-p" => {
                if index + 1 < args.len() {
                    size = args[index + 1]
                        .parse::<u16>()
                        .ok()
                        .map(|value| value.clamp(1, 99));
                    size_is_percentage = true;
                }
                index += 2;
            }
            "-e" => {
                if index + 1 < args.len() {
                    if let Some((key, value)) = args[index + 1].split_once('=') {
                        env_overrides.insert(
                            key.to_string(),
                            serde_json::Value::String(value.to_string()),
                        );
                    }
                }
                index += 2;
            }
            "-d" => {
                detached = true;
                index += 1;
            }
            "-b" => {
                before = true;
                index += 1;
            }
            "-f" => {
                full_span = true;
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
      "command": command,
      "format": format,
      "detached": detached,
      "before": before,
      "fullSpan": full_span,
      "size": size,
      "sizeIsPercentage": size_is_percentage,
      "env": if env_overrides.is_empty() { serde_json::Value::Null } else { serde_json::Value::Object(env_overrides) }
    });
    let response = post_json(url, token, "/v1/tmux/split-window", &value)?;

    if print_info {
        let rendered = match format {
            Some(format) => render_tmux_response_format(&format, &response),
            None => response
                .get("paneId")
                .and_then(|value| value.as_str())
                .map(|pane_id| {
                    if pane_id.starts_with('%') {
                        pane_id.to_string()
                    } else {
                        format!("%{pane_id}")
                    }
                })
                .unwrap_or_default(),
        };
        if !rendered.is_empty() {
            println!("{rendered}");
        }
    }

    Ok(0)
}

fn handle_new_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut cwd: Option<String> = None;
    let mut name: Option<String> = None;
    let mut target: Option<String> = None;
    let mut detached = false;
    let mut print_info = false;
    let mut format: Option<String> = None;
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
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-F" => {
                if index + 1 < args.len() {
                    format = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-d" => {
                detached = true;
                index += 1;
            }
            "-P" => {
                print_info = true;
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
      "command": command,
      "target": target,
      "detached": detached
    });
    let response = post_json(url, token, "/v1/tmux/new-window", &value)?;
    if print_info {
        let rendered = match format {
            Some(format) => render_tmux_response_format(&format, &response),
            None => response
                .get("windowId")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .unwrap_or_default(),
        };
        if !rendered.is_empty() {
            println!("{rendered}");
        }
    }
    Ok(0)
}

fn handle_new_session(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut name: Option<String> = None;
    let mut window_name: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut target: Option<String> = None;
    let mut detached = false;
    let mut print_info = false;
    let mut format: Option<String> = None;
    let mut command_parts: Vec<String> = Vec::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-s" => {
                if index + 1 < args.len() {
                    name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-n" => {
                if index + 1 < args.len() {
                    window_name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-c" => {
                if index + 1 < args.len() {
                    cwd = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-d" => {
                detached = true;
                index += 1;
            }
            "-P" => {
                print_info = true;
                index += 1;
            }
            "-F" => {
                if index + 1 < args.len() {
                    format = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "--" => {
                command_parts.extend(args[index + 1..].iter().cloned());
                break;
            }
            arg if !arg.starts_with('-') => {
                command_parts.extend(args[index..].iter().cloned());
                break;
            }
            _ => {
                index += 1;
            }
        }
    }

    let command = if command_parts.is_empty() {
        None
    } else {
        Some(command_parts.join(" "))
    };
    let value = json!({
      "name": name,
      "windowName": window_name,
      "command": command,
      "cwd": cwd,
      "target": target,
      "detached": detached
    });
    let response = post_json(url, token, "/v1/tmux/new-session", &value)?;
    if print_info {
        let rendered = match format {
            Some(format) => render_tmux_response_format(&format, &response),
            None => response
                .get("sessionId")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .unwrap_or_default(),
        };
        if !rendered.is_empty() {
            println!("{rendered}");
        }
    }
    Ok(0)
}

fn handle_attach_session(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
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
    let _ = post_json(url, token, "/v1/tmux/attach-session", &value)?;
    Ok(0)
}

fn handle_switch_client(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
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
    let _ = post_json(url, token, "/v1/tmux/switch-client", &value)?;
    Ok(0)
}

fn handle_kill_session(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
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
    let _ = post_json(url, token, "/v1/tmux/kill-session", &value)?;
    Ok(0)
}

fn handle_rename_session(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut name: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            arg if !arg.starts_with('-') => {
                name = Some(args[index..].join(" "));
                break;
            }
            _ => {
                index += 1;
            }
        }
    }

    let Some(name) = name else {
        return Err("workspace-terminal-tmux: rename-session requires a name".to_string());
    };
    let value = json!({ "target": target, "name": name });
    let _ = post_json(url, token, "/v1/tmux/rename-session", &value)?;
    Ok(0)
}

fn handle_rename_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut name: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            arg if !arg.starts_with('-') => {
                name = Some(args[index..].join(" "));
                break;
            }
            _ => {
                index += 1;
            }
        }
    }

    let Some(name) = name else {
        return Err("workspace-terminal-tmux: rename-window requires a name".to_string());
    };
    let value = json!({ "target": target, "name": name });
    let _ = post_json(url, token, "/v1/tmux/rename-window", &value)?;
    Ok(0)
}

fn handle_bind_key(
    url: &str,
    token: &str,
    args: &[String],
    is_unbind: bool,
) -> Result<i32, String> {
    let mut table: Option<String> = None;
    let mut key: Option<String> = None;
    let mut command: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-T" => {
                if index + 1 < args.len() {
                    table = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-n" => {
                table = Some("root".to_string());
                index += 1;
            }
            arg if !arg.starts_with('-') => {
                if key.is_none() {
                    key = Some(arg.to_string());
                    index += 1;
                } else {
                    command = Some(args[index..].join(" "));
                    break;
                }
            }
            _ => {
                index += 1;
            }
        }
    }

    let Some(key) = key else {
        return Err(if is_unbind {
            "workspace-terminal-tmux: unbind-key requires a key".to_string()
        } else {
            "workspace-terminal-tmux: bind-key requires a key".to_string()
        });
    };

    let path = if is_unbind {
        "/v1/tmux/unbind-key"
    } else {
        "/v1/tmux/bind-key"
    };
    let value = json!({
      "table": table,
      "key": key,
      "command": command
    });
    let _ = post_json(url, token, path, &value)?;
    Ok(0)
}

fn handle_list_keys(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut table: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-T" => {
                if index + 1 < args.len() {
                    table = Some(args[index + 1].clone());
                }
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }
    let path = match table {
        Some(table) => format!("/v1/tmux/list-keys?table={}", urlencoding::encode(&table)),
        None => "/v1/tmux/list-keys".to_string(),
    };
    let body = get_text(url, token, &path)?;
    if !body.is_empty() {
        println!("{body}");
    }
    Ok(0)
}

fn handle_list_clients(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut format = "#{client_tty}".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-F" => {
                if index + 1 < args.len() {
                    format = args[index + 1].clone();
                }
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }
    let encoded = urlencoding::encode(&format);
    let body = get_text(url, token, &format!("/v1/tmux/list-clients?format={encoded}"))?;
    if !body.is_empty() {
        println!("{body}");
    }
    Ok(0)
}

fn handle_set_environment(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut unset = false;
    let mut name: Option<String> = None;
    let mut value: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-u" => {
                unset = true;
                index += 1;
            }
            "-h" => {
                index += 1;
            }
            arg if !arg.starts_with('-') => {
                if name.is_none() {
                    name = Some(arg.to_string());
                } else if value.is_none() {
                    value = Some(arg.to_string());
                }
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let Some(name) = name else {
        return Err("workspace-terminal-tmux: set-environment requires a name".to_string());
    };
    let value = json!({
      "name": name,
      "value": value,
      "unset": unset
    });
    let _ = post_json(url, token, "/v1/tmux/set-environment", &value)?;
    Ok(0)
}

fn handle_show_environment(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut name: Option<String> = None;
    let mut value_only = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-s" => {
                index += 1;
            }
            "-h" => {
                index += 1;
            }
            "-v" => {
                value_only = true;
                index += 1;
            }
            arg if !arg.starts_with('-') && name.is_none() => {
                name = Some(arg.to_string());
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let mut path = format!("/v1/tmux/show-environment?valueOnly={}", value_only);
    if let Some(name) = name {
        path.push_str("&name=");
        path.push_str(&urlencoding::encode(&name));
    }
    let body = get_text(url, token, &path)?;
    if !body.is_empty() {
        println!("{body}");
    }
    Ok(0)
}

fn handle_set_hook(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut global = false;
    let mut unset = false;
    let mut name: Option<String> = None;
    let mut command: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-g" => {
                global = true;
                index += 1;
            }
            "-u" => {
                unset = true;
                index += 1;
            }
            arg if !arg.starts_with('-') => {
                if name.is_none() {
                    name = Some(arg.to_string());
                    index += 1;
                } else {
                    command = Some(args[index..].join(" "));
                    break;
                }
            }
            _ => {
                index += 1;
            }
        }
    }

    let Some(name) = name else {
        return Err("workspace-terminal-tmux: set-hook requires a hook name".to_string());
    };
    let value = json!({
      "target": target,
      "name": name,
      "command": command,
      "global": global,
      "unset": unset
    });
    let _ = post_json(url, token, "/v1/tmux/set-hook", &value)?;
    Ok(0)
}

fn handle_show_hooks(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut global = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-g" => {
                global = true;
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }
    let mut path = format!("/v1/tmux/show-hooks?global={global}");
    if let Some(target) = target {
        path.push_str("&target=");
        path.push_str(&urlencoding::encode(&target));
    }
    let body = get_text(url, token, &path)?;
    if !body.is_empty() {
        println!("{body}");
    }
    Ok(0)
}

fn handle_set_buffer(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut name: Option<String> = None;
    let mut append = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-b" => {
                if index + 1 < args.len() {
                    name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-a" => {
                append = true;
                index += 1;
            }
            _ => {
                break;
            }
        }
    }

    let value = if index < args.len() {
        args[index..].join(" ")
    } else {
        String::new()
    };
    let _ = post_json(
        url,
        token,
        "/v1/tmux/set-buffer",
        &json!({ "name": name, "value": value, "append": append }),
    )?;
    Ok(0)
}

fn handle_show_buffer(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut name: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-b" => {
                if index + 1 < args.len() {
                    name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }
    let path = match name {
        Some(name) => format!("/v1/tmux/show-buffer?name={}", urlencoding::encode(&name)),
        None => "/v1/tmux/show-buffer".to_string(),
    };
    let body = get_text(url, token, &path)?;
    if !body.is_empty() {
        println!("{body}");
    }
    Ok(0)
}

fn handle_list_buffers(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut format = "#{buffer_name}".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-F" => {
                if index + 1 < args.len() {
                    format = args[index + 1].clone();
                }
                index += 2;
            }
            _ => index += 1,
        }
    }
    let body = get_text(
        url,
        token,
        &format!("/v1/tmux/list-buffers?format={}", urlencoding::encode(&format)),
    )?;
    if !body.is_empty() {
        println!("{body}");
    }
    Ok(0)
}

fn handle_delete_buffer(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut name: Option<String> = None;
    let mut delete_all = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-b" => {
                if index + 1 < args.len() {
                    name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-a" => {
                delete_all = true;
                index += 1;
            }
            _ => index += 1,
        }
    }
    let _ = post_json(
        url,
        token,
        "/v1/tmux/delete-buffer",
        &json!({ "name": name, "deleteAll": delete_all }),
    )?;
    Ok(0)
}

fn handle_load_buffer(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut name: Option<String> = None;
    let mut path: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-b" => {
                if index + 1 < args.len() {
                    name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            arg if !arg.starts_with('-') && path.is_none() => {
                path = Some(arg.to_string());
                index += 1;
            }
            _ => index += 1,
        }
    }
    let Some(path) = path else {
        return Err("workspace-terminal-tmux: load-buffer requires a file path".to_string());
    };
    let _ = post_json(
        url,
        token,
        "/v1/tmux/load-buffer",
        &json!({ "name": name, "path": path }),
    )?;
    Ok(0)
}

fn handle_save_buffer(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut name: Option<String> = None;
    let mut path: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-b" => {
                if index + 1 < args.len() {
                    name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            arg if !arg.starts_with('-') && path.is_none() => {
                path = Some(arg.to_string());
                index += 1;
            }
            _ => index += 1,
        }
    }
    let Some(path) = path else {
        return Err("workspace-terminal-tmux: save-buffer requires a file path".to_string());
    };
    let _ = post_json(
        url,
        token,
        "/v1/tmux/save-buffer",
        &json!({ "name": name, "path": path }),
    )?;
    Ok(0)
}

fn handle_paste_buffer(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut name: Option<String> = None;
    let mut target: Option<String> = None;
    let mut delete_after = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-b" => {
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
            "-d" => {
                delete_after = true;
                index += 1;
            }
            _ => index += 1,
        }
    }
    let _ = post_json(
        url,
        token,
        "/v1/tmux/paste-buffer",
        &json!({ "name": name, "target": target, "deleteAfter": delete_after }),
    )?;
    Ok(0)
}

fn handle_wait_for(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut mode: Option<String> = None;
    let mut name: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-S" => {
                mode = Some("signal".to_string());
                index += 1;
            }
            "-L" => {
                mode = Some("lock".to_string());
                index += 1;
            }
            "-U" => {
                mode = Some("unlock".to_string());
                index += 1;
            }
            arg if !arg.starts_with('-') && name.is_none() => {
                name = Some(arg.to_string());
                index += 1;
            }
            _ => index += 1,
        }
    }
    let Some(name) = name else {
        return Err("workspace-terminal-tmux: wait-for requires a channel".to_string());
    };
    let _ = post_json(
        url,
        token,
        "/v1/tmux/wait-for",
        &json!({ "name": name, "mode": mode }),
    )?;
    Ok(0)
}

fn handle_set_option(
    url: &str,
    token: &str,
    args: &[String],
    is_window: bool,
) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut key: Option<String> = None;
    let mut value: Option<String> = None;
    let mut unset = false;
    let mut append = false;
    let mut only_if_unset = false;
    let mut global = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-u" => {
                unset = true;
                index += 1;
            }
            "-a" => {
                append = true;
                index += 1;
            }
            "-o" => {
                only_if_unset = true;
                index += 1;
            }
            "-g" => {
                global = true;
                index += 1;
            }
            "-q" => {
                index += 1;
            }
            arg if !arg.starts_with('-') => {
                if key.is_none() {
                    key = Some(arg.to_string());
                } else if value.is_none() {
                    value = Some(args[index..].join(" "));
                    break;
                } else {
                    index += 1;
                }
            }
            _ => {
                index += 1;
            }
        }
    }

    let Some(key) = key else {
        return Err("workspace-terminal-tmux: set-option requires an option name".to_string());
    };
    let endpoint = if is_window {
        "/v1/tmux/set-window-option"
    } else {
        "/v1/tmux/set-option"
    };
    let value = json!({
      "target": target,
      "key": key,
      "value": value,
      "unset": unset,
      "append": append,
      "onlyIfUnset": only_if_unset,
      "global": global
    });
    let _ = post_json(url, token, endpoint, &value)?;
    Ok(0)
}

fn handle_show_options(
    url: &str,
    token: &str,
    args: &[String],
    is_window: bool,
) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut key: Option<String> = None;
    let mut global = false;
    let mut value_only = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-g" => {
                global = true;
                index += 1;
            }
            "-v" => {
                value_only = true;
                index += 1;
            }
            "-q" | "-w" => {
                index += 1;
            }
            arg if !arg.starts_with('-') && key.is_none() => {
                key = Some(arg.to_string());
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let mut path = format!(
        "{}?global={}&valueOnly={}",
        if is_window {
            "/v1/tmux/show-window-options"
        } else {
            "/v1/tmux/show-options"
        },
        global,
        value_only
    );
    if let Some(target) = target {
        path.push_str("&target=");
        path.push_str(&urlencoding::encode(&target));
    }
    if let Some(key) = key {
        path.push_str("&key=");
        path.push_str(&urlencoding::encode(&key));
    }
    let body = get_text(url, token, &path)?;
    if !body.is_empty() {
        println!("{body}");
    }
    Ok(0)
}

fn handle_respawn_pane(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let value = parse_respawn_value(args);
    let response = post_json(url, token, "/v1/tmux/respawn-pane", &value)?;
    if let Some(pane_id) = response.get("paneId").and_then(|value| value.as_str()) {
        println!("{pane_id}");
    }
    Ok(0)
}

fn handle_respawn_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let value = parse_respawn_value(args);
    let response = post_json(url, token, "/v1/tmux/respawn-window", &value)?;
    if let Some(tab_id) = response.get("tabId").and_then(|value| value.as_str()) {
        println!("{tab_id}");
    }
    Ok(0)
}

fn parse_respawn_value(args: &[String]) -> serde_json::Value {
    let mut target: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut kill_existing = false;
    let mut env = serde_json::Map::new();
    let mut command_parts = Vec::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-c" => {
                if index + 1 < args.len() {
                    cwd = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-k" => {
                kill_existing = true;
                index += 1;
            }
            "-e" => {
                if index + 1 < args.len() {
                    if let Some((key, value)) = args[index + 1].split_once('=') {
                        env.insert(key.to_string(), serde_json::Value::String(value.to_string()));
                    }
                }
                index += 2;
            }
            "--" => {
                command_parts.extend(args[index + 1..].iter().cloned());
                break;
            }
            arg if !arg.starts_with('-') => {
                command_parts.extend(args[index..].iter().cloned());
                break;
            }
            _ => {
                index += 1;
            }
        }
    }

    let command = if command_parts.is_empty() {
        None
    } else {
        Some(command_parts.join(" "))
    };

    json!({
      "target": target,
      "command": command,
      "cwd": cwd,
      "killExisting": kill_existing,
      "env": if env.is_empty() { serde_json::Value::Null } else { serde_json::Value::Object(env) }
    })
}

fn handle_break_pane(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut source: Option<String> = None;
    let mut target: Option<String> = None;
    let mut name: Option<String> = None;
    let mut detached = false;
    let mut before = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-s" => {
                if index + 1 < args.len() {
                    source = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-n" => {
                if index + 1 < args.len() {
                    name = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-d" => {
                detached = true;
                index += 1;
            }
            "-b" => {
                before = true;
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "source": source,
      "target": target,
      "name": name,
      "detached": detached,
      "before": before
    });
    let response = post_json(url, token, "/v1/tmux/break-pane", &value)?;
    if let Some(tab_id) = response.get("tabId").and_then(|value| value.as_str()) {
        println!("{tab_id}");
    }
    Ok(0)
}

fn handle_join_pane(
    url: &str,
    token: &str,
    args: &[String],
    is_move: bool,
) -> Result<i32, String> {
    let mut source: Option<String> = None;
    let mut target: Option<String> = None;
    let mut direction: Option<String> = None;
    let mut detached = false;
    let mut before = false;
    let mut size: Option<u16> = None;
    let mut size_is_percentage = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-s" => {
                if index + 1 < args.len() {
                    source = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-h" => {
                direction = Some("horizontal".to_string());
                index += 1;
            }
            "-v" => {
                direction = Some("vertical".to_string());
                index += 1;
            }
            "-d" => {
                detached = true;
                index += 1;
            }
            "-b" => {
                before = true;
                index += 1;
            }
            "-l" => {
                if index + 1 < args.len() {
                    let (parsed, is_percent) = parse_tmux_size(&args[index + 1]);
                    size = parsed;
                    size_is_percentage = is_percent;
                }
                index += 2;
            }
            "-p" => {
                if index + 1 < args.len() {
                    size = args[index + 1]
                        .parse::<u16>()
                        .ok()
                        .map(|value| value.clamp(1, 99));
                    size_is_percentage = true;
                }
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "source": source,
      "target": target,
      "direction": direction,
      "detached": detached,
      "before": before,
      "size": size,
      "sizeIsPercentage": size_is_percentage
    });
    let path = if is_move {
        "/v1/tmux/move-pane"
    } else {
        "/v1/tmux/join-pane"
    };
    let response = post_json(url, token, path, &value)?;
    if let Some(pane_id) = response.get("paneId").and_then(|value| value.as_str()) {
        println!("{pane_id}");
    }
    Ok(0)
}

fn handle_swap_pane(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut source: Option<String> = None;
    let mut target: Option<String> = None;
    let mut detached = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-s" => {
                if index + 1 < args.len() {
                    source = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-d" => {
                detached = true;
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "source": source,
      "target": target,
      "detached": detached
    });
    let _ = post_json(url, token, "/v1/tmux/swap-pane", &value)?;
    Ok(0)
}

fn handle_swap_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut source: Option<String> = None;
    let mut target: Option<String> = None;
    let mut detached = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-s" => {
                if index + 1 < args.len() {
                    source = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-d" => {
                detached = true;
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "source": source,
      "target": target,
      "detached": detached
    });
    let _ = post_json(url, token, "/v1/tmux/swap-window", &value)?;
    Ok(0)
}

fn handle_rotate_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut direction: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-D" => {
                direction = Some("down".to_string());
                index += 1;
            }
            "-U" => {
                direction = Some("up".to_string());
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "target": target,
      "direction": direction
    });
    let _ = post_json(url, token, "/v1/tmux/rotate-window", &value)?;
    Ok(0)
}

fn handle_move_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut source: Option<String> = None;
    let mut target: Option<String> = None;
    let mut before = false;
    let mut after = false;
    let mut detached = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-s" => {
                if index + 1 < args.len() {
                    source = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-b" => {
                before = true;
                index += 1;
            }
            "-a" => {
                after = true;
                index += 1;
            }
            "-d" => {
                detached = true;
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "source": source,
      "target": target,
      "before": before,
      "after": after,
      "detached": detached
    });
    let _ = post_json(url, token, "/v1/tmux/move-window", &value)?;
    Ok(0)
}

fn handle_pipe_pane(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut pipe_output = false;
    let mut pipe_input = false;
    let mut only_if_none = false;
    let mut command_parts = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-I" => {
                pipe_input = true;
                index += 1;
            }
            "-O" => {
                pipe_output = true;
                index += 1;
            }
            "-o" => {
                only_if_none = true;
                index += 1;
            }
            "--" => {
                command_parts.extend(args[index + 1..].iter().cloned());
                break;
            }
            arg if !arg.starts_with('-') => {
                command_parts.extend(args[index..].iter().cloned());
                break;
            }
            _ => {
                index += 1;
            }
        }
    }

    let command = if command_parts.is_empty() {
        None
    } else {
        Some(command_parts.join(" "))
    };
    let value = json!({
      "target": target,
      "command": command,
      "pipeOutput": pipe_output,
      "pipeInput": pipe_input,
      "onlyIfNone": only_if_none
    });
    let _ = post_json(url, token, "/v1/tmux/pipe-pane", &value)?;
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

fn handle_list_windows(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut format = "#{window_id}".to_string();
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
    let body = get_text(url, token, &format!("/v1/tmux/list-windows?format={encoded}"))?;
    if !body.is_empty() {
        println!("{body}");
    }
    Ok(0)
}

fn handle_list_sessions(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut format = "#{session_id}".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-F" => {
                if index + 1 < args.len() {
                    format = args[index + 1].clone();
                }
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }

    let encoded = urlencoding::encode(&format);
    let body = get_text(url, token, &format!("/v1/tmux/list-sessions?format={encoded}"))?;
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

fn handle_kill_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
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
    let _ = post_json(url, token, "/v1/tmux/kill-window", &value)?;
    Ok(0)
}

fn handle_select_pane(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut direction: Option<String> = None;
    let mut last = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-L" => {
                direction = Some("left".to_string());
                index += 1;
            }
            "-R" => {
                direction = Some("right".to_string());
                index += 1;
            }
            "-U" => {
                direction = Some("up".to_string());
                index += 1;
            }
            "-D" => {
                direction = Some("down".to_string());
                index += 1;
            }
            "-l" => {
                last = true;
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "target": target,
      "direction": direction,
      "last": last
    });
    let _ = post_json(url, token, "/v1/tmux/select-pane", &value)?;
    Ok(0)
}

fn handle_last_pane(url: &str, token: &str) -> Result<i32, String> {
    let value = json!({ "last": true });
    let _ = post_json(url, token, "/v1/tmux/select-pane", &value)?;
    Ok(0)
}

fn handle_select_window(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut mode: Option<String> = None;
    let mut toggle_if_current = false;
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
                mode = Some("last".to_string());
                index += 1;
            }
            "-n" => {
                mode = Some("next".to_string());
                index += 1;
            }
            "-p" => {
                mode = Some("previous".to_string());
                index += 1;
            }
            "-T" => {
                toggle_if_current = true;
                index += 1;
            }
            _ if args[index].starts_with('-') => {
                index += 1;
            }
            _ => {
                if target.is_none() {
                    target = Some(args[index].clone());
                }
                index += 1;
            }
        }
    }

    let value = json!({
      "target": target,
      "mode": mode,
      "toggleIfCurrent": toggle_if_current
    });
    let _ = post_json(url, token, "/v1/tmux/select-window", &value)?;
    Ok(0)
}

fn handle_cycle_window(
    url: &str,
    token: &str,
    mode: &str,
    args: &[String],
) -> Result<i32, String> {
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "mode": mode
    });
    let _ = post_json(url, token, "/v1/tmux/select-window", &value)?;
    Ok(0)
}

fn handle_resize_pane(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut direction: Option<String> = None;
    let mut adjustment: Option<u16> = None;
    let mut width: Option<u16> = None;
    let mut height: Option<u16> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-L" => {
                direction = Some("left".to_string());
                adjustment = parse_optional_u16(args.get(index + 1));
                index += if adjustment.is_some() { 2 } else { 1 };
            }
            "-R" => {
                direction = Some("right".to_string());
                adjustment = parse_optional_u16(args.get(index + 1));
                index += if adjustment.is_some() { 2 } else { 1 };
            }
            "-U" => {
                direction = Some("up".to_string());
                adjustment = parse_optional_u16(args.get(index + 1));
                index += if adjustment.is_some() { 2 } else { 1 };
            }
            "-D" => {
                direction = Some("down".to_string());
                adjustment = parse_optional_u16(args.get(index + 1));
                index += if adjustment.is_some() { 2 } else { 1 };
            }
            "-x" => {
                width = parse_optional_u16(args.get(index + 1));
                index += 2;
            }
            "-y" => {
                height = parse_optional_u16(args.get(index + 1));
                index += 2;
            }
            "-Z" => {
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "target": target,
      "direction": direction,
      "adjustment": adjustment,
      "width": width,
      "height": height
    });
    let _ = post_json(url, token, "/v1/tmux/resize-pane", &value)?;
    Ok(0)
}

fn handle_capture_pane(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut target: Option<String> = None;
    let mut print_mode = false;
    let mut include_escape = false;
    let mut join_lines = false;
    let mut start_line: Option<i32> = None;
    let mut end_line: Option<i32> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => {
                if index + 1 < args.len() {
                    target = Some(args[index + 1].clone());
                }
                index += 2;
            }
            "-p" => {
                print_mode = true;
                index += 1;
            }
            "-e" => {
                include_escape = true;
                index += 1;
            }
            "-J" => {
                join_lines = true;
                index += 1;
            }
            "-S" => {
                start_line = args.get(index + 1).and_then(|value| value.parse::<i32>().ok());
                index += 2;
            }
            "-E" => {
                end_line = args.get(index + 1).and_then(|value| value.parse::<i32>().ok());
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }

    let value = json!({
      "target": target,
      "includeEscape": include_escape,
      "joinLines": join_lines,
      "startLine": start_line,
      "endLine": end_line
    });
    let body = post_text(url, token, "/v1/tmux/capture-pane", &value)?;
    if print_mode && !body.is_empty() {
        println!("{body}");
    }
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

fn handle_refresh_client(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-t" => index += 2,
            _ => index += 1,
        }
    }
    let _ = post_json(url, token, "/v1/tmux/refresh-client", &json!({}))?;
    Ok(0)
}

fn handle_source_file(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
    let path = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .cloned()
        .ok_or_else(|| "workspace-terminal-tmux: source-file requires a file path".to_string())?;
    let content = std::fs::read_to_string(&path)
        .map_err(|err| format!("workspace-terminal-tmux: failed to read source file: {err}"))?;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens = tokenize_tmux_command_line(line)?;
        if tokens.is_empty() {
            continue;
        }
        let subcmd = tokens[0].as_str();
        dispatch_tmux_command(url, token, subcmd, &tokens[1..])?;
    }

    Ok(0)
}

fn handle_has_session(url: &str, token: &str, args: &[String]) -> Result<i32, String> {
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

    let path = match target {
        Some(target) => format!(
            "/v1/tmux/has-session?target={}",
            urlencoding::encode(&target)
        ),
        None => "/v1/tmux/has-session".to_string(),
    };
    let _ = get_text(url, token, &path)?;
    Ok(0)
}

fn tokenize_tmux_command_line(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' if !in_single => {
                escape = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            '#' if !in_single && !in_double && current.is_empty() && tokens.is_empty() => {
                break;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                while chars.peek().is_some_and(|peek| peek.is_whitespace()) {
                    chars.next();
                }
            }
            _ => current.push(ch),
        }
    }

    if escape || in_single || in_double {
        return Err("workspace-terminal-tmux: unterminated quoted string in source-file".to_string());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
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

fn parse_tmux_size(value: &str) -> (Option<u16>, bool) {
    if let Some(percent) = value.strip_suffix('%') {
        let parsed = percent
            .parse::<u16>()
            .ok()
            .map(|entry| entry.clamp(1, 99));
        return (parsed, true);
    }

    (value.parse::<u16>().ok(), false)
}

fn render_tmux_response_format(format: &str, response: &serde_json::Value) -> String {
    let session_id = response
        .get("sessionId")
        .and_then(|value| value.as_str())
        .unwrap_or("$session");
    let pane_id = response
        .get("paneId")
        .and_then(|value| value.as_str())
        .map(|value| {
            if value.starts_with('%') {
                value.to_string()
            } else {
                format!("%{value}")
            }
        })
        .unwrap_or_default();
    let window_index = response
        .get("windowIndex")
        .and_then(|value| value.as_i64())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "0".to_string());
    let pane_index = response
        .get("paneIndex")
        .and_then(|value| value.as_i64())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "0".to_string());
    let pane_title = response
        .get("paneTitle")
        .and_then(|value| value.as_str())
        .unwrap_or("pane");
    let pane_current_command = response
        .get("paneCurrentCommand")
        .and_then(|value| value.as_str())
        .unwrap_or("terminal");
    let session_name = response
        .get("sessionName")
        .and_then(|value| value.as_str())
        .unwrap_or("session");
    let session_windows = response
        .get("sessionWindows")
        .and_then(|value| value.as_u64())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "0".to_string());
    let session_attached = response
        .get("sessionAttached")
        .and_then(|value| value.as_bool())
        .map(|value| if value { "1".to_string() } else { "0".to_string() })
        .unwrap_or_else(|| "0".to_string());
    let window_id = response
        .get("windowId")
        .and_then(|value| value.as_str())
        .unwrap_or("window");
    let window_name = response
        .get("windowName")
        .and_then(|value| value.as_str())
        .unwrap_or("window");
    let mut rendered = format.to_string();
    rendered = rendered.replace("#{session_id}", session_id);
    rendered = rendered.replace("#{session_windows}", &session_windows);
    rendered = rendered.replace("#{session_attached}", &session_attached);
    rendered = rendered.replace("#{pane_id}", &pane_id);
    rendered = rendered.replace("#{pane_index}", &pane_index);
    rendered = rendered.replace("#{pane_title}", pane_title);
    rendered = rendered.replace("#{pane_current_command}", pane_current_command);
    rendered = rendered.replace("#{window_index}", &window_index);
    rendered = rendered.replace("#{window_name}", window_name);
    rendered = rendered.replace("#{session_name}", session_name);
    rendered = rendered.replace("#{window_id}", window_id);
    rendered = rendered.replace("#D", &pane_id);
    rendered = rendered.replace("#I", &window_index);
    rendered = rendered.replace("#W", window_name);
    rendered = rendered.replace("#S", session_name);
    rendered
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

fn post_text(
    base_url: &str,
    token: &str,
    path: &str,
    value: &serde_json::Value,
) -> Result<String, String> {
    ureq::post(&format!("{base_url}{path}"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .send_json(value.clone())
        .map_err(|err| err.to_string())?
        .into_string()
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

fn parse_optional_u16(value: Option<&String>) -> Option<u16> {
    value.and_then(|entry| entry.parse::<u16>().ok())
}

#[cfg(test)]
mod tests {
    use super::dispatch_tmux_command;
    use std::{
        sync::mpsc,
        thread,
        time::Duration,
    };
    use tiny_http::{Header, Response, Server};

    fn spawn_capture_server(
        response_body: &str,
    ) -> (
        String,
        mpsc::Receiver<(String, String)>,
        thread::JoinHandle<()>,
    ) {
        let server = Server::http("127.0.0.1:0").expect("server");
        let port = server
            .server_addr()
            .to_ip()
            .map(|address| address.port())
            .expect("port");
        let base_url = format!("http://127.0.0.1:{port}");
        let (tx, rx) = mpsc::channel::<(String, String)>();
        let response_body = response_body.to_string();
        let handle = thread::spawn(move || {
            let mut request = server.recv().expect("request");
            let path = request.url().to_string();
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            tx.send((path, body)).expect("capture send");
            let response = Response::from_string(response_body).with_header(
                Header::from_bytes("Content-Type", "application/json").expect("content-type"),
            );
            request.respond(response).expect("respond");
        });

        (base_url, rx, handle)
    }

    #[test]
    fn dispatch_has_session_preserves_target_argument() {
        let (base_url, rx, handle) = spawn_capture_server("{\"ok\":true}");
        dispatch_tmux_command(
            &base_url,
            "token",
            "has-session",
            &["-t".to_string(), "session-1".to_string()],
        )
        .expect("dispatch has-session");

        let (path, body) = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("captured request");
        assert!(
            path.contains("target=session-1"),
            "expected target query in path, got: {path}"
        );
        assert!(body.is_empty(), "has-session should not send request body");
        handle.join().expect("server thread");
    }

    #[test]
    fn dispatch_split_window_preserves_first_flag() {
        let (base_url, rx, handle) = spawn_capture_server("{}");
        dispatch_tmux_command(
            &base_url,
            "token",
            "split-window",
            &["-h".to_string()],
        )
        .expect("dispatch split-window");

        let (path, body) = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("captured request");
        assert_eq!(path, "/v1/tmux/split-window");
        let payload: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(
            payload
                .get("direction")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
            "horizontal"
        );
        handle.join().expect("server thread");
    }
}
