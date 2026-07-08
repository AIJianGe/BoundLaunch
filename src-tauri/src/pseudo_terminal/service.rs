//! 伪终端服务
//!
//! 管理多个终端会话，每个会话独立运行 shell。
//! 输出通过 Tauri 事件（pty_output / pty_exit）推送到前端。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use chrono::Utc;
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, NativePtySystem, PtySize as PtySysSize,
    PtySystem, SlavePty,
};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::pseudo_terminal::models::{PtySize, TerminalSessionInfo};

struct ChildState {
    child: Option<Box<dyn Child + Send + Sync>>,
    is_alive: bool,
    exit_code: Option<i32>,
}

struct TerminalSession {
    master: Box<dyn MasterPty + Send>,
    _slave: Box<dyn SlavePty + Send>,
    child_state: Arc<parking_lot::Mutex<ChildState>>,
    write_tx: mpsc::Sender<Vec<u8>>,
    size: PtySize,
    shell: String,
    cwd: String,
    created_at: String,
}

pub struct PseudoTerminalService {
    sessions: parking_lot::Mutex<HashMap<String, TerminalSession>>,
    pty_system: NativePtySystem,
}

impl PseudoTerminalService {
    pub fn new() -> Self {
        Self {
            sessions: parking_lot::Mutex::new(HashMap::new()),
            pty_system: NativePtySystem::default(),
        }
    }

