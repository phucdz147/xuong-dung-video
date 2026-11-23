// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Xưởng Dựng Video contributors

//! Xưởng Dựng Video: the agent's column, the window's half.
//!
//! The agent is Claude Code, already installed on this machine and signed
//! in, started by the window as a child process that speaks JSON lines
//! (`claude -p --input-format stream-json --output-format stream-json`).
//! It reaches the editor the way every agent does, through `concat --mcp`
//! and the window's server, so what it edits is what the person sees (see
//! `crate::live`). This file only carries the conversation: what the
//! person writes goes in on the child's standard input, what the agent
//! says and does comes out on its standard output and lands in the column.
//!
//! A step the rules say to ask about - an export, a change of frame, a
//! command - comes out as a `can_use_tool` request
//! (`--permission-prompt-tool stdio`); the column shows it as a card, and
//! the person's Duyệt or Từ chối goes back as the answer. Nothing is
//! approved without them.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;

use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::host::Shell;
use crate::ui::{Agent, AgentLine, App};

/// The conversation and the process behind it, on the window's thread.
struct Chat {
    lines: Rc<VecModel<AgentLine>>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    /// The step waiting for the person: the request it answers, and the
    /// tool's input, handed back unchanged with a yes.
    pending: Option<(String, Value)>,
    /// Which process the events belong to; one from a process already
    /// restarted is dropped.
    generation: u64,
    /// Whether the turn ended with a result, so a process that exits
    /// without one is said to have stopped.
    answered: bool,
}

thread_local! {
    static CHAT: RefCell<Option<Chat>> = const { RefCell::new(None) };
}

/// Hands the column its model and connects its buttons. Called once, from
/// the window's setup.
pub fn install(app: &App) {
    let lines = Rc::new(VecModel::<AgentLine>::default());
    let agent = app.global::<Agent>();
    agent.set_lines(ModelRc::from(lines.clone()));
    agent.set_status(SharedString::from(
        "Sẵn sàng · dùng Claude Code trên máy này",
    ));
    CHAT.with(|chat| {
        *chat.borrow_mut() = Some(Chat {
            lines,
            child: None,
            stdin: None,
            pending: None,
            generation: 0,
            answered: true,
        })
    });
    agent.on_send(|text| send(text.to_string()));
    agent.on_answer(answer);
    agent.on_stop(stop);
    agent.on_restart(restart);
}

fn with_chat<T>(body: impl FnOnce(&mut Chat) -> T) -> Option<T> {
    CHAT.with(|chat| chat.borrow_mut().as_mut().map(body))
}

fn with_agent(body: impl FnOnce(Agent<'_>)) {
    Shell::with(|_, app| body(app.global::<Agent>()));
}

fn push(role: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    with_chat(|chat| {
        chat.lines.push(AgentLine {
            role: role.into(),
            text: text.into(),
        })
    });
    with_agent(|agent| agent.set_revision(agent.get_revision() + 1));
}

fn set_status(busy: bool, status: &str) {
    with_agent(|agent| {
        agent.set_busy(busy);
        agent.set_status(SharedString::from(status));
    });
}

/// What the person wrote: started if need be, then sent with a line on
/// which project is on screen, so "sửa trực tiếp" has something to mean.
fn send(text: String) {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return;
    }
    push("you", &text);
    if with_chat(|chat| chat.stdin.is_none()).unwrap_or(true)
        && let Err(why) = start()
    {
        push("error", &why);
        set_status(false, "Chưa chạy được agent");
        return;
    }
    let mut on_screen = None;
    Shell::with(|shell, _| {
        on_screen = shell
            .studio
            .borrow()
            .session
            .as_ref()
            .map(|session| session.path().to_owned());
    });
    let context = match on_screen {
        Some(path) => format!("[Xưởng Dựng Video · project đang mở trên màn hình: {path}]\n"),
        None => "[Xưởng Dựng Video · chưa mở project nào trên màn hình]\n".to_owned(),
    };
    let message = json!({
        "type": "user",
        "message": { "role": "user", "content": format!("{context}{text}") },
    });
    if let Err(why) = write_line(&message) {
        push("error", &format!("Không gửi được cho agent: {why}"));
        return;
    }
    with_chat(|chat| chat.answered = false);
    set_status(true, "Agent đang làm…");
}

fn write_line(value: &Value) -> Result<(), String> {
    with_chat(|chat| {
        let stdin = chat.stdin.as_mut().ok_or("agent chưa chạy")?;
        let mut line = value.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|error| error.to_string())
    })
    .unwrap_or_else(|| Err("agent chưa chạy".to_owned()))
}

