//! Userspace collector for eBPF span events
//!
//! Reads from the SPAN_EVENTS perf buffer and exports to Prometheus metrics.

use aya::maps::{HashMap, AsyncPerfEventArray};
use aya::programs::TracePoint;
use aya::{include_bytes_aligned, Bpf, Btf};
use aya::util::online_cpus;
use bytes::BytesMut;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::ebpf::metrics::{
    EBPF_TRACE_SPANS, EBPF_TRACE_DURATION, EBPF_TRACED_PROCESSES,
};

/// Trace ID length (16 bytes = 128-bit)
const TRACE_ID_LEN: usize = 16;

/// Event types matching the eBPF program
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanEventType {
    ProcessExec = 1,
    ProcessExit = 2,
    SyscallSample = 3,
}

impl SpanEventType {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(SpanEventType::ProcessExec),
            2 => Some(SpanEventType::ProcessExit),
            3 => Some(SpanEventType::SyscallSample),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            SpanEventType::ProcessExec => "exec",
            SpanEventType::ProcessExit => "exit",
            SpanEventType::SyscallSample => "syscall",
        }
    }
}

/// Span event from eBPF (must match kernel struct layout)
#[repr(C)]
#[derive(Debug, Clone)]
pub struct SpanEvent {
    pub trace_id: [u8; TRACE_ID_LEN],
    pub pid: u32,
    pub ppid: u32,
    pub event_type: u8,
    pub _pad: [u8; 3],
    pub timestamp_ns: u64,
    pub duration_ns: u64,
    pub syscall_nr: u32,
    pub exit_code: i32,
    pub comm: [u8; 16],
}

impl SpanEvent {
    /// Get trace ID as hex string
    pub fn trace_id_hex(&self) -> String {
        hex::encode(&self.trace_id)
    }

    /// Check if this event has a valid trace ID (not all zeros)
    pub fn has_trace_id(&self) -> bool {
        self.trace_id.iter().any(|&b| b != 0)
    }

    /// Get process name as string
    pub fn comm_str(&self) -> String {
        let end = self.comm.iter().position(|&b| b == 0).unwrap_or(16);
        String::from_utf8_lossy(&self.comm[..end]).to_string()
    }

    /// Get event type
    pub fn event_type(&self) -> Option<SpanEventType> {
        SpanEventType::from_u8(self.event_type)
    }

    /// Duration in milliseconds
    pub fn duration_ms(&self) -> f64 {
        self.duration_ns as f64 / 1_000_000.0
    }
}

/// Error type for tracing operations
#[derive(Debug)]
pub enum TraceError {
    LoadError(String),
    AttachError(String),
    MapError(String),
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceError::LoadError(msg) => write!(f, "Trace load error: {}", msg),
            TraceError::AttachError(msg) => write!(f, "Trace attach error: {}", msg),
            TraceError::MapError(msg) => write!(f, "Trace map error: {}", msg),
        }
    }
}

impl std::error::Error for TraceError {}

/// Manages eBPF-based distributed tracing
pub struct TraceManager {
    bpf: Arc<RwLock<Bpf>>,
    collector_handle: Option<JoinHandle<()>>,
}

impl TraceManager {
    /// Create a new TraceManager, loading the span-trace eBPF program
    pub fn new() -> Option<Self> {
        match Self::try_new() {
            Ok(manager) => {
                println!("[TRACE] Distributed tracing enabled");
                Some(manager)
            }
            Err(e) => {
                println!("[TRACE] Failed to initialize: {} (continuing without tracing)", e);
                None
            }
        }
    }

