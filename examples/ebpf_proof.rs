//! eBPF Observability Proof - Demonstrates kernel-level syscall tracing
//!
//! This example shows NeuroVisor's eBPF integration for syscall monitoring.
//! When enabled, eBPF tracepoints count syscalls per tracked process,
//! exporting metrics to Prometheus.
//!
//! # Requirements
//!
//! - Linux kernel 5.8+ with CONFIG_BPF_SYSCALL=y
//! - CAP_BPF and CAP_PERFMON capabilities (or root)
//! - Compile with: `cargo build --features ebpf`
//! - Run eBPF build first: `./build-ebpf.sh`
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Kernel Space                                               │
//! │                                                             │
//! │  tracepoint/syscalls/sys_enter                             │
//! │           │                                                 │
//! │           ▼                                                 │
//! │  ┌─────────────────┐    ┌──────────────────┐               │
//! │  │  eBPF Program   │───►│  SYSCALL_COUNTS  │               │
//! │  │ (syscall_trace) │    │    BPF Map       │               │
//! │  └─────────────────┘    └──────────────────┘               │
//! │           ▲                      │                          │
//! │           │                      │                          │
//! │  ┌─────────────────┐            │                          │
//! │  │   PID_TO_VM     │            │                          │
//! │  │    BPF Map      │            │                          │
//! │  └─────────────────┘            │                          │
//! └─────────────────────────────────┼──────────────────────────┘
//!                                   │
//!            User Space             │
//!                                   ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │  EbpfManager                                                │
//! │                                                             │
//! │  start_tracing(vm_id, pid) → insert PID_TO_VM              │
//! │  collect_metrics()         → read SYSCALL_COUNTS           │
//! │  stop_tracing(pid)         → remove PID_TO_VM              │
//! │                                                             │
//! │           │                                                 │
//! │           ▼                                                 │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │  Prometheus Metrics                                  │   │
//! │  │                                                      │   │
//! │  │  neurovisor_ebpf_syscall_total{vm_id, syscall}      │   │
//! │  │  neurovisor_ebpf_enabled                            │   │
//! │  │  neurovisor_ebpf_tracked_pids                       │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Run
//!
//! ```bash
//! # Without eBPF feature (shows graceful degradation)
//! cargo run --example ebpf_proof
//!
//! # With eBPF feature (requires root and eBPF build)
//! sudo ./build-ebpf.sh
//! sudo cargo run --example ebpf_proof --features ebpf
//! ```

use neurovisor::ebpf::{self, EbpfManager};

