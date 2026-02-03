//! VM lifecycle management
//!
//! Functions for spawning Firecracker processes and waiting for API readiness.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::{thread, time};
use tokio::time::Duration;

/// Spawn a Firecracker process with the given API socket path
///
/// Note: Firecracker has its own built-in seccomp filter that it applies
/// internally. We don't need to apply an external seccomp filter here.
/// See: https://github.com/firecracker-microvm/firecracker/blob/main/docs/seccomp.md
///
/// # Arguments
/// * `api_socket` - Path to the Unix socket for Firecracker's API
/// * `stdin_mode` - How to handle stdin (Inherit, Piped, or Null)
///
/// # Returns
/// The spawned child process
pub fn spawn_firecracker(
    api_socket: &str,
    stdin_mode: Stdio,
) -> Result<Child, Box<dyn std::error::Error>> {
    let firecracker_bin = "./firecracker";

    let child = Command::new(firecracker_bin)
        .arg("--api-sock")
        .arg(api_socket)
        .stdin(stdin_mode)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    println!("[INFO] ✅ FIRECRACKER SPAWNED (using built-in seccomp filter)");

    Ok(child)
}

/// Wait for the Firecracker API socket to become available
///
/// # Arguments
/// * `socket_path` - Path to the API socket
/// * `timeout` - Optional timeout duration. If None, waits indefinitely.
///
/// # Returns
/// Ok(()) if socket is ready, Err if timeout expires
pub fn wait_for_api_socket(
    socket_path: &str,
    timeout: Option<Duration>,
) -> Result<(), Box<dyn std::error::Error>> {
    let start_time = time::Instant::now();
    let poll_interval = time::Duration::from_millis(100);

    loop {
        if Path::new(socket_path).exists() {
            return Ok(());
        }

        // Check timeout if specified
        if let Some(max_duration) = timeout {
            if start_time.elapsed() > max_duration {
                return Err(format!(
                    "Firecracker API socket not ready after {:?}",
                    max_duration
                )
                .into());
            }
        }

        thread::sleep(poll_interval);
    }
}

/// Helper to convert relative paths to absolute paths
///
/// This is needed because Firecracker API expects absolute paths for snapshots.
pub fn to_absolute_path(relative_path: &str) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let abs_path = cwd.join(relative_path);
    Ok(abs_path.to_str().unwrap().to_string())
}
