use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader, ReadBuf};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::instance::InstancePool;
use switchyard_provider_api::LiveInstanceRegistry;

pub enum IpcStream {
    #[cfg(windows)]
    Windows(tokio::net::windows::named_pipe::NamedPipeServer),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

impl AsyncRead for IpcStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(windows)]
            IpcStream::Windows(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for IpcStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        match self.get_mut() {
            #[cfg(windows)]
            IpcStream::Windows(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        match self.get_mut() {
            #[cfg(windows)]
            IpcStream::Windows(s) => Pin::new(s).poll_flush(cx),
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        match self.get_mut() {
            #[cfg(windows)]
            IpcStream::Windows(s) => Pin::new(s).poll_shutdown(cx),
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub action: String,
    pub provider: String,
    pub task: String,
    pub session_id: Option<String>,
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub status: String,
    pub event: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[cfg(windows)]
pub async fn run_ipc_server(pool: Arc<InstancePool>, pipe_name: &str, cancel: CancellationToken) -> Result<(), std::io::Error> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut first = true;
    loop {
        let server = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(pipe_name)?;
        first = false;

        tokio::select! {
            _ = cancel.cancelled() => break,
            conn = server.connect() => {
                match conn {
                    Ok(_) => {
                        let stream = IpcStream::Windows(server);
                        let pool_clone = Arc::clone(&pool);
                        tokio::spawn(async move {
                            if let Err(e) = handle_ipc_client(stream, pool_clone).await {
                                eprintln!("IPC client error: {:?}", e);
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Failed to connect named pipe: {:?}", e);
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
pub async fn run_ipc_server(pool: Arc<InstancePool>, socket_path: &str, cancel: CancellationToken) -> Result<(), std::io::Error> {
    use tokio::net::UnixListener;
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            conn = listener.accept() => {
                match conn {
                    Ok((socket, _)) => {
                        let stream = IpcStream::Unix(socket);
                        let pool_clone = Arc::clone(&pool);
                        tokio::spawn(async move {
                            if let Err(e) = handle_ipc_client(stream, pool_clone).await {
                                eprintln!("IPC client error: {:?}", e);
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Failed to accept unix connection: {:?}", e);
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }
    Ok(())
}

async fn handle_ipc_client(stream: IpcStream, pool: Arc<InstancePool>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::AsyncWriteExt;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        let req: IpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let err_resp = IpcResponse {
                    status: "failed".to_string(),
                    event: None,
                    result: None,
                    error: Some(format!("Invalid JSON request: {e}")),
                };
                let mut resp_str = serde_json::to_string(&err_resp)?;
                resp_str.push('\n');
                writer.write_all(resp_str.as_bytes()).await?;
                continue;
            }
        };

        if req.action == "delegate" {
            let provider = req.provider.clone();
            let task = req.task.clone();
            let turn_id = Uuid::now_v7();

            let inst_lock = match pool.checkout_instance(&provider) {
                Some(inst) => inst,
                None => {
                    let err_resp = IpcResponse {
                        status: "failed".to_string(),
                        event: None,
                        result: None,
                        error: Some(format!("Provider {provider} not running in pool")),
                    };
                    let mut resp_str = serde_json::to_string(&err_resp)?;
                    resp_str.push('\n');
                    writer.write_all(resp_str.as_bytes()).await?;
                    continue;
                }
            };

            let inst_clone = Arc::clone(&inst_lock);
            let runner_task = tokio::spawn(async move {
                let mut inst = inst_clone.lock().await;
                inst.send_message(&task).await
            });

            // Write start response
            let start_resp = IpcResponse {
                status: "running".to_string(),
                event: Some(serde_json::json!({ "event_type": "turn.started", "turn_id": turn_id })),
                result: None,
                error: None,
            };
            let mut resp_str = serde_json::to_string(&start_resp)?;
            resp_str.push('\n');
            writer.write_all(resp_str.as_bytes()).await?;

            let mut inner_rx = match runner_task.await? {
                Ok(rx) => rx,
                Err(e) => {
                    pool.release_instance(&provider, inst_lock);
                    let fail_resp = IpcResponse {
                        status: "failed".to_string(),
                        event: None,
                        result: None,
                        error: Some(e.to_string()),
                    };
                    let mut resp_str = serde_json::to_string(&fail_resp)?;
                    resp_str.push('\n');
                    writer.write_all(resp_str.as_bytes()).await?;
                    continue;
                }
            };

            let mut response_text = String::new();
            while let Some(event) = inner_rx.recv().await {
                if let Some(text) = event.payload.get("text").and_then(|t| t.as_str()) {
                    response_text.push_str(text);
                } else if let Some(result_text) = event.payload.get("result").and_then(|r| r.as_str()) {
                    response_text.push_str(result_text);
                }

                let event_resp = IpcResponse {
                    status: "running".to_string(),
                    event: Some(serde_json::to_value(&event)?),
                    result: None,
                    error: None,
                };
                let mut resp_str = serde_json::to_string(&event_resp)?;
                resp_str.push('\n');
                writer.write_all(resp_str.as_bytes()).await?;
            }

            pool.release_instance(&provider, inst_lock);

            let success_resp = IpcResponse {
                status: "success".to_string(),
                event: None,
                result: Some(serde_json::json!({ "response_text": response_text })),
                error: None,
            };
            let mut resp_str = serde_json::to_string(&success_resp)?;
            resp_str.push('\n');
            writer.write_all(resp_str.as_bytes()).await?;
        } else {
            let err_resp = IpcResponse {
                status: "failed".to_string(),
                event: None,
                result: None,
                error: Some(format!("Unknown action: {}", req.action)),
            };
            let mut resp_str = serde_json::to_string(&err_resp)?;
            resp_str.push('\n');
            writer.write_all(resp_str.as_bytes()).await?;
        }
    }

    Ok(())
}
