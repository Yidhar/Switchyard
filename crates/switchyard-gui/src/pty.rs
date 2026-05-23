//! Real PTY backend for the embedded Terminal panel.
//!
//! Each open terminal tab in the GUI is backed by one `PtySession`
//! holding:
//!   - The PTY master (writer for user input + resize handle)
//!   - The spawned child process (a real shell — `cmd.exe` /
//!     `powershell.exe` on Windows, `$SHELL` / `/bin/sh` elsewhere)
//!   - A reader thread that pumps PTY output → Tauri `pty_output`
//!     events to the frontend (where xterm.js renders it)
//!
//! ## Why portable-pty?
//!
//! Cross-platform PTY without writing two backends:
//!   - Windows: uses ConPTY (Windows 10 1809+) so colors + cursor
//!     control work natively.
//!   - Unix: uses `openpty(3)` for the same.
//!
//! ## Threading model
//!
//! `Command::spawn` returns the child handle; `master.try_clone_reader`
//! gives us a `Box<dyn Read + Send>` we move into a dedicated OS
//! thread (PTY reads are blocking). Output is base64-encoded before
//! emit so binary bytes (ANSI escape control codes, UTF-8 multibyte,
//! and the occasional `0x00`) round-trip cleanly through JSON.
//! Frontend decodes with `atob()` + ` TextDecoder('utf-8', { fatal: false })`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use base64::Engine;
use portable_pty::{CommandBuilder, MasterPty, PtySize, SlavePty, native_pty_system};
use serde::Serialize;
use tauri::ipc::Channel;
use uuid::Uuid;

/// One live terminal. The `master` resize handle and the PTY input
/// `writer` are wrapped in separate mutexes so resize and keystroke
/// calls can borrow only the handle they need without holding the
/// sessions map lock.
///
/// Important portable-pty detail: `MasterPty::take_writer()` is a
/// one-shot operation. Dropping that writer sends EOF to the slave.
/// A live terminal must therefore take the writer once at creation
/// time and keep it alive for the whole tab lifetime.
///
/// On Windows, the `_slave` field must stay alive for the lifetime
/// of the session. ConPTY's pseudoconsole handle (HPCON) is tied to
/// the slave; dropping the slave end before the child exits closes
/// the pseudoconsole and the child's stdout writes disappear into
/// the void. portable-pty doesn't document this clearly but the
/// behavior matches Windows' ConPTY rules. On Unix it's just a
/// duplicate fd we could drop, but keeping it doesn't hurt.
struct PtySession {
    /// Holding the master keeps the slave end open. We pull a writer +
    /// resize handle off it; both share the underlying file
    /// descriptor.
    master: Arc<StdMutex<Box<dyn MasterPty + Send>>>,
    /// Persistent PTY input stream. Do not call `take_writer()` per
    /// keypress; portable-pty marks the writer as consumed after the
    /// first call, and dropping it sends EOF to the shell.
    writer: Arc<StdMutex<Box<dyn Write + Send>>>,
    /// Kept alive for ConPTY's sake — see the doc comment above.
    _slave: Box<dyn SlavePty + Send>,
    /// Stored so the child process remains alive while the terminal
    /// tab exists. `pty_close` removes the session and explicitly
    /// requests termination; merely dropping a `Child` handle is not
    /// a reliable cross-platform process kill.
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// Tauri-managed state. Plain `StdMutex<HashMap>` because every
/// operation is sub-millisecond and we don't want async lock
/// contention from the runtime.
pub struct PtyState {
    sessions: StdMutex<HashMap<Uuid, PtySession>>,
}

impl PtyState {
    pub fn new() -> Self {
        Self {
            sessions: StdMutex::new(HashMap::new()),
        }
    }
}

impl Default for PtyState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize)]
pub struct PtyCreated {
    pub pty_id: String,
}

