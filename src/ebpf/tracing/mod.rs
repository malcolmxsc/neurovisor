//! eBPF-based distributed tracing for NeuroVisor
//!
//! This module provides kernel-level span collection for distributed tracing.
//! It correlates VM lifecycle events with trace IDs for request tracking.
//!
//! ## Architecture
//!
//! ```text
//! Userspace                          Kernel (eBPF)
//! ─────────────────────────────────────────────────────
//!
//! TraceManager                      span-trace.o
//!    │                                   │
//!    ├─► start_trace(vm_id, trace_id)    │
//!    │   └─► PID_TO_TRACE.insert()  ────►│
//!    │                                   │
//!    │                              sched_process_exec
//!    │                              sched_process_exit
//!    │                                   │
//!    │◄── SPAN_EVENTS perf buffer ◄──────┘
//!    │
//!    └─► SpanEvent { trace_id, pid, timestamp, ... }
//!            │
//!            ▼
//!        Prometheus metrics
//! ```

#[cfg(feature = "ebpf")]
mod collector;

#[cfg(feature = "ebpf")]
pub use collector::{SpanEvent, SpanEventType, TraceManager, TraceError};

/// Stub TraceManager for when eBPF feature is disabled.
#[cfg(not(feature = "ebpf"))]
pub struct TraceManager;

#[cfg(not(feature = "ebpf"))]
impl TraceManager {
    pub fn new() -> Option<Self> {
        None
    }

    pub async fn start_trace(&self, _pid: u32, _trace_id: &str) -> Result<(), TraceError> {
        Ok(())
    }

    pub async fn stop_trace(&self, _pid: u32) -> Result<(), TraceError> {
        Ok(())
    }
}

#[cfg(not(feature = "ebpf"))]
#[derive(Debug)]
pub struct TraceError;

#[cfg(not(feature = "ebpf"))]
impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tracing not enabled")
    }
}

#[cfg(not(feature = "ebpf"))]
impl std::error::Error for TraceError {}