/// The person's Duyệt (true) or Từ chối (false) on the waiting step.
fn answer(allow: bool) {
    let Some((id, input)) = with_chat(|chat| chat.pending.take()).flatten() else {
        return;
    };
    let response = if allow {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": "Phúc đã từ chối bước này. Đừng thử lại cách khác; hỏi Phúc muốn làm gì." })
    };
    let reply = json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": id, "response": response },
    });
    if let Err(why) = write_line(&reply) {
        push("error", &format!("Không gửi được câu trả lời: {why}"));
    }
    with_agent(|agent| agent.set_asking(false));
    push(
        "note",
        if allow {
            "✓ Anh đã duyệt"
        } else {
            "✕ Anh đã từ chối"
        },
    );
    set_status(true, "Agent đang làm…");
}

/// Stops the turn in progress; the conversation stays.
fn stop() {
    let interrupt = json!({
        "type": "control_request",
        "request_id": format!("dung-{}", std::process::id()),
        "request": { "subtype": "interrupt" },
    });
    if with_chat(|chat| chat.pending.is_some()).unwrap_or(false) {
        answer(false);
    }
    let _ = write_line(&interrupt);
    push("note", "Anh đã bảo agent dừng");
}

/// Ends the process and the conversation, for a fresh one.
fn restart() {
    with_chat(|chat| {
        chat.generation += 1;
        chat.stdin = None;
        if let Some(mut child) = chat.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        chat.pending = None;
        chat.answered = true;
        chat.lines.set_vec(Vec::new());
    });
    with_agent(|agent| agent.set_asking(false));
    set_status(false, "Chat mới · sẵn sàng");
}

/// The app's folder: the one `du-lieu` and `cong-cu` sit in, above the
/// folder the executable is in.
fn app_root() -> PathBuf {
    let exe = std::env::current_exe().ok();
    exe.as_deref()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|root| root.join("du-lieu").is_dir() || root.join("cong-cu").is_dir())
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Claude Code's program: `claude.exe` where the native installer put it,
/// or the `claude.cmd` npm makes, found on the path.
fn find_claude() -> Option<(PathBuf, bool)> {
    let mut places: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        places.push(PathBuf::from(&home).join(".local").join("bin"));
    }
    let names: &[(&str, bool)] = if cfg!(windows) {
        &[("claude.exe", false), ("claude.cmd", true)]
    } else {
        &[("claude", false)]
    };
    for (name, through_shell) in names {
        for place in &places {
            let candidate = place.join(name);
            if candidate.is_file() {
                return Some((candidate, *through_shell));
            }
        }
    }
    None
}

fn start() -> Result<(), String> {
    let (program, through_shell) = find_claude().ok_or(
        "Không tìm thấy Claude Code trên máy này. Cài Claude Code rồi mở lại app.".to_owned(),
    )?;
    let root = app_root();
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-prompt-tool",
        "stdio",
        "--allowedTools",
        "mcp__concat",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect();
    // Relative to the app's folder, where the child starts: the path holds
    // no space and no accent that a shell in between could trip on.
    if root.join("cong-cu").join("claude-settings.json").is_file() {
        args.push("--settings".to_owned());
        args.push(
            Path::new("cong-cu")
                .join("claude-settings.json")
                .to_string_lossy()
                .into_owned(),
        );
    }
    let mut command = if through_shell {
        let mut command = Command::new("cmd.exe");
        command.arg("/c").arg(&program).args(&args);
        command
    } else {
        let mut command = Command::new(&program);
        command.args(&args);
        command
    };
    command
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window of its own behind the editor.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|error| {
        format!(
            "Không chạy được Claude Code ({}): {error}",
            program.display()
        )
    })?;
    log::info!("agent: started {} in {}", program.display(), root.display());
    let stdout = child.stdout.take().ok_or("không đọc được agent")?;
    let stderr = child.stderr.take();
    let stdin = child.stdin.take();
    let generation = with_chat(|chat| {
        chat.generation += 1;
        chat.child = Some(child);
        chat.stdin = stdin;
        chat.generation
    })
    .unwrap_or(0);

    std::thread::Builder::new()
        .name("agent out".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let _ = slint::invoke_from_event_loop(move || on_event(generation, event));
            }
            let _ = slint::invoke_from_event_loop(move || on_ended(generation));
        })
        .map_err(|error| error.to_string())?;
    if let Some(stderr) = stderr {
        let _ = std::thread::Builder::new()
            .name("agent err".to_owned())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::warn!("agent: {line}");
                }
            });
    }
    set_status(true, "Đang khởi động agent…");
    Ok(())
}