/// Create a new PTY rooted at `cwd` with the host's default shell.
/// `cols`/`rows` come from the frontend's `FitAddon` measurement so
/// the shell sees a sensible terminal size from the first byte.
///
/// **Output delivery uses `Channel<T>`, not events.** With the old
/// `app.emit("pty_output:…")` approach there was a window between
/// `pty_create` returning and the frontend's `listen()` resolving
/// during which the shell's initial banner (and sometimes the first
/// prompt) emitted into a void. Channels are baked into the IPC at
/// command-call time, so the handler is hot before the reader
/// thread can possibly send.
#[tauri::command]
pub async fn pty_create(
    state: tauri::State<'_, PtyState>,
    cwd: String,
    cols: u16,
    rows: u16,
    on_output: Channel<String>,
    on_exit: Channel<i32>,
) -> Result<PtyCreated, String> {
    let cwd_path = PathBuf::from(&cwd);
    if !cwd_path.is_dir() {
        return Err(format!("cwd '{cwd}' is not a directory"));
    }

    let (id, session) = tokio::task::spawn_blocking(move || {
        create_pty_session_blocking(cwd_path, cols, rows, on_output, on_exit)
    })
    .await
    .map_err(|e| format!("pty create task join failed: {e}"))??;

    {
        let mut sessions = state
            .sessions
            .lock()
            .map_err(|_| "pty state poisoned".to_string())?;
        sessions.insert(id, session);
    }

    Ok(PtyCreated {
        pty_id: id.to_string(),
    })
}