fn main() {
    println!("═══════════════════════════════════════════════════════════════");
    println!("  eBPF OBSERVABILITY PROOF");
    println!("  Kernel-level syscall tracing for NeuroVisor VMs");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // Check kernel capabilities
    println!("1. CHECKING KERNEL CAPABILITIES");
    println!("   ├── Checking for BPF support...");

    if std::path::Path::new("/sys/kernel/btf/vmlinux").exists() {
        println!("   │   ✅ BTF (BPF Type Format) available");
    } else {
        println!("   │   ⚠️  BTF not available (CO-RE may not work)");
    }

    if std::path::Path::new("/sys/kernel/security/lsm").exists() {
        if let Ok(lsm) = std::fs::read_to_string("/sys/kernel/security/lsm") {
            if lsm.contains("bpf") {
                println!("   │   ✅ BPF LSM enabled");
            } else {
                println!("   │   ⚠️  BPF LSM not in LSM list: {}", lsm.trim());
            }
        }
    }

    // Check capabilities
    println!("   ├── Checking capabilities...");
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        println!("   │   ✅ Running as root (CAP_BPF available)");
    } else {
        println!("   │   ⚠️  Not running as root (may need CAP_BPF, CAP_PERFMON)");
    }
    println!();

    // Try to initialize eBPF
    println!("2. INITIALIZING eBPF MANAGER");
    ebpf::metrics::init();

    let ebpf = EbpfManager::new();

    match &ebpf {
        Some(manager) => {
            println!("   ✅ eBPF manager initialized successfully!");
            println!();

            println!("3. eBPF STATUS");
            println!("   ├── Enabled: {}", manager.is_enabled());
            println!("   ├── Tracepoint: syscalls/sys_enter");
            println!("   └── Maps: PID_TO_VM, SYSCALL_COUNTS");
            println!();

            println!("4. PROMETHEUS METRICS AVAILABLE");
            println!("   ├── neurovisor_ebpf_syscall_total{{vm_id, syscall}}");
            println!("   ├── neurovisor_ebpf_process_exec_total{{vm_id, command}}");
            println!("   ├── neurovisor_ebpf_enabled");
            println!("   └── neurovisor_ebpf_tracked_pids");
            println!();

            println!("5. INTEGRATION POINTS");
            println!("   ├── VMManager.create_vm() → start_tracing(vm_id, pid)");
            println!("   ├── VMManager.destroy_vm() → stop_tracing(pid)");
            println!("   └── Metrics collection → collect_metrics()");
        }
        None => {
            println!("   ⚠️  eBPF manager not available (graceful degradation)");
            println!();

            #[cfg(not(feature = "ebpf"))]
            {
                println!("   REASON: eBPF feature not enabled at compile time");
                println!();
                println!("   To enable eBPF:");
                println!("   1. Build eBPF programs: ./build-ebpf.sh");
                println!("   2. Rebuild with feature: cargo build --features ebpf");
                println!("   3. Run as root: sudo cargo run --example ebpf_proof --features ebpf");
            }

            #[cfg(feature = "ebpf")]
            {
                println!("   REASON: eBPF feature enabled but loading failed");
                println!();
                println!("   Possible causes:");
                println!("   - eBPF programs not built (run ./build-ebpf.sh)");
                println!("   - Kernel doesn't support BPF");
                println!("   - Missing capabilities (try running as root)");
                println!("   - Verifier rejected program");
            }

            println!();
            println!("   The system continues without eBPF tracing.");
            println!("   VM isolation, seccomp, and cgroups still protect workloads.");
        }
    }

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  DEFENSE-IN-DEPTH LAYERS");
    println!("═══════════════════════════════════════════════════════════════");
    println!();
    println!("  ┌─────────────────────────────────────────────────────────┐");
    println!("  │  Layer 1: CAPABILITIES                                  │");
    println!("  │  Drop dangerous root powers before starting Firecracker │");
    println!("  └─────────────────────────────────────────────────────────┘");
    println!("                           │");
    println!("                           ▼");
    println!("  ┌─────────────────────────────────────────────────────────┐");
    println!("  │  Layer 2: SECCOMP BPF                                   │");
    println!("  │  Block dangerous syscalls at kernel level               │");
    println!("  └─────────────────────────────────────────────────────────┘");
    println!("                           │");
    println!("                           ▼");
    println!("  ┌─────────────────────────────────────────────────────────┐");
    if ebpf.is_some() {
        println!("  │  Layer 3: eBPF TRACING ✅ ACTIVE                       │");
    } else {
        println!("  │  Layer 3: eBPF TRACING ⚠️  INACTIVE                     │");
    }
    println!("  │  Syscall counting and observability                     │");
    println!("  └─────────────────────────────────────────────────────────┘");
    println!("                           │");
    println!("                           ▼");
    println!("  ┌─────────────────────────────────────────────────────────┐");
    println!("  │  Layer 4: FIRECRACKER VM                                │");
    println!("  │  KVM hardware virtualization                            │");
    println!("  └─────────────────────────────────────────────────────────┘");
    println!("                           │");
    println!("                           ▼");
    println!("  ┌─────────────────────────────────────────────────────────┐");
    println!("  │  Layer 5: CGROUPS v2                                    │");
    println!("  │  CPU and memory resource limits                         │");
    println!("  └─────────────────────────────────────────────────────────┘");
    println!();
}
