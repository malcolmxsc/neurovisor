//! VM lifecycle management
//!
//! Functions for spawning Firecracker processes and waiting for API readiness.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::{thread, time};
use tokio::time::Duration;

use crate::security::FirecrackerSeccomp;

/// Spawn a Firecracker process with the given API socket path
///
/// The spawned process will have a seccomp filter applied that restricts
/// syscalls to only those required by Firecracker. This is applied via
/// pre_exec, so it only affects the child process (not the orchestrator).
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

    // SAFETY: pre_exec runs after fork() but before exec() in the child process.
    // We only call async-signal-safe operations (seccomp filter application).
    let child = unsafe {
        Command::new(firecracker_bin)
            .arg("--api-sock")
            .arg(api_socket)
            .stdin(stdin_mode)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .pre_exec(|| {
                // Apply seccomp filter in the child process before exec
                // This restricts Firecracker's syscalls without affecting the orchestrator
                let filter = FirecrackerSeccomp::with_firecracker_defaults();
                match filter.apply() {
                    Ok(()) => {
                        // Note: Can't easily print from pre_exec, filter is applied silently
                        Ok(())
                    }
                    Err(e) => {
                        // Convert io::Error to the expected type
                        Err(e)
                    }
                }
            })
            .spawn()?
    };

    println!("[INFO] ✅ SECCOMP FILTER APPLIED (Firecracker syscalls restricted)");

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
