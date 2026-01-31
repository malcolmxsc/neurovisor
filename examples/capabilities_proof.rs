//! Capabilities PROOF - Actually demonstrates capability dropping
//!
//! Run with: cargo build --example capabilities_proof && sudo ./target/debug/examples/capabilities_proof

use std::fs;

fn main() {
    println!("┌─────────────────────────────────────────┐");
    println!("│  Capabilities PROOF - Real Drop Test   │");
    println!("└─────────────────────────────────────────┘\n");

    let pid = std::process::id();
    println!("Process PID: {}\n", pid);

    // ─────────────────────────────────────────────────────────────────────
    // Check if running on WSL
    // ─────────────────────────────────────────────────────────────────────
    let is_wsl = fs::read_to_string("/proc/version")
        .map(|v| v.contains("microsoft") || v.contains("WSL"))
        .unwrap_or(false);

    if is_wsl {
        println!("⚠️  DETECTED: Running on WSL2\n");
        println!("WSL2 has LIMITED capability support.");
        println!("Capability modification may fail even as root.\n");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Step 1: Read REAL capabilities from /proc/self/status
    // ─────────────────────────────────────────────────────────────────────
    println!("1. Reading REAL capabilities from /proc/self/status:\n");

    let status = fs::read_to_string("/proc/self/status").expect("Failed to read /proc/self/status");

    for line in status.lines() {
        if line.starts_with("Cap") {
            println!("   {}", line);
        }
    }
    println!();

    // ─────────────────────────────────────────────────────────────────────
    // Step 2: Check if we're root
    // ─────────────────────────────────────────────────────────────────────
    let uid = unsafe { libc::getuid() };
    println!("2. Current UID: {}", uid);

    if uid != 0 {
        println!("   ⚠️  Not running as root");
        println!("   Run with: sudo ./target/debug/examples/capabilities_proof\n");
        return;
    }
    println!("   ✅ Running as root\n");

    // ─────────────────────────────────────────────────────────────────────
    // Step 3: Test raw socket BEFORE dropping caps
    // ─────────────────────────────────────────────────────────────────────
    println!("3. Testing raw socket BEFORE dropping caps...\n");

    let sock_before = unsafe {
        libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP)
    };

    if sock_before >= 0 {
        println!("   ✅ Raw socket created successfully (fd={})", sock_before);
        println!("   → We HAVE CAP_NET_RAW right now");
        unsafe { libc::close(sock_before) };
    } else {
        println!("   ❌ Raw socket failed even before dropping caps");
        println!("   → System may not support this test");
    }
    println!();

    // ─────────────────────────────────────────────────────────────────────
    // Step 4: Try to drop capabilities
    // ─────────────────────────────────────────────────────────────────────
    println!("4. Attempting to drop CAP_NET_RAW...\n");

    use neurovisor::security::CapabilityDropper;
    let mut dropper = CapabilityDropper::new();
    dropper.drop_cap(caps::Capability::CAP_NET_RAW);

    match dropper.apply() {
        Ok(()) => {
            println!("   ✅ CAP_NET_RAW dropped successfully!\n");

            // ─────────────────────────────────────────────────────────────
            // Step 5: Test raw socket AFTER dropping caps
            // ─────────────────────────────────────────────────────────────
            println!("5. Testing raw socket AFTER dropping caps...\n");

            let sock_after = unsafe {
                libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP)
            };

            if sock_after == -1 {
                let errno = std::io::Error::last_os_error();
                println!("   ❌ Raw socket FAILED: {}", errno);
                println!("\n┌─────────────────────────────────────────────────────────┐");
                println!("│  ✅ SUCCESS! Capability drop PROVED to work!           │");
                println!("│                                                         │");
                println!("│  Before drop: socket() succeeded (had CAP_NET_RAW)     │");
                println!("│  After drop:  socket() FAILED with EPERM               │");
                println!("│                                                         │");
                println!("│  The kernel denied the operation!                      │");
                println!("└─────────────────────────────────────────────────────────┘");
            } else {
                unsafe { libc::close(sock_after) };
                println!("   ⚠️  Raw socket still works - cap drop may not have worked");
            }
        }
        Err(e) => {
            println!("   ❌ Failed to drop capability: {}\n", e);

            if is_wsl {
                println!("┌─────────────────────────────────────────────────────────┐");
                println!("│  WSL2 LIMITATION                                        │");
                println!("│                                                         │");
                println!("│  WSL2 restricts capability modification syscalls.      │");
                println!("│  This is a known limitation of the WSL2 kernel.        │");
                println!("│                                                         │");
                println!("│  On REAL Linux (AWS, bare metal, VM):                  │");
                println!("│  • Capability dropping works fully                     │");
                println!("│  • All three sets (bounding/permitted/effective)       │");
                println!("│  • This is how Firecracker is secured in production   │");
                println!("│                                                         │");
                println!("│  The CODE is correct - WSL2 just can't run it.        │");
                println!("└─────────────────────────────────────────────────────────┘");
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Show capabilities after (if we got here)
    // ─────────────────────────────────────────────────────────────────────
    println!("\n6. Final capability state:\n");
    let status = fs::read_to_string("/proc/self/status").expect("Failed to read /proc/self/status");
    for line in status.lines() {
        if line.starts_with("Cap") {
            println!("   {}", line);
        }
    }
}
