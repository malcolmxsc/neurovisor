//! Test cgroups v2 resource isolation
//!
//! Run with: sudo cargo run --bin test_cgroups
//! (Requires root for cgroup operations)

use neurovisor::cgroups::{CgroupManager, ResourceLimits};
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("┌─────────────────────────────────────────┐");
    println!("│  Testing cgroups v2 Resource Isolation  │");
    println!("└─────────────────────────────────────────┘\n");

    // Check if running as root (euid == 0)
    // Using libc directly since nix's geteuid requires the "user" feature
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        eprintln!("Warning: Not running as root (euid={}). cgroup operations may fail.", euid);
        eprintln!("   Run with: sudo cargo run --bin test_cgroups\n");
    }

    // Create the cgroup manager
    println!("1. Creating CgroupManager...");
    let manager = match CgroupManager::new() {
        Ok(m) => {
            println!("   ✅ CgroupManager created");
            m
        }
        Err(e) => {
            eprintln!("   ❌ Failed to create CgroupManager: {}", e);
            eprintln!("   Make sure cgroups v2 is mounted at /sys/fs/cgroup");
            return Err(e.into());
        }
    };

    let vm_id = "test-vm-1";
    let limits = ResourceLimits::medium(); // 2 cores, 4GB

    // Create cgroup
    println!("\n2. Creating cgroup for '{}'...", vm_id);
    println!("   Limits: {} cores, {} GB RAM",
        limits.cpu_cores,
        limits.memory_bytes / (1024 * 1024 * 1024)
    );

    match manager.create(vm_id, limits) {
        Ok(()) => println!("   ✅ Cgroup created"),
        Err(e) => {
            eprintln!("   ❌ Failed to create cgroup: {}", e);
            return Err(e.into());
        }
    }

    // Verify files were created
    println!("\n3. Verifying cgroup files...");
    let cgroup_path = format!("/sys/fs/cgroup/neurovisor/{}", vm_id);

    for file in ["cpu.max", "memory.max", "cgroup.procs"] {
        let path = format!("{}/{}", cgroup_path, file);
        if std::path::Path::new(&path).exists() {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            println!("   ✅ {} = {}", file, content.trim());
        } else {
            println!("   ❌ {} not found", file);
        }
    }

    // Spawn a test process and add it to the cgroup
    println!("\n4. Spawning test process and adding to cgroup...");
    let child = Command::new("sleep")
        .arg("5")
        .spawn()?;

    let pid = child.id();
    println!("   Spawned 'sleep 5' with PID {}", pid);

    match manager.add_process(vm_id, pid) {
        Ok(()) => println!("   ✅ Process {} added to cgroup", pid),
        Err(e) => eprintln!("   ❌ Failed to add process: {}", e),
    }

    // Check cgroup.procs
    let procs_path = format!("{}/cgroup.procs", cgroup_path);
    if let Ok(content) = std::fs::read_to_string(&procs_path) {
        println!("   cgroup.procs now contains: {}", content.trim());
    }

    // Get resource stats
    println!("\n5. Reading resource statistics...");
    match manager.get_memory_usage(vm_id) {
        Ok(bytes) => println!("   Memory usage: {} bytes ({:.2} MB)", bytes, bytes as f64 / (1024.0 * 1024.0)),
        Err(e) => eprintln!("   ❌ Failed to read memory: {}", e),
    }

    match manager.get_cpu_stats(vm_id) {
        Ok(stats) => {
            println!("   CPU usage: {} usec", stats.usage_usec);
            println!("   User time: {} usec", stats.user_usec);
            println!("   System time: {} usec", stats.system_usec);
            println!("   Times throttled: {}", stats.nr_throttled);
        }
        Err(e) => eprintln!("   ❌ Failed to read CPU stats: {}", e),
    }

    // List all VMs
    println!("\n6. Listing all VM cgroups...");
    match manager.list_vms() {
        Ok(vms) => {
            for vm in &vms {
                println!("   - {}", vm);
            }
            if vms.is_empty() {
                println!("   (none)");
            }
        }
        Err(e) => eprintln!("   ❌ Failed to list VMs: {}", e),
    }

    // Clean up - wait for process to finish first
    println!("\n7. Cleaning up (waiting for test process)...");
    std::thread::sleep(std::time::Duration::from_secs(1));

    // Kill the test process so we can remove the cgroup
    let _ = Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .output();

    std::thread::sleep(std::time::Duration::from_millis(100));

    match manager.destroy(vm_id) {
        Ok(()) => println!("   ✅ Cgroup destroyed"),
        Err(e) => eprintln!("   ❌ Failed to destroy cgroup: {}", e),
    }

    println!("\n┌─────────────────────────────────────────┐");
    println!("│  ✅ cgroups test complete!              │");
    println!("└─────────────────────────────────────────┘");

    Ok(())
}