fn create_pty_session_blocking(
    cwd_path: PathBuf,
    cols: u16,
    rows: u16,
    on_output: Channel<String>,
    on_exit: Channel<i32>,
) -> Result<(Uuid, PtySession), String> {
    let debug = pty_debug_enabled();
    if debug {
        let worker_probe = base64::engine::general_purpose::STANDARD
            .encode("\x1b[36m[switchyard] PTY worker started\x1b[0m\r\n".as_bytes());
        let _ = on_output.send(worker_probe);
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty failed: {e}"))?;

    let shell = default_shell();
    let mut cmd = CommandBuilder::new(&shell);
    cmd.cwd(&cwd_path);
    // CommandBuilder inherits the parent process env by default —
    // explicit `env()` calls layer ON TOP. Forwarding every variable
    // from `std::env::vars()` in a loop was redundant and risked
    // overriding portable-pty's own staging (e.g. it sets a few
    // ConPTY-specific markers internally). Just set TERM so curses
    // apps know to use ANSI.
    cmd.env("TERM", "xterm-256color");
    let shell_lower = shell.to_ascii_lowercase();
    if cfg!(target_os = "windows") && shell_lower.ends_with("cmd.exe") {
        // On Windows, cmd.exe runs the registry-defined AutoRun
        // command when it starts. Buggy AutoRun (a vendored doskey
        // macro that can't load, an `init.bat` that prints nothing
        // then waits for input, …) is the #1 cause of "cmd.exe
        // spawned but produced no banner". `/D` disables AutoRun —
        // same flag VS Code's terminal uses for the same reason.
        cmd.arg("/D");
    } else if cfg!(target_os = "windows")
        && (shell_lower.ends_with("pwsh.exe") || shell_lower.ends_with("powershell.exe"))
        && std::env::var("SWITCHYARD_SHELL_LOAD_PROFILE").as_deref() != Ok("1")
    {
        // PowerShell's profile.ps1 is the #1 cause of "shell spawned
        // but no banner appeared" — modules that hang, network
        // probes that time out, oh-my-posh themes that re-fetch
        // remote glyphs, etc. Default to `-NoLogo -NoProfile` so the
        // terminal always starts within ~100 ms. Users who want
        // their profile back can `$env:SWITCHYARD_SHELL_LOAD_PROFILE
        // = "1"` before launching Switchyard.
        cmd.arg("-NoLogo");
        cmd.arg("-NoProfile");
    }

    // Optional sanity probe. Keep disabled by default: synthetic lines written
    // directly into the emulator mutate cursor/screen state and make terminal
    // refresh/TUI behavior look wrong. Set SWITCHYARD_PTY_DEBUG=1 when
    // debugging startup plumbing.
    if debug {
        let banner = base64::engine::general_purpose::STANDARD.encode(
            format!(
                "\x1b[36m[switchyard] PTY ready ({}x{}), spawning shell: {}\x1b[0m\r\n",
                cols, rows, shell
            )
            .as_bytes(),
        );
        let _ = on_output.send(banner);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn shell failed: {e}"))?;

    // Diagnostic: did the shell exit immediately? An immediate exit
    // (typical when AutoRun is broken or the path/cwd is invalid)
    // shows up here. We wait briefly so the OS has a chance to
    // schedule the child, then poll. If it's still running we
    // proceed; if it died, surface the exit code so the user knows
    // why their terminal looks empty.
    std::thread::sleep(std::time::Duration::from_millis(80));
    match child.try_wait() {
        Ok(Some(status)) => {
            let msg =
                format!("\x1b[31m[switchyard] shell exited immediately: {status:?}\x1b[0m\r\n");
            let _ =
                on_output.send(base64::engine::general_purpose::STANDARD.encode(msg.as_bytes()));
        }
        Ok(None) => {
            if debug {
                let _ = on_output.send(
                    base64::engine::general_purpose::STANDARD
                        .encode("\x1b[36m[switchyard] shell is running\x1b[0m\r\n".as_bytes()),
                );
            }
        }
        Err(e) => {
            if debug {
                let msg = format!("\x1b[31m[switchyard] try_wait error: {e}\x1b[0m\r\n");
                let _ = on_output
                    .send(base64::engine::general_purpose::STANDARD.encode(msg.as_bytes()));
            }
        }
    }

    // Pull the reader off the master BEFORE moving the master into
    // the Arc<Mutex<…>>. portable-pty's reader implements Read + Send
    // but not Sync, so it must live in a dedicated thread.
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone reader: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take writer: {e}"))?;

    let id = Uuid::now_v7();

    // Reader thread — drains PTY output into the output channel. The
    // channel is already wired on the frontend by the time `invoke`
    // returns the Channel handle to us, so no message can be lost.
    std::thread::spawn(move || pump_reader(reader, on_output, on_exit, debug));

    Ok((
        id,
        PtySession {
            master: Arc::new(StdMutex::new(pair.master)),
            writer: Arc::new(StdMutex::new(writer)),
            _slave: pair.slave,
            child,
        },
    ))
}

/// Write user input bytes (typed keys, paste, escape sequences) to
/// the PTY. `data` is base64-encoded so control bytes and binary
/// paste payloads round-trip safely through Tauri's JSON IPC.
#[tauri::command]
pub fn pty_write(
    state: tauri::State<'_, PtyState>,
    pty_id: String,
    data: String,
) -> Result<(), String> {
    let id = parse_uuid(&pty_id)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("decode base64: {e}"))?;
    let session_writer = {
        let sessions = state
            .sessions
            .lock()
            .map_err(|_| "pty state poisoned".to_string())?;
        sessions
            .get(&id)
            .map(|s| Arc::clone(&s.writer))
            .ok_or_else(|| format!("pty {pty_id} not found"))?
    };
    let mut writer = session_writer
        .lock()
        .map_err(|_| "pty writer poisoned".to_string())?;
    writer
        .write_all(&bytes)
        .map_err(|e| format!("pty write: {e}"))?;
    writer.flush().map_err(|e| format!("pty flush: {e}"))?;
    Ok(())
}

/// Resize the PTY. xterm.js's FitAddon recomputes columns/rows
/// whenever the panel changes size; we forward each new size so the
/// shell wraps lines correctly.
#[tauri::command]
pub fn pty_resize(
    state: tauri::State<'_, PtyState>,
    pty_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let id = parse_uuid(&pty_id)?;
    let session_master = {
        let sessions = state
            .sessions
            .lock()
            .map_err(|_| "pty state poisoned".to_string())?;
        sessions
            .get(&id)
            .map(|s| Arc::clone(&s.master))
            .ok_or_else(|| format!("pty {pty_id} not found"))?
    };
    let guard = session_master
        .lock()
        .map_err(|_| "pty master poisoned".to_string())?;
    guard
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("pty resize: {e}"))?;
    Ok(())
}