    pub async fn create_session(
        &self,
        app: AppHandle,
        shell: Option<String>,
        cwd: Option<String>,
        size: Option<PtySize>,
    ) -> Result<TerminalSessionInfo, String> {
        let session_id = Uuid::new_v4().to_string();
        let size = size.unwrap_or_default();
        let shell = shell.unwrap_or_else(default_shell);
        let cwd = cwd.unwrap_or_else(default_cwd);

        let pair = self
            .pty_system
            .openpty(PtySysSize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("failed to open pty: {}", e))?;

        let master = pair.master;
        let slave = pair.slave;

        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(PathBuf::from(&cwd));

        let child = slave
            .spawn_command(cmd)
            .map_err(|e| format!("failed to spawn shell: {}", e))?;

        let reader = master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = master.try_clone_writer().map_err(|e| e.to_string())?;

        let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(1024);
        let created_at = Utc::now().to_rfc3339();

        let child_state = Arc::new(parking_lot::Mutex::new(ChildState {
            child: Some(child),
            is_alive: true,
            exit_code: None,
        }));

        // reader 线程
        let session_id_clone = session_id.clone();
        let app_clone = app.clone();
        tokio::spawn(async move {
            use std::io::Read;

            let mut buf = [0u8; 4096];
            let mut reader = std::io::BufReader::new(reader);

            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        tracing::debug!(session_id = %session_id_clone, "pty reader EOF");
                        break;
                    }
                    Ok(n) => {
                        let data = &buf[..n];
                        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                        let _ = app_clone.emit(
                            "pty_output",
                            serde_json::json!({
                                "session_id": session_id_clone,
                                "data": b64,
                            }),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(session_id = %session_id_clone, error = %e, "pty read error");
                        break;
                    }
                }
            }
        });

        // writer 线程
        tokio::spawn(async move {
            use std::io::Write;

            let mut writer = writer;

            while let Some(data) = write_rx.recv().await {
                match writer.write_all(&data) {
                    Ok(_) => {
                        let _ = writer.flush();
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "pty write error");
                        break;
                    }
                }
            }
        });

        // exit 监听线程
        let child_state_exit = child_state.clone();
        let child_state_update = child_state.clone();
        let session_id_exit = session_id.clone();
        let app_exit = app.clone();
        tokio::spawn(async move {
            let exit_code = tokio::task::spawn_blocking(move || {
                let mut state = child_state_exit.lock();
                let child = match state.child.as_mut() {
                    Some(c) => c,
                    None => return None,
                };
                wait_child_exit(&mut **child)
            })
            .await
            .unwrap_or(None);

            {
                let mut state = child_state_update.lock();
                state.is_alive = false;
                state.exit_code = exit_code;
            }

            let _ = app_exit.emit(
                "pty_exit",
                serde_json::json!({
                    "session_id": session_id_exit,
                    "exit_code": exit_code,
                }),
            );

            tracing::info!(session_id = %session_id_exit, ?exit_code, "pty session exited");
        });

        let session = TerminalSession {
            master,
            _slave: slave,
            child_state,
            write_tx,
            size,
            shell: shell.clone(),
            cwd: cwd.clone(),
            created_at: created_at.clone(),
        };

        self.sessions.lock().insert(session_id.clone(), session);

        tracing::info!(session_id = %session_id, shell = %shell, cwd = %cwd, "pty session created");

        Ok(TerminalSessionInfo {
            session_id,
            shell,
            cwd,
            size,
            is_alive: true,
            exit_code: None,
            created_at,
        })
    }

    pub async fn write(&self, session_id: &str, data: &[u8]) -> Result<(), String> {
        let (write_tx, is_alive) = {
            let guard = self.sessions.lock();
            let session = guard
                .get(session_id)
                .ok_or_else(|| format!("session not found: {}", session_id))?;
            let state = session.child_state.lock();
            (session.write_tx.clone(), state.is_alive)
        };

        if !is_alive {
            return Err("session already exited".into());
        }

        write_tx
            .send(data.to_vec())
            .await
            .map_err(|e| format!("write channel closed: {}", e))?;

        Ok(())
    }

    pub async fn resize(&self, session_id: &str, size: PtySize) -> Result<(), String> {
        let mut guard = self.sessions.lock();
        let session = guard
            .get_mut(session_id)
            .ok_or_else(|| format!("session not found: {}", session_id))?;

        session
            .master
            .resize(PtySysSize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("resize failed: {}", e))?;

        session.size = size;
        Ok(())
    }

    pub async fn close(&self, session_id: &str) -> Result<(), String> {
        let mut guard = self.sessions.lock();
        let session = match guard.remove(session_id) {
            Some(s) => s,
            None => return Err(format!("session not found: {}", session_id)),
        };
        drop(guard);

        let mut state = session.child_state.lock();
        if state.is_alive {
            if let Some(ref mut child) = state.child {
                let _ = child.kill();
            }
        }

        tracing::info!(session_id = %session_id, "pty session closed");
        Ok(())
    }

    pub fn list_sessions(&self) -> Vec<TerminalSessionInfo> {
        let guard = self.sessions.lock();
        guard
            .iter()
            .map(|(id, s)| {
                let state = s.child_state.lock();
                TerminalSessionInfo {
                    session_id: id.clone(),
                    shell: s.shell.clone(),
                    cwd: s.cwd.clone(),
                    size: s.size,
                    is_alive: state.is_alive,
                    exit_code: state.exit_code,
                    created_at: s.created_at.clone(),
                }
            })
            .collect()
    }

    pub fn get_session(&self, session_id: &str) -> Option<TerminalSessionInfo> {
        let guard = self.sessions.lock();
        guard.get(session_id).map(|s| {
            let state = s.child_state.lock();
            TerminalSessionInfo {
                session_id: session_id.to_string(),
                shell: s.shell.clone(),
                cwd: s.cwd.clone(),
                size: s.size,
                is_alive: state.is_alive,
                exit_code: state.exit_code,
                created_at: s.created_at.clone(),
            }
        })
    }
}

fn default_shell() -> String {
    if cfg!(windows) {
        "powershell.exe".into()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
}

fn default_cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".into())
}

fn wait_child_exit(child: &mut (dyn Child + Send + Sync)) -> Option<i32> {
    match child.wait() {
        Ok(status) => exit_code_from_status(status),
        Err(e) => {
            tracing::warn!(error = %e, "wait child failed");
            None
        }
    }
}

fn exit_code_from_status(status: ExitStatus) -> Option<i32> {
    if status.success() {
        Some(0)
    } else {
        Some(-1)
    }
}
