//! Seccomp PROOF - Actually demonstrates syscall blocking
//!
//! This forks a child process, applies a filter, and tries a blocked syscall.
//! The child WILL BE KILLED - proving the filter works.
//!
//! Run with: cargo build --example seccomp_proof && sudo ./target/debug/examples/seccomp_proof

use std::process::exit;

fn main() {
    println!("┌─────────────────────────────────────────┐");
    println!("│  Seccomp PROOF - Real Blocking Test    │");
    println!("└─────────────────────────────────────────┘\n");

    println!("This test PROVES seccomp works by:");
    println!("  1. Forking a child process");
    println!("  2. Child applies restrictive filter (blocks getpid)");
    println!("  3. Child calls getpid() - a blocked syscall");
    println!("  4. Child gets KILLED by kernel\n");

    // Fork a child process
    let pid = unsafe { libc::fork() };

    match pid {
        -1 => {
            eprintln!("Fork failed!");
            exit(1);
        }
        0 => {
            // ═══════════════════════════════════════════════════════════════
            // CHILD PROCESS - will be killed by seccomp
            // ═══════════════════════════════════════════════════════════════
            println!("[CHILD] PID {} - I'm the child process", unsafe { libc::getpid() });
            println!("[CHILD] Applying seccomp filter that BLOCKS getpid()...");

            // Create a filter that allows basic ops but blocks getpid
            use neurovisor::security::FirecrackerSeccomp;
            let mut filter = FirecrackerSeccomp::new();
            filter
                .allow(libc::SYS_write)      // Need this for println
                .allow(libc::SYS_exit_group) // Need this for clean exit
                .allow(libc::SYS_brk)        // Memory allocation
                .allow(libc::SYS_mmap)       // Memory allocation
                .allow(libc::SYS_munmap)     // Memory cleanup
                .allow(libc::SYS_futex)      // Threading
                .allow(libc::SYS_rt_sigprocmask) // Signals
                .allow(libc::SYS_rt_sigaction);  // Signals
            // NOTE: getpid is NOT allowed!

            if let Err(e) = filter.apply() {
                eprintln!("[CHILD] Failed to apply filter: {}", e);
                exit(1);
            }

            println!("[CHILD] Filter applied! Now calling getpid()...");
            println!("[CHILD] If you see this, the filter is working so far.");
            println!("[CHILD] Next line calls getpid() - I should be KILLED:\n");

            // This syscall is BLOCKED - kernel will kill us with SIGSYS
            let _pid = unsafe { libc::getpid() };

            // If we reach here, the filter FAILED
            println!("[CHILD] ERROR: I survived! Filter didn't work!");
            exit(1);
        }
        child_pid => {
            // ═══════════════════════════════════════════════════════════════
            // PARENT PROCESS - waits for child and checks exit status
            // ═══════════════════════════════════════════════════════════════
            println!("[PARENT] Waiting for child (PID {})...\n", child_pid);

            let mut status: i32 = 0;
            unsafe { libc::waitpid(child_pid, &mut status, 0) };

            println!("═══════════════════════════════════════════════════════");

            if libc::WIFSIGNALED(status) {
                let signal = libc::WTERMSIG(status);
                let signal_name = match signal {
                    31 => "SIGSYS (seccomp violation!)",
                    9 => "SIGKILL",
                    6 => "SIGABRT",
                    _ => "unknown signal",
                };

                println!("[PARENT] Child was KILLED by signal {} ({})", signal, signal_name);

                if signal == 31 {
                    println!("\n┌─────────────────────────────────────────────────────┐");
                    println!("│  ✅ SUCCESS! Seccomp filter PROVED to work!        │");
                    println!("│                                                     │");
                    println!("│  The child tried to call getpid() which was        │");
                    println!("│  blocked by our filter. The kernel killed it       │");
                    println!("│  with SIGSYS (signal 31).                          │");
                    println!("│                                                     │");
                    println!("│  This is EXACTLY what would happen if Firecracker  │");
                    println!("│  tried to call execve, ptrace, or other blocked    │");
                    println!("│  syscalls - instant death!                         │");
                    println!("└─────────────────────────────────────────────────────┘");
                } else if signal == 9 {
                    println!("\n✅ SUCCESS! Child was killed (SIGKILL action)");
                }
            } else if libc::WIFEXITED(status) {
                let exit_code = libc::WEXITSTATUS(status);
                println!("[PARENT] Child exited normally with code {}", exit_code);
                if exit_code == 0 {
                    println!("\n❌ FAILURE: Child should have been killed!");
                }
            } else {
                println!("[PARENT] Child terminated with unknown status: {}", status);
            }

            println!("\n═══════════════════════════════════════════════════════");
        }
    }
}