    fn try_new() -> Result<Self, TraceError> {
        // Load BTF for CO-RE
        let btf = Btf::from_sys_fs()
            .map_err(|e| TraceError::LoadError(format!("BTF: {}", e)))?;

        // Load the span-trace eBPF program
        let bpf_bytes = include_bytes_aligned!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/ebpf/span-trace.o"
        ));

        let mut bpf = Bpf::load(bpf_bytes)
            .map_err(|e| TraceError::LoadError(e.to_string()))?;

        // Attach to sched_process_exec tracepoint
        let exec_prog: &mut TracePoint = bpf
            .program_mut("trace_exec")
            .ok_or_else(|| TraceError::LoadError("trace_exec not found".to_string()))?
            .try_into()
            .map_err(|e: aya::programs::ProgramError| TraceError::LoadError(e.to_string()))?;

        exec_prog.load()
            .map_err(|e| TraceError::LoadError(e.to_string()))?;
        exec_prog.attach("sched", "sched_process_exec")
            .map_err(|e| TraceError::AttachError(e.to_string()))?;

        // Attach to sched_process_exit tracepoint
        let exit_prog: &mut TracePoint = bpf
            .program_mut("trace_exit")
            .ok_or_else(|| TraceError::LoadError("trace_exit not found".to_string()))?
            .try_into()
            .map_err(|e: aya::programs::ProgramError| TraceError::LoadError(e.to_string()))?;

        exit_prog.load()
            .map_err(|e| TraceError::LoadError(e.to_string()))?;
        exit_prog.attach("sched", "sched_process_exit")
            .map_err(|e| TraceError::AttachError(e.to_string()))?;

        println!("[TRACE] Attached to sched_process_exec and sched_process_exit");

        Ok(Self {
            bpf: Arc::new(RwLock::new(bpf)),
            collector_handle: None,
        })
    }

    /// Start trace collection for a PID with the given trace ID
    pub async fn start_trace(&self, pid: u32, trace_id: &str) -> Result<(), TraceError> {
        let mut bpf = self.bpf.write().await;

        // Convert trace_id to bytes (pad or truncate to 16 bytes)
        let mut trace_bytes = [0u8; TRACE_ID_LEN];
        let id_bytes = trace_id.as_bytes();
        let len = id_bytes.len().min(TRACE_ID_LEN);
        trace_bytes[..len].copy_from_slice(&id_bytes[..len]);

        // Insert into PID_TO_TRACE map
        let mut pid_map: HashMap<_, u32, [u8; TRACE_ID_LEN]> = bpf
            .map_mut("PID_TO_TRACE")
            .ok_or_else(|| TraceError::MapError("PID_TO_TRACE not found".to_string()))?
            .try_into()
            .map_err(|e: aya::maps::MapError| TraceError::MapError(e.to_string()))?;

        pid_map.insert(pid, trace_bytes, 0)
            .map_err(|e| TraceError::MapError(e.to_string()))?;

        // Update metrics
        EBPF_TRACED_PROCESSES.inc();

        println!("[TRACE] Started tracing PID {} with trace_id {}", pid, trace_id);
        Ok(())
    }

    /// Stop trace collection for a PID
    pub async fn stop_trace(&self, pid: u32) -> Result<(), TraceError> {
        let mut bpf = self.bpf.write().await;

        let mut pid_map: HashMap<_, u32, [u8; TRACE_ID_LEN]> = bpf
            .map_mut("PID_TO_TRACE")
            .ok_or_else(|| TraceError::MapError("PID_TO_TRACE not found".to_string()))?
            .try_into()
            .map_err(|e: aya::maps::MapError| TraceError::MapError(e.to_string()))?;

        let _ = pid_map.remove(&pid);

        // Update metrics
        EBPF_TRACED_PROCESSES.dec();

        println!("[TRACE] Stopped tracing PID {}", pid);
        Ok(())
    }

    /// Start the background collector task that reads from perf buffer
    pub async fn start_collector(&mut self) -> Result<(), TraceError> {
        let bpf = Arc::clone(&self.bpf);

        let handle = tokio::spawn(async move {
            if let Err(e) = run_collector(bpf).await {
                eprintln!("[TRACE] Collector error: {}", e);
            }
        });

        self.collector_handle = Some(handle);
        println!("[TRACE] Background collector started");
        Ok(())
    }
}

/// Background task that reads from the perf buffer and updates metrics
async fn run_collector(bpf: Arc<RwLock<Bpf>>) -> Result<(), TraceError> {
    let mut bpf = bpf.write().await;

    // Get the perf event array
    let mut perf_array: AsyncPerfEventArray<_> = bpf
        .take_map("SPAN_EVENTS")
        .ok_or_else(|| TraceError::MapError("SPAN_EVENTS not found".to_string()))?
        .try_into()
        .map_err(|e: aya::maps::MapError| TraceError::MapError(e.to_string()))?;

    // Open perf buffers for each CPU
    let cpus = online_cpus()
        .map_err(|e| TraceError::MapError(format!("Failed to get online CPUs: {}", e)))?;

    let mut handles = Vec::new();

    for cpu_id in cpus {
        let mut buf = perf_array
            .open(cpu_id, Some(256))
            .map_err(|e| TraceError::MapError(format!("Failed to open perf buffer: {}", e)))?;

        let handle = tokio::spawn(async move {
            let mut buffers = (0..10)
                .map(|_| BytesMut::with_capacity(std::mem::size_of::<SpanEvent>()))
                .collect::<Vec<_>>();

            loop {
                let events = match buf.read_events(&mut buffers).await {
                    Ok(events) => events,
                    Err(e) => {
                        eprintln!("[TRACE] Error reading events: {}", e);
                        continue;
                    }
                };

                for i in 0..events.read {
                    let buf = &buffers[i];
                    if buf.len() >= std::mem::size_of::<SpanEvent>() {
                        let event = unsafe {
                            std::ptr::read_unaligned(buf.as_ptr() as *const SpanEvent)
                        };
                        process_span_event(&event);
                    }
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all handles (they run forever)
    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

/// Process a single span event and update metrics
fn process_span_event(event: &SpanEvent) {
    let event_type = match event.event_type() {
        Some(t) => t,
        None => return,
    };

    let trace_id = if event.has_trace_id() {
        event.trace_id_hex()
    } else {
        "untraced".to_string()
    };

    let comm = event.comm_str();

    // Update span counter
    EBPF_TRACE_SPANS
        .with_label_values(&[event_type.as_str(), &comm])
        .inc();

    // For exit events, record duration
    if event_type == SpanEventType::ProcessExit && event.duration_ns > 0 {
        let duration_secs = event.duration_ns as f64 / 1_000_000_000.0;
        EBPF_TRACE_DURATION
            .with_label_values(&[&comm])
            .observe(duration_secs);

        if event.has_trace_id() {
            println!(
                "[TRACE] {} exited after {:.3}ms (trace: {})",
                comm,
                event.duration_ms(),
                &trace_id[..16.min(trace_id.len())]
            );
        }
    }
}