fn current(generation: u64) -> bool {
    with_chat(|chat| chat.generation == generation).unwrap_or(false)
}

fn on_event(generation: u64, event: Value) {
    if !current(generation) {
        return;
    }
    let kind = event["type"].as_str().unwrap_or("");
    match kind {
        "system" if event["subtype"] == "init" => {
            let model = event["model"].as_str().unwrap_or("");
            set_status(true, &format!("Agent đang làm… · {model}"));
        }
        "assistant" => {
            for block in event["message"]["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => push("agent", block["text"].as_str().unwrap_or("")),
                    Some("tool_use") => {
                        let name = block["name"].as_str().unwrap_or("");
                        push("tool", &format!("→ {}", friendly(name, &block["input"])));
                    }
                    _ => {}
                }
            }
        }
        "control_request" if event["request"]["subtype"] == "can_use_tool" => {
            let id = event["request_id"].as_str().unwrap_or("").to_owned();
            let request = &event["request"];
            let name = request["tool_name"].as_str().unwrap_or("");
            let input = request["input"].clone();
            let title = friendly(name, &input);
            let detail = detail(&input);
            with_chat(|chat| chat.pending = Some((id, input)));
            with_agent(|agent| {
                agent.set_ask_title(SharedString::from(title));
                agent.set_ask_detail(SharedString::from(detail));
                agent.set_asking(true);
            });
            set_status(true, "Đang chờ anh duyệt");
        }
        "result" => {
            with_chat(|chat| chat.answered = true);
            if event["is_error"].as_bool().unwrap_or(false) {
                let why = event["result"].as_str().unwrap_or("agent báo lỗi");
                push("error", why);
            }
            set_status(false, "Xong · nhắn tiếp nếu cần");
        }
        _ => {}
    }
}

fn on_ended(generation: u64) {
    if !current(generation) {
        return;
    }
    let answered = with_chat(|chat| {
        chat.stdin = None;
        if let Some(mut child) = chat.child.take() {
            let _ = child.wait();
        }
        chat.pending = None;
        chat.answered
    })
    .unwrap_or(true);
    with_agent(|agent| agent.set_asking(false));
    if !answered {
        push(
            "error",
            "Agent dừng giữa chừng. Kiểm tra Claude Code đã đăng nhập chưa (mở cửa sổ Claude gõ /login), rồi gửi lại.",
        );
    }
    set_status(false, "Agent nghỉ · gửi tin để mở lại");
}

/// A tool call as the person would say it.
fn friendly(name: &str, input: &Value) -> String {
    let tool = name.strip_prefix("mcp__concat__").unwrap_or(name);
    let said = match tool {
        "export_start" => "Xuất video",
        "export_cancel" => "Huỷ xuất video",
        "project_set_video" => "Đổi khung hình / kích thước video",
        "project_open" => "Mở project",
        "project_create" => "Tạo project",
        "project_save" => "Lưu project",
        "project_close" => "Đóng project",
        "media_import" => "Nhập file vào project",
        "captions_add" => "Thêm phụ đề",
        "title_add" => "Thêm chữ",
        "clips_split" => "Cắt clip",
        "clips_remove" => "Xoá clip",
        "clip_trim" => "Cắt bớt clip",
        "edit_apply" => "Sửa timeline",
        "edit_undo" => "Hoàn tác",
        "preview_frame" => "Xem một khung hình",
        "timeline_read" => "Đọc timeline",
        "Bash" | "PowerShell" => "Chạy lệnh",
        "Write" => "Ghi file",
        "Edit" => "Sửa file",
        "Read" => "Đọc file",
        _ => "",
    };
    let target = input["output"]
        .as_str()
        .or_else(|| input["path"].as_str())
        .or_else(|| input["file_path"].as_str())
        .map(|path| {
            Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_owned())
        });
    match (said.is_empty(), target) {
        (false, Some(target)) => format!("{said}: {target}"),
        (false, None) => said.to_owned(),
        (true, Some(target)) => format!("{tool}: {target}"),
        (true, None) => tool.to_owned(),
    }
}

/// What a step will do, in enough detail to say yes or no to.
fn detail(input: &Value) -> String {
    if let Some(command) = input["command"].as_str() {
        return command.to_owned();
    }
    let mut text = serde_json::to_string_pretty(input).unwrap_or_default();
    if text.chars().count() > 600 {
        text = text.chars().take(600).collect::<String>() + "…";
    }
    text
}

/// Ends the agent with the window, so no process is left behind.
pub fn shut_down() {
    with_chat(|chat| {
        chat.stdin = None;
        if let Some(mut child) = chat.child.take() {
            let _ = child.kill();
        }
    });
}
