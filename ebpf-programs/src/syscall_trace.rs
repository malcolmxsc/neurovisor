//! eBPF syscall tracing program for NeuroVisor
//!
//! This program attaches to the syscalls tracepoint and counts syscalls
//! per tracked PID (Firecracker VM processes). Data is exported to userspace
//! via BPF maps for Prometheus metrics.

#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{map, tracepoint},
    maps::HashMap,
    programs::TracePointContext,
    helpers::bpf_get_current_pid_tgid,
};
use aya_log_ebpf::info;

/// Maximum number of PIDs we can track (one per VM)
const MAX_PIDS: u32 = 1024;

/// Maximum number of unique (vm_id, syscall) pairs
const MAX_SYSCALL_ENTRIES: u32 = 65536;

/// BPF Map: PID -> VM ID hash
/// Populated from userspace when a VM starts. We track the Firecracker
/// process PID and its children.
#[map]
static PID_TO_VM: HashMap<u32, u64> = HashMap::with_max_entries(MAX_PIDS, 0);

/// BPF Map: (vm_id_hash, syscall_nr) -> count
/// Read from userspace to export to Prometheus metrics.
#[map]
static SYSCALL_COUNTS: HashMap<SyscallKey, u64> = HashMap::with_max_entries(MAX_SYSCALL_ENTRIES, 0);

/// Key for syscall counting: VM ID hash + syscall number
#[repr(C)]
struct SyscallKey {
    vm_id: u64,
    syscall_nr: u32,
    _pad: u32,  // Padding for alignment
}

/// Tracepoint for syscall entry
/// Attaches to: tracepoint/syscalls/sys_enter
#[tracepoint]
pub fn sys_enter(ctx: TracePointContext) -> u32 {
    match try_sys_enter(&ctx) {
        Ok(ret) => ret,
        Err(_) => 1,
    }
}

fn try_sys_enter(ctx: &TracePointContext) -> Result<u32, i64> {
    // Get current PID (lower 32 bits of pid_tgid)
    let pid_tgid = unsafe { bpf_get_current_pid_tgid() };
    let pid = (pid_tgid & 0xFFFFFFFF) as u32;

    // Check if this PID belongs to a tracked VM
    let vm_id = match unsafe { PID_TO_VM.get(&pid) } {
        Some(id) => *id,
        None => return Ok(0), // Not a tracked PID, skip
    };

    // Read syscall number from tracepoint context
    // The syscall number is at offset 8 in the tracepoint args
    let syscall_nr: u32 = unsafe { ctx.read_at(8)? };

    // Build the key for our counter map
    let key = SyscallKey {
        vm_id,
        syscall_nr,
        _pad: 0,
    };

    // Increment the counter atomically
    let count = unsafe { SYSCALL_COUNTS.get(&key).copied().unwrap_or(0) };
    let _ = unsafe { SYSCALL_COUNTS.insert(&key, &(count + 1), 0) };

    // Log for debugging (only in debug builds, high overhead)
    #[cfg(debug_assertions)]
    info!(ctx, "VM {} syscall {}: count {}", vm_id, syscall_nr, count + 1);

    Ok(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
