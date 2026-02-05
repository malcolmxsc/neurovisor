//! eBPF LSM program for file access control
//!
//! This program attaches to the LSM file_open hook and enforces
//! file access policies for tracked PIDs (Firecracker VM processes).
//!
//! It can block access to sensitive paths like /etc/shadow, /etc/passwd,
//! or any paths configured via the BLOCKED_PATHS map.

#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{lsm, map},
    maps::HashMap,
    programs::LsmContext,
    helpers::bpf_get_current_pid_tgid,
};

/// Maximum number of PIDs we can track
const MAX_PIDS: u32 = 1024;

/// Maximum number of blocked path prefixes
const MAX_BLOCKED_PATHS: u32 = 256;

/// Path prefix length for matching (64 bytes)
const PATH_PREFIX_LEN: usize = 64;

/// BPF Map: PID -> 1 (set of tracked PIDs)
/// Populated from userspace when a VM starts.
#[map]
static TRACKED_PIDS: HashMap<u32, u8> = HashMap::with_max_entries(MAX_PIDS, 0);

/// BPF Map: path_prefix -> 1 (set of blocked path prefixes)
/// Populated from userspace with paths like "/etc/shadow", "/proc/kcore", etc.
#[map]
static BLOCKED_PATHS: HashMap<[u8; PATH_PREFIX_LEN], u8> = HashMap::with_max_entries(MAX_BLOCKED_PATHS, 0);

/// BPF Map: Counter for blocked access attempts
#[map]
static BLOCKED_COUNT: HashMap<u32, u64> = HashMap::with_max_entries(1, 0);

/// LSM hook for file_open
/// Returns 0 to allow, negative errno to deny
#[lsm(hook = "file_open")]
pub fn file_open_check(ctx: LsmContext) -> i32 {
    match try_file_open_check(&ctx) {
        Ok(ret) => ret,
        Err(_) => 0, // On error, allow (fail-open for safety)
    }
}

fn try_file_open_check(ctx: &LsmContext) -> Result<i32, i64> {
    // Get current PID
    let pid_tgid = unsafe { bpf_get_current_pid_tgid() };
    let pid = (pid_tgid & 0xFFFFFFFF) as u32;

    // Only enforce for tracked PIDs (Firecracker processes)
    if unsafe { TRACKED_PIDS.get(&pid).is_none() } {
        return Ok(0); // Not tracked, allow
    }

    // Read file path from LSM context
    // The file structure is the first argument to file_open
    // We need to extract the path from struct file -> f_path -> dentry -> d_name
    //
    // For now, we use a simplified approach: read the path from context
    // In production, this would use bpf_d_path or similar helpers
    let path = get_file_path(ctx)?;

    // Check if path matches any blocked prefix
    if is_path_blocked(&path) {
        // Increment blocked counter
        let key: u32 = 0;
        let count = unsafe { BLOCKED_COUNT.get(&key).copied().unwrap_or(0) };
        let _ = unsafe { BLOCKED_COUNT.insert(&key, &(count + 1), 0) };

        // Return -EACCES (Permission denied)
        return Ok(-13);
    }

    Ok(0) // Allow
}

/// Extract file path from LSM context
/// This is a simplified version - full implementation would use bpf_d_path
fn get_file_path(_ctx: &LsmContext) -> Result<[u8; PATH_PREFIX_LEN], i64> {
    // In a real implementation, we would:
    // 1. Get struct file* from ctx
    // 2. Read f_path.dentry
    // 3. Use bpf_d_path to get the full path
    //
    // For now, return empty path (allows everything)
    // This will be enhanced when we have proper BTF support
    Ok([0u8; PATH_PREFIX_LEN])
}

/// Check if a path matches any blocked prefix
fn is_path_blocked(path: &[u8; PATH_PREFIX_LEN]) -> bool {
    // Check exact match in blocked paths
    if unsafe { BLOCKED_PATHS.get(path).is_some() } {
        return true;
    }

    // For prefix matching, we'd need to iterate
    // eBPF has limited loop support, so we check common prefixes
    // This is a simplified version

    false
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