/// Tear down a PTY. Dropping the writer/master/slave closes the PTY
/// endpoints; we also explicitly kill the child so closing a tab
/// cannot leave a hidden shell process alive.
#[tauri::command]
pub fn pty_close(state: tauri::State<'_, PtyState>, pty_id: String) -> Result<(), String> {
    let id = parse_uuid(&pty_id)?;
    let mut sessions = state
        .sessions
        .lock()
        .map_err(|_| "pty state poisoned".to_string())?;
    if let Some(mut session) = sessions.remove(&id) {
        let _ = session.child.kill();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("invalid pty id '{s}': {e}"))
}

fn pty_debug_enabled() -> bool {
    matches!(
        std::env::var("SWITCHYARD_PTY_DEBUG").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    )
}

/// Default shell to spawn for the current OS.
///
/// Resolution order (Windows):
///   1. `$SWITCHYARD_SHELL` — explicit override for power users who
///      want fish-on-WSL, nu, etc.
///   2. Standard local PowerShell 7 install locations (`pwsh.exe`).
///      First-choice on Windows: pairs cleanly with ConPTY, ANSI by
///      default, no AutoRun footguns.
///   3. Standard local Windows PowerShell 5.1 location. Reliable
///      fallback and avoids scanning PATH during terminal startup.
///   4. `$ComSpec` / `cmd.exe` — last resort. cmd has been the source
///      of every "PTY spawns but produces no output" we've hit
///      (AutoRun hooks, doskey macros), so we avoid it unless the
///      user has no other shell installed.
///
/// On Unix it's just `$SHELL` (or `/bin/sh`); the cascade above is
/// Windows-specific.
fn default_shell() -> String {
    if let Ok(custom) = std::env::var("SWITCHYARD_SHELL") {
        if !custom.trim().is_empty() {
            return custom;
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Do not scan PATH while opening the GUI terminal. Corporate
        // machines and dev boxes often have slow/offline network
        // entries in PATH; `Path::is_file()` against those locations
        // can block terminal startup for seconds. Prefer known local
        // shell locations and let power users opt into anything else
        // with SWITCHYARD_SHELL.
        if let Some(p) = standard_windows_pwsh() {
            return p.to_string_lossy().into_owned();
        }
        if let Some(p) = standard_windows_powershell() {
            return p.to_string_lossy().into_owned();
        }
        return std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

#[cfg(target_os = "windows")]
fn standard_windows_pwsh() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        candidates.push(
            PathBuf::from(program_files)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe"),
        );
    }
    if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
        candidates.push(
            PathBuf::from(program_files_x86)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe"),
        );
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local_app_data)
                .join("Microsoft")
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe"),
        );
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(target_os = "windows")]
fn standard_windows_powershell() -> Option<PathBuf> {
    let windir = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("WINDIR"))
        .unwrap_or_else(|| "C:\\Windows".into());
    let candidate = PathBuf::from(windir)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    candidate.is_file().then_some(candidate)
}

/// PTY reader loop. Runs in its own OS thread; sends base64-encoded
/// payloads down the output channel so binary bytes (ANSI escape
/// codes, UTF-8 multibyte) survive the JSON IPC. Exits silently on
/// EOF (the shell has closed); the exit channel fires once after.
///
/// Sends a second sanity probe on entry so users can tell whether the
/// reader thread booted at all (vs. the main thread's probe being the
/// only message they see).
fn pump_reader(
    mut reader: Box<dyn Read + Send>,
    on_output: Channel<String>,
    on_exit: Channel<i32>,
    debug: bool,
) {
    if debug {
        let probe = base64::engine::general_purpose::STANDARD.encode(
            "\x1b[36m[switchyard] reader thread entered, awaiting bytes…\x1b[0m\r\n".as_bytes(),
        );
        let _ = on_output.send(probe);
    }

    // Use a moderately large read buffer to reduce IPC wakeups during heavy
    // output/TUI redraws. The frontend still applies animation-frame batching
    // and xterm write backpressure before rendering.
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                // Best-effort send — if the window closed mid-read,
                // the channel send errors and we'll catch EOF on the
                // next iteration.
                if on_output.send(encoded).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = on_exit.send(0);
}
