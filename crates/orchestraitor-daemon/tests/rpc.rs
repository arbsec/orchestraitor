//! Integration tests for the `orcd` Unix-domain JSON-RPC server.

use std::{
    process::Command,
    time::{Duration, Instant},
};

use orchestraitor_daemon::{DaemonConfig, serve_until};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::watch,
    time::timeout,
};

#[tokio::test(flavor = "current_thread")]
async fn initialize_responds_when_daemon_serves_test_socket()
-> Result<(), Box<dyn std::error::Error>> {
    // Given: a daemon serving a test-scoped Unix-domain socket.
    let temp = TempDir::new()?;
    let socket = temp.path().join("orcd.sock");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let config = DaemonConfig::new(socket.clone());
    let server = tokio::spawn(async move {
        serve_until(config, async move {
            let mut shutdown_rx = shutdown_rx;
            let _changed = shutdown_rx.changed().await;
            Ok(())
        })
        .await
    });

    wait_for_socket(&socket).await?;

    // When: a JSON-RPC initialize request is sent over that socket.
    let response = rpc_call(
        &socket,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .await?;
    shutdown_tx.send(true)?;
    server.await??;

    // Then: the daemon returns its protocol identity.
    assert_eq!(response["result"]["server_name"], "orcd");
    assert_eq!(response["result"]["protocol_version"], 1);
    Ok(())
}

#[test]
fn sigterm_stops_orcd_within_five_seconds() -> Result<(), Box<dyn std::error::Error>> {
    // Given: an `orcd` process started with a test-scoped Unix-domain socket.
    let temp = TempDir::new()?;
    let socket = temp.path().join("orcd.sock");
    let binary = std::env::var("CARGO_BIN_EXE_orcd")?;
    let mut child = Command::new(binary).arg(&socket).spawn()?;
    wait_for_socket_blocking(&socket)?;

    // When: SIGTERM is delivered to the daemon process.
    let started = Instant::now();
    let kill_status = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()?;
    assert!(kill_status.success());

    // Then: the process exits inside the 5s graceful-shutdown budget.
    while started.elapsed() < Duration::from_secs(5) {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let _ignored = child.kill();
    Err("orcd did not exit within five seconds after SIGTERM".into())
}

async fn rpc_call(
    socket: &std::path::Path,
    request: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(socket).await?;
    let body = serde_json::to_vec(&request)?;
    let headers = format!(
        "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&body).await?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let marker = b"\r\n\r\n";
    let body_start = response
        .windows(marker.len())
        .position(|window| window == marker)
        .map(|index| index + marker.len())
        .ok_or("missing HTTP response body")?;
    Ok(serde_json::from_slice(&response[body_start..])?)
}

async fn wait_for_socket(socket: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    timeout(Duration::from_secs(5), async {
        loop {
            if UnixStream::connect(socket).await.is_ok() {
                return Ok::<(), Box<dyn std::error::Error>>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

fn wait_for_socket_blocking(socket: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if socket.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("orcd socket did not appear within five seconds".into())
}
