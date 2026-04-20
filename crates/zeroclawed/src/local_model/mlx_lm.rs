//! mlx_lm.server process lifecycle management.
//!
//! Spawns `mlx_lm.server` as a child process and polls for readiness by
//! attempting a TCP connection to the server port. No extra HTTP crate needed.

use std::net::TcpStream;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

/// Handle to a running mlx_lm.server child process.
pub struct MlxLmHandle {
    child: Child,
    port: u16,
}

impl std::fmt::Debug for MlxLmHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlxLmHandle")
            .field("pid", &self.child.id())
            .field("port", &self.port)
            .finish()
    }
}

impl MlxLmHandle {
    /// Kill any existing process listening on `port` before starting a new one.
    /// Uses `lsof -ti :{port}` to find PIDs, then sends SIGTERM + waits for port to close.
    fn kill_existing_on_port(port: u16) {
        let output = Command::new("lsof")
            .args(["-ti", &format!(":{port}")])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let pids_str = String::from_utf8_lossy(&out.stdout);
                for pid_str in pids_str.split_whitespace() {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        info!(pid = pid, port = port, "Killing existing process on port");
                        let _ = Command::new("kill").args(["-TERM", pid_str]).output();
                    }
                }
                // Wait up to 15s for port to close.
                let deadline = Instant::now() + Duration::from_secs(15);
                let addr = format!("127.0.0.1:{port}");
                while Instant::now() < deadline {
                    if TcpStream::connect_timeout(
                        &addr.parse().expect("valid addr"),
                        Duration::from_secs(1),
                    )
                    .is_err()
                    {
                        debug!(port = port, "Port is now free");
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                warn!(port = port, "Port may still be in use after killing previous process");
            }
            Ok(_) => {
                debug!(port = port, "No existing process on port");
            }
            Err(e) => {
                warn!(error = %e, "lsof check failed — proceeding anyway");
            }
        }
    }

    /// Spawn mlx_lm.server for the given HuggingFace model and wait until ready.
    pub fn start(
        hf_model_id: &str,
        host: &str,
        port: u16,
        extra_args: &[String],
        startup_timeout: Duration,
    ) -> Result<Self> {
        info!(
            model = %hf_model_id,
            host = %host,
            port = %port,
            "Starting mlx_lm.server"
        );

        // Kill any existing server on this port before spawning the new one.
        // This handles the case where a previous mlx_lm.server was started externally.
        Self::kill_existing_on_port(port);

        let child = Command::new("mlx_lm.server")
            .args(["--model", hf_model_id, "--host", host, "--port", &port.to_string()])
            .args(extra_args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("spawning mlx_lm.server — is mlx_lm installed and on PATH?")?;

        let handle = MlxLmHandle { child, port };

        // Poll until the port accepts connections (server is ready).
        handle.wait_for_ready(host, startup_timeout)?;

        Ok(handle)
    }

    /// Polls for TCP connectivity on the server port until timeout.
    fn wait_for_ready(&self, host: &str, timeout: Duration) -> Result<()> {
        let addr = format!("{}:{}", host, self.port);
        let deadline = Instant::now() + timeout;

        info!(
            addr = %addr,
            timeout_s = timeout.as_secs(),
            "Waiting for mlx_lm.server to accept connections"
        );

        loop {
            if Instant::now() >= deadline {
                bail!(
                    "mlx_lm.server did not become ready within {}s (port {})",
                    timeout.as_secs(),
                    self.port
                );
            }

            match TcpStream::connect_timeout(
                &addr.parse().expect("valid addr"),
                Duration::from_secs(2),
            ) {
                Ok(_) => {
                    // Port is open — give the HTTP server a moment to finish init.
                    std::thread::sleep(Duration::from_millis(500));
                    info!("mlx_lm.server is ready on port {}", self.port);
                    return Ok(());
                }
                Err(e) => {
                    debug!(error = %e, "mlx_lm.server not yet ready");
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }

    /// Stop the server. Uses SIGTERM via shell on Unix, kill() elsewhere.
    pub fn stop(mut self) {
        let pid = self.child.id();
        info!(pid = pid, "Stopping mlx_lm.server");

        // Send SIGTERM via `kill` shell command (available on macOS/Linux).
        #[cfg(unix)]
        {
            let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).output();
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }

        // Wait up to 10 seconds for graceful exit, then force kill.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    debug!(pid = pid, exit = ?status, "mlx_lm.server exited");
                    return;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        warn!(pid = pid, "Sending SIGKILL to mlx_lm.server after timeout");
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => {
                    warn!(pid = pid, error = %e, "Error waiting for mlx_lm.server exit");
                    return;
                }
            }
        }
    }
}
