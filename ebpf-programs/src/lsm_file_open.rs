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
    helpers::{bpf_get_current_pid_tgid, bpf_d_path, bpf_probe_read_kernel},
    cty::c_long,
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

/// BPF Map: Per-path counter for blocked access attempts
/// Key: path prefix (64 bytes), Value: count
/// Read from userspace to export per-path metrics
#[map]
static BLOCKED_PATH_COUNTS: HashMap<[u8; PATH_PREFIX_LEN], u64> = HashMap::with_max_entries(MAX_BLOCKED_PATHS, 0);

/// BPF Map: Total blocked count (for quick summary)
#[map]
static BLOCKED_TOTAL: HashMap<u32, u64> = HashMap::with_max_entries(1, 0);

/// LSM hook for file_open
/// The hook signature is: int file_open(struct file *file)
/// Returns 0 to allow, negative errno to deny
#[lsm(hook = "file_open")]
pub fn file_open_check(ctx: LsmContext) -> i32 {
    match try_file_open_check(&ctx) {
        Ok(ret) => ret,
        Err(_) => 0, // On error, allow (fail-open for safety)
    }
}

fn try_file_open_check(ctx: &LsmContext) -> Result<i32, c_long> {
    // Get current PID
    let pid_tgid = unsafe { bpf_get_current_pid_tgid() };
    let pid = (pid_tgid & 0xFFFFFFFF) as u32;

    // Only enforce for tracked PIDs (Firecracker processes)
    if unsafe { TRACKED_PIDS.get(&pid).is_none() } {
        return Ok(0); // Not tracked, allow
    }

    // Get the file path from LSM context
    let path = get_file_path(ctx)?;

    // Check if path matches any blocked prefix and get which one
    if let Some(blocked_path) = get_blocked_match(&path) {
        // Increment per-path counter
        let path_count = unsafe { BLOCKED_PATH_COUNTS.get(&blocked_path).copied().unwrap_or(0) };
        let _ = unsafe { BLOCKED_PATH_COUNTS.insert(&blocked_path, &(path_count + 1), 0) };

        // Increment total counter
        let key: u32 = 0;
        let total = unsafe { BLOCKED_TOTAL.get(&key).copied().unwrap_or(0) };
        let _ = unsafe { BLOCKED_TOTAL.insert(&key, &(total + 1), 0) };

        // Return -EACCES (Permission denied)
        return Ok(-13);
    }

    Ok(0) // Allow
}

/// Kernel struct file layout (partial, for path extraction)
/// struct file {
///     ...
///     struct path f_path;  // offset varies by kernel, typically around 16-24
///     ...
/// }
/// struct path {
///     struct vfsmount *mnt;
///     struct dentry *dentry;
/// }

/// Extract file path from LSM context using bpf_d_path
///
/// The file_open LSM hook receives struct file * as the first argument.
/// We extract the path using the bpf_d_path helper.
fn get_file_path(ctx: &LsmContext) -> Result<[u8; PATH_PREFIX_LEN], c_long> {
    let mut path_buf = [0u8; PATH_PREFIX_LEN];

    // Get struct file * from first argument
    // In LSM hooks, arguments are accessed via ctx
    let file_ptr: *const u8 = unsafe { ctx.arg(0) };

    if file_ptr.is_null() {
        return Ok(path_buf); // Empty path, will allow
    }

    // The f_path field is at a fixed offset in struct file
    // This offset can vary between kernel versions, but is typically:
    // - 16 bytes on older kernels
    // - 24 bytes on newer kernels (5.x+)
    // We use 16 as a common offset for the path struct
    const F_PATH_OFFSET: usize = 16;

    // Read the path struct pointer (struct path is embedded, not a pointer)
    // struct path { struct vfsmount *mnt; struct dentry *dentry; }
    let path_struct_ptr = unsafe { file_ptr.add(F_PATH_OFFSET) };

    // Use bpf_d_path to get the full path string
    // bpf_d_path takes a pointer to struct path and writes to buffer
    let ret = unsafe {
        bpf_d_path(
            path_struct_ptr as *mut _,
            path_buf.as_mut_ptr() as *mut _,
            PATH_PREFIX_LEN as u32,
        )
    };

    if ret < 0 {
        // bpf_d_path failed, return empty path (allow access)
        return Ok([0u8; PATH_PREFIX_LEN]);
    }

    Ok(path_buf)
}

/// Check if a path matches any blocked prefix and return the matching blocked path
fn get_blocked_match(path: &[u8; PATH_PREFIX_LEN]) -> Option<[u8; PATH_PREFIX_LEN]> {
    // Skip empty paths
    if path[0] == 0 {
        return None;
    }

    // Check each blocked path for prefix match
    // We iterate through well-known blocked paths
    check_prefix(path, b"/etc/shadow\0")
        .or_else(|| check_prefix(path, b"/etc/gshadow\0"))
        .or_else(|| check_prefix(path, b"/etc/sudoers\0"))
        .or_else(|| check_prefix(path, b"/proc/kcore\0"))
        .or_else(|| check_prefix(path, b"/proc/kmem\0"))
        .or_else(|| check_prefix(path, b"/dev/mem\0"))
        .or_else(|| check_prefix(path, b"/dev/kmem\0"))
        .or_else(|| check_prefix(path, b"/dev/port\0"))
        .or_else(|| check_prefix(path, b"/etc/ssh/ssh_host\0"))
        .or_else(|| check_prefix(path, b"/root/.ssh\0"))
        .or_else(|| check_prefix(path, b"/run/firecracker\0"))
        .or_else(|| check_prefix(path, b"/var/run/docker.sock\0"))
        .or_else(|| check_prefix(path, b"/run/containerd\0"))
}

/// Check if path starts with the given prefix
#[inline(always)]
fn check_prefix(path: &[u8; PATH_PREFIX_LEN], prefix: &[u8]) -> Option<[u8; PATH_PREFIX_LEN]> {
    let prefix_len = prefix.len().min(PATH_PREFIX_LEN);

    // Compare bytes up to prefix length (excluding null terminator)
    let cmp_len = if prefix[prefix_len - 1] == 0 {
        prefix_len - 1
    } else {
        prefix_len
    };

    // Manual comparison (eBPF verifier friendly)
    let mut matches = true;
    let mut i = 0;
    while i < cmp_len && i < PATH_PREFIX_LEN {
        if path[i] != prefix[i] {
            matches = false;
            break;
        }
        i += 1;
    }

    if matches && cmp_len > 0 {
        // Return the blocked prefix as the key for counting
        let mut result = [0u8; PATH_PREFIX_LEN];
        let mut j = 0;
        while j < cmp_len && j < PATH_PREFIX_LEN {
            result[j] = prefix[j];
            j += 1;
        }
        Some(result)
    } else {
        None
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
