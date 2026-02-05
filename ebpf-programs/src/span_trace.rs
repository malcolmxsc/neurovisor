//! eBPF program for distributed tracing span collection
//!
//! This program captures lifecycle events for traced processes and emits
//! them via perf buffer for userspace collection. Events include:
//! - Process exec (VM start)
//! - Process exit (VM stop)
//! - Syscall latency samples
//!
//! Trace IDs are correlated via the PID_TO_TRACE map populated by userspace.

#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{map, tracepoint},
    maps::{HashMap, PerfEventArray},
    programs::TracePointContext,
    helpers::bpf_get_current_pid_tgid,
    cty::c_long,
};

/// Maximum number of PIDs we can track
const MAX_PIDS: u32 = 1024;

/// Trace ID length (16 bytes = 128-bit UUID)
const TRACE_ID_LEN: usize = 16;

/// Event types for span tracing
#[repr(u8)]
pub enum SpanEventType {
    ProcessExec = 1,
    ProcessExit = 2,
    SyscallSample = 3,
}

/// Span event sent to userspace via perf buffer
#[repr(C)]
pub struct SpanEvent {
    /// Trace ID (from PID_TO_TRACE map, or zeros if not tracked)
    pub trace_id: [u8; TRACE_ID_LEN],
    /// Process ID
    pub pid: u32,
    /// Parent process ID
    pub ppid: u32,
    /// Event type
    pub event_type: u8,
    /// Padding for alignment
    pub _pad: [u8; 3],
    /// Timestamp (nanoseconds since boot)
    pub timestamp_ns: u64,
    /// Duration (for exit events, time since exec)
    pub duration_ns: u64,
    /// Syscall number (for syscall events)
    pub syscall_nr: u32,
    /// Exit code (for exit events)
    pub exit_code: i32,
    /// Process name (comm)
    pub comm: [u8; 16],
}

/// BPF Map: PID -> trace_id
/// Populated from userspace when a VM starts with a trace context.
#[map]
static PID_TO_TRACE: HashMap<u32, [u8; TRACE_ID_LEN]> = HashMap::with_max_entries(MAX_PIDS, 0);

/// BPF Map: PID -> start timestamp
/// Used to calculate duration for exit events.
#[map]
static PID_START_TIME: HashMap<u32, u64> = HashMap::with_max_entries(MAX_PIDS, 0);

/// Perf event array for sending span events to userspace
#[map]
static SPAN_EVENTS: PerfEventArray<SpanEvent> = PerfEventArray::new(0);

/// Tracepoint for sched:sched_process_exec
/// Fires when a process calls execve()
#[tracepoint]
pub fn trace_exec(ctx: TracePointContext) -> u32 {
    match try_trace_exec(&ctx) {
        Ok(ret) => ret,
        Err(_) => 0,
    }
}

fn try_trace_exec(ctx: &TracePointContext) -> Result<u32, c_long> {
    let pid_tgid = bpf_get_current_pid_tgid();
    let pid = (pid_tgid >> 32) as u32;
    let tgid = (pid_tgid & 0xFFFFFFFF) as u32;

    // Check if this PID or its parent is being traced
    let trace_id = get_trace_id(pid, tgid);

    // Record start time for duration calculation
    let timestamp = unsafe { aya_ebpf::helpers::bpf_ktime_get_ns() };
    let _ = PID_START_TIME.insert(&pid, &timestamp, 0);

    // Get process name
    let comm = aya_ebpf::helpers::bpf_get_current_comm().unwrap_or([0u8; 16]);

    // Create span event
    let event = SpanEvent {
        trace_id,
        pid,
        ppid: tgid,
        event_type: SpanEventType::ProcessExec as u8,
        _pad: [0; 3],
        timestamp_ns: timestamp,
        duration_ns: 0,
        syscall_nr: 0,
        exit_code: 0,
        comm,
    };

    // Send to userspace
    SPAN_EVENTS.output(ctx, &event, 0);

    Ok(0)
}

/// Tracepoint for sched:sched_process_exit
/// Fires when a process exits
#[tracepoint]
pub fn trace_exit(ctx: TracePointContext) -> u32 {
    match try_trace_exit(&ctx) {
        Ok(ret) => ret,
        Err(_) => 0,
    }
}

fn try_trace_exit(ctx: &TracePointContext) -> Result<u32, c_long> {
    let pid_tgid = bpf_get_current_pid_tgid();
    let pid = (pid_tgid >> 32) as u32;
    let tgid = (pid_tgid & 0xFFFFFFFF) as u32;

    // Check if this PID is being traced
    let trace_id = get_trace_id(pid, tgid);

    // Get timestamps
    let end_time = unsafe { aya_ebpf::helpers::bpf_ktime_get_ns() };
    let start_time = unsafe { PID_START_TIME.get(&pid).copied().unwrap_or(end_time) };
    let duration = end_time.saturating_sub(start_time);

    // Get process name
    let comm = aya_ebpf::helpers::bpf_get_current_comm().unwrap_or([0u8; 16]);

    // Create span event
    let event = SpanEvent {
        trace_id,
        pid,
        ppid: tgid,
        event_type: SpanEventType::ProcessExit as u8,
        _pad: [0; 3],
        timestamp_ns: end_time,
        duration_ns: duration,
        syscall_nr: 0,
        exit_code: 0, // TODO: extract from context if needed
        comm,
    };

    // Send to userspace
    SPAN_EVENTS.output(ctx, &event, 0);

    // Clean up maps
    let _ = PID_START_TIME.remove(&pid);

    Ok(0)
}

/// Get trace ID for a PID, checking parent if not found
#[inline(always)]
fn get_trace_id(pid: u32, ppid: u32) -> [u8; TRACE_ID_LEN] {
    // First check the PID itself
    if let Some(trace_id) = unsafe { PID_TO_TRACE.get(&pid) } {
        return *trace_id;
    }

    // Check parent PID (for child processes of tracked VMs)
    if let Some(trace_id) = unsafe { PID_TO_TRACE.get(&ppid) } {
        return *trace_id;
    }

    // Not tracked, return zeros
    [0u8; TRACE_ID_LEN]
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
