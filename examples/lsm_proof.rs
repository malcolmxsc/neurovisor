//! eBPF LSM Security Proof - Demonstrates file access control
//!
//! This example shows NeuroVisor's eBPF LSM integration for blocking
//! access to sensitive files from Firecracker VM processes.
//!
//! # Requirements
//!
//! - Linux kernel 5.8+ with CONFIG_BPF_LSM=y
//! - BPF in LSM list (/sys/kernel/security/lsm must contain "bpf")
//! - CAP_BPF, CAP_PERFMON, CAP_MAC_ADMIN capabilities (or root)
//! - Compile with: `cargo build --features ebpf`
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Kernel Space                                               │
//! │                                                             │
//! │  security_file_open() ──► LSM BPF hook                     │
//! │                               │                             │
//! │                               ▼                             │
//! │                    ┌──────────────────┐                     │
//! │                    │ file_open_check  │                     │
//! │                    │   LSM program    │                     │
//! │                    └────────┬─────────┘                     │
//! │                             │                               │
//! │           ┌─────────────────┼─────────────────┐             │
//! │           ▼                 ▼                 ▼             │
//! │    ┌─────────────┐   ┌─────────────┐   ┌───────────┐       │
//! │    │TRACKED_PIDS │   │BLOCKED_PATHS│   │ Return    │       │
//! │    │   BPF Map   │   │   BPF Map   │   │ -EACCES   │       │
//! │    └─────────────┘   └─────────────┘   └───────────┘       │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Run
//!
//! ```bash
//! # Check LSM support
//! cat /sys/kernel/security/lsm
//! # Should include "bpf" in the list
//!
//! # Run proof (requires root)
//! sudo cargo run --example lsm_proof --features ebpf
//! ```

use neurovisor::ebpf::security::{SecurityPolicy, DEFAULT_BLOCKED_PATHS};

fn main() {
    println!("═══════════════════════════════════════════════════════════════");
    println!("  eBPF LSM SECURITY PROOF");
    println!("  File access control for NeuroVisor VMs");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // Check kernel LSM support
    println!("1. CHECKING KERNEL LSM SUPPORT");
    check_lsm_support();
    println!();

    // Show default security policy
    println!("2. DEFAULT SECURITY POLICY");
    println!("   Blocked path prefixes:");
    for path in DEFAULT_BLOCKED_PATHS {
        println!("   ├── {}", path);
    }
    println!();

    // Demonstrate policy API
    println!("3. SECURITY POLICY API");
    let mut policy = SecurityPolicy::new();
    println!("   ├── Default blocked paths: {}", policy.blocked_paths.len());

    // Test path matching
    println!("   │");
    println!("   ├── Testing path matching:");
    let test_paths = [
        ("/etc/shadow", true),
        ("/etc/passwd", false),
        ("/proc/kcore", true),
        ("/tmp/safe.txt", false),
        ("/etc/ssh/ssh_host_rsa_key", true),
    ];

    for (path, should_block) in test_paths {
        let blocked = policy.is_blocked(path);
        let icon = if blocked == should_block { "✅" } else { "❌" };
        let status = if blocked { "BLOCKED" } else { "allowed" };
        println!("   │   {} {} → {}", icon, path, status);
    }

    // Add custom blocked path
    println!("   │");
    policy.block_path("/custom/sensitive");
    println!("   ├── Added custom block: /custom/sensitive");
    println!(
        "   │   /custom/sensitive/data → {}",
        if policy.is_blocked("/custom/sensitive/data") {
            "BLOCKED"
        } else {
            "allowed"
        }
    );

    // Show byte conversion for eBPF map
    println!("   │");
    println!("   └── paths_as_bytes(): {} entries for eBPF map", policy.paths_as_bytes().len());
    println!();

    // Show defense-in-depth integration
    println!("4. DEFENSE-IN-DEPTH INTEGRATION");
    println!();
    println!("   ┌─────────────────────────────────────────────────────────┐");
    println!("   │  Layer 1: CAPABILITIES                                  │");
    println!("   │  Drop CAP_SYS_ADMIN, CAP_NET_RAW, etc.                  │");
    println!("   └─────────────────────────────────────────────────────────┘");
    println!("                            │");
    println!("                            ▼");
    println!("   ┌─────────────────────────────────────────────────────────┐");
    println!("   │  Layer 2: SECCOMP BPF                                   │");
    println!("   │  Whitelist ~50 syscalls, KILL on violation              │");
    println!("   └─────────────────────────────────────────────────────────┘");
    println!("                            │");
    println!("                            ▼");
    println!("   ┌─────────────────────────────────────────────────────────┐");
    println!("   │  Layer 3: eBPF LSM (THIS LAYER)                         │");
    println!("   │  Context-aware file access control                      │");
    println!("   │  - Block /etc/shadow, /proc/kcore, etc.                 │");
    println!("   │  - Return -EACCES on policy violation                   │");
    println!("   └─────────────────────────────────────────────────────────┘");
    println!("                            │");
    println!("                            ▼");
    println!("   ┌─────────────────────────────────────────────────────────┐");
    println!("   │  Layer 4: FIRECRACKER VM                                │");
    println!("   │  KVM hardware virtualization (strongest isolation)      │");
    println!("   └─────────────────────────────────────────────────────────┘");
    println!("                            │");
    println!("                            ▼");
    println!("   ┌─────────────────────────────────────────────────────────┐");
    println!("   │  Layer 5: CGROUPS v2                                    │");
    println!("   │  CPU throttling, memory limits                          │");
    println!("   └─────────────────────────────────────────────────────────┘");
    println!();

    // Comparison with seccomp
    println!("5. SECCOMP vs eBPF LSM COMPARISON");
    println!();
    println!("   ┌────────────────┬──────────────────────────────────────────┐");
    println!("   │ Feature        │ Seccomp BPF          │ eBPF LSM          │");
    println!("   ├────────────────┼──────────────────────┼───────────────────┤");
    println!("   │ Granularity    │ Syscall level        │ Semantic level    │");
    println!("   │ Arguments      │ Limited inspection   │ Full context      │");
    println!("   │ Example        │ Block open()         │ Block /etc/shadow │");
    println!("   │ On violation   │ SIGKILL (process)    │ -EACCES (call)    │");
    println!("   │ Overhead       │ Very low             │ Low               │");
    println!("   └────────────────┴──────────────────────┴───────────────────┘");
    println!();

    println!("═══════════════════════════════════════════════════════════════");
    println!("  LSM provides fine-grained, context-aware security that");
    println!("  complements the binary allow/deny of seccomp.");
    println!("═══════════════════════════════════════════════════════════════");
}

fn check_lsm_support() {
    // Check BTF support
    if std::path::Path::new("/sys/kernel/btf/vmlinux").exists() {
        println!("   ├── ✅ BTF (BPF Type Format) available");
    } else {
        println!("   ├── ⚠️  BTF not available");
    }

    // Check LSM list
    if let Ok(lsm) = std::fs::read_to_string("/sys/kernel/security/lsm") {
        let lsm = lsm.trim();
        if lsm.contains("bpf") {
            println!("   ├── ✅ BPF in LSM list");
        } else {
            println!("   ├── ⚠️  BPF not in LSM list");
            println!("   │   Current LSMs: {}", lsm);
            println!("   │   To enable: Add 'lsm=...,bpf' to kernel cmdline");
        }
    } else {
        println!("   ├── ⚠️  Cannot read /sys/kernel/security/lsm");
    }

    // Check if running as root
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        println!("   └── ✅ Running as root");
    } else {
        println!("   └── ⚠️  Not root (LSM attach requires CAP_MAC_ADMIN)");
    }
}
