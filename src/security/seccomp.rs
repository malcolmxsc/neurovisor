//! Seccomp BPF filter for Firecracker process isolation
//!
//! # What is Seccomp?
//!
//! Seccomp (Secure Computing) is a Linux kernel feature that restricts
//! which system calls a process can make. We use it to limit what
//! Firecracker can do - even if an attacker escapes the VM, they can't
//! use dangerous syscalls like `execve` (run programs) or `ptrace` (debug).
//!
//! # How BPF Filters Work
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────┐
//! │  BPF = Berkeley Packet Filter                                  │
//! │  Originally for network packets, now used for syscall filtering│
//! │                                                                │
//! │  ┌──────────┐     ┌─────────────┐     ┌──────────┐            │
//! │  │ Syscall  │────►│ BPF Program │────►│ Decision │            │
//! │  │ (read,   │     │ (tiny code  │     │ ALLOW or │            │
//! │  │  write,  │     │  in kernel) │     │ KILL     │            │
//! │  │  ioctl)  │     └─────────────┘     └──────────┘            │
//! │  └──────────┘                                                  │
//! └────────────────────────────────────────────────────────────────┘
//! ```

use seccompiler::{
    BpfProgram,       // Vec<sock_filter> - the compiled bytecode
    SeccompAction,    // What to do: Allow, Kill, Errno, etc.
    SeccompFilter,    // The filter with rules
    SeccompRule,      // A single rule (conditions for a syscall)
    TargetArch,       // CPU architecture (x86_64, aarch64)
};
use std::convert::TryInto;
use std::io;

// ═══════════════════════════════════════════════════════════════════════════
// FirecrackerSeccomp
// ═══════════════════════════════════════════════════════════════════════════
// WHAT IT IS:
//   A builder that creates a seccomp filter tailored for Firecracker.
//   It whitelists only the syscalls Firecracker needs to run VMs.
//
// WHY WE NEED IT:
//   Firecracker uses KVM to run VMs. It needs syscalls like:
//   - ioctl (talk to /dev/kvm)
//   - read/write (file I/O)
//   - mmap (memory mapping for guest RAM)
//
//   But it does NOT need dangerous syscalls like:
//   - execve (run other programs)
//   - ptrace (debug/hijack other processes)
//   - mount (access filesystems)
// ═══════════════════════════════════════════════════════════════════════════

pub struct FirecrackerSeccomp {
    /// List of syscall numbers we allow
    /// (stored as libc constants like libc::SYS_read)
    allowed_syscalls: Vec<i64>,
}

impl FirecrackerSeccomp {
    // ═══════════════════════════════════════════════════════════════════════
    // new() - Constructor
    // ═══════════════════════════════════════════════════════════════════════
    // WHAT IT DOES:
    //   Creates a new builder with an empty whitelist.
    //   You then call .allow() to add syscalls.
    //
    // RETURNS:
    //   FirecrackerSeccomp - the builder object
    // ═══════════════════════════════════════════════════════════════════════
    pub fn new() -> Self {
        Self {
            allowed_syscalls: Vec::new(),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // with_firecracker_defaults() - Pre-configured for Firecracker
    // ═══════════════════════════════════════════════════════════════════════
    // WHAT IT DOES:
    //   Creates a filter with all the syscalls Firecracker needs.
    //   This is the recommended way to create a filter.
    //
    // SYSCALL CATEGORIES:
    //   1. Basic I/O: read, write, close
    //   2. Memory: mmap, munmap, mprotect, brk
    //   3. KVM: ioctl (for /dev/kvm)
    //   4. Files: open, openat, fstat, fcntl
    //   5. Events: epoll_*, eventfd, timerfd
    //   6. Signals: rt_sigaction, rt_sigprocmask
    //   7. Exit: exit, exit_group
    //
    // RETURNS:
    //   FirecrackerSeccomp - pre-configured with safe defaults
    // ═══════════════════════════════════════════════════════════════════════
    pub fn with_firecracker_defaults() -> Self {
        let mut filter = Self::new();

        // ─────────────────────────────────────────────────────────────────
        // Basic I/O - reading/writing to files, sockets, vsock
        // ─────────────────────────────────────────────────────────────────
        filter.allow(libc::SYS_read);       // read from fd
        filter.allow(libc::SYS_write);      // write to fd
        filter.allow(libc::SYS_close);      // close fd
        filter.allow(libc::SYS_lseek);      // seek in file
        filter.allow(libc::SYS_pread64);    // read at offset
        filter.allow(libc::SYS_pwrite64);   // write at offset
        filter.allow(libc::SYS_readv);      // scatter read
        filter.allow(libc::SYS_writev);     // gather write

        // ─────────────────────────────────────────────────────────────────
        // Memory Management - Firecracker needs these for guest RAM
        // ─────────────────────────────────────────────────────────────────
        filter.allow(libc::SYS_mmap);       // map memory (guest RAM!)
        filter.allow(libc::SYS_munmap);     // unmap memory
        filter.allow(libc::SYS_mprotect);   // change memory permissions
        filter.allow(libc::SYS_brk);        // grow heap
        filter.allow(libc::SYS_madvise);    // memory hints

        // ─────────────────────────────────────────────────────────────────
        // KVM - The heart of virtualization
        // ─────────────────────────────────────────────────────────────────
        // ioctl is used to control /dev/kvm:
        //   KVM_CREATE_VM, KVM_CREATE_VCPU, KVM_RUN, etc.
        filter.allow(libc::SYS_ioctl);      // device control (KVM!)

        // ─────────────────────────────────────────────────────────────────
        // File Operations - opening config, rootfs, kernel
        // ─────────────────────────────────────────────────────────────────
        filter.allow(libc::SYS_openat);     // open file (modern)
        filter.allow(libc::SYS_fstat);      // file stats
        filter.allow(libc::SYS_newfstatat); // file stats (modern)
        filter.allow(libc::SYS_fcntl);      // file control
        filter.allow(libc::SYS_dup);        // duplicate fd
        filter.allow(libc::SYS_dup2);       // duplicate fd to specific number

        // ─────────────────────────────────────────────────────────────────
        // Event Loop - async I/O for network, vsock, timers
        // ─────────────────────────────────────────────────────────────────
        filter.allow(libc::SYS_epoll_create1);  // create epoll instance
        filter.allow(libc::SYS_epoll_ctl);      // add/remove fds to epoll
        filter.allow(libc::SYS_epoll_wait);     // wait for events
        filter.allow(libc::SYS_epoll_pwait);    // wait with signal mask
        filter.allow(libc::SYS_eventfd2);       // create eventfd
        filter.allow(libc::SYS_timerfd_create); // create timer fd
        filter.allow(libc::SYS_timerfd_settime);// set timer

        // ─────────────────────────────────────────────────────────────────
        // Signals - handling interrupts, shutdown
        // ─────────────────────────────────────────────────────────────────
        filter.allow(libc::SYS_rt_sigaction);   // set signal handler
        filter.allow(libc::SYS_rt_sigprocmask); // block/unblock signals
        filter.allow(libc::SYS_rt_sigreturn);   // return from signal handler

        // ─────────────────────────────────────────────────────────────────
        // Process - exit cleanly
        // ─────────────────────────────────────────────────────────────────
        filter.allow(libc::SYS_exit);        // exit thread
        filter.allow(libc::SYS_exit_group);  // exit all threads

        // ─────────────────────────────────────────────────────────────────
        // Misc - futex for threading, clock for time
        // ─────────────────────────────────────────────────────────────────
        filter.allow(libc::SYS_futex);          // threading primitive
        filter.allow(libc::SYS_clock_gettime);  // get current time
        filter.allow(libc::SYS_nanosleep);      // sleep
        filter.allow(libc::SYS_getrandom);      // random numbers

        filter
    }

    // ═══════════════════════════════════════════════════════════════════════
    // allow(syscall) - Add syscall to whitelist
    // ═══════════════════════════════════════════════════════════════════════
    // WHAT IT DOES:
    //   Adds a single syscall to the allowed list.
    //
    // ARGUMENTS:
    //   syscall: i64 - the syscall number (e.g., libc::SYS_read = 0)
    //
    // RETURNS:
    //   &mut Self - returns self for method chaining
    // ═══════════════════════════════════════════════════════════════════════
    pub fn allow(&mut self, syscall: i64) -> &mut Self {
        self.allowed_syscalls.push(syscall);
        self
    }

    // ═══════════════════════════════════════════════════════════════════════
    // build() - Compile the filter into BPF bytecode
    // ═══════════════════════════════════════════════════════════════════════
    // WHAT IT DOES:
    //   Takes our list of allowed syscalls and compiles them into a
    //   BPF program that the kernel can execute.
    //
    // HOW IT WORKS:
    //   1. Create rules: syscall_number → empty vec (no conditions)
    //   2. Empty vec = "always allow this syscall" (no extra checks)
    //   3. Create SeccompFilter with mismatch_action = KillProcess
    //   4. Compile to BPF bytecode
    //
    // ```text
    // ┌─────────────────────────────────────────────────────────┐
    // │  allowed_syscalls: [read, write, ioctl, mmap, ...]     │
    // │           │                                             │
    // │           ▼                                             │
    // │  ┌─────────────────────────────────────────────────┐   │
    // │  │ SeccompFilter                                   │   │
    // │  │   mismatch_action: KillProcess                  │   │
    // │  │   match_action: Allow                           │   │
    // │  │   rules:                                        │   │
    // │  │     read  → [] (empty = always allow)           │   │
    // │  │     write → []                                  │   │
    // │  │     ioctl → []                                  │   │
    // │  │     (anything else) → KillProcess               │   │
    // │  └─────────────────────────────────────────────────┘   │
    // │           │                                             │
    // │           ▼                                             │
    // │  BpfMap { "x86_64": [bytecode...] }                    │
    // └─────────────────────────────────────────────────────────┘
    // ```
    //
    // RETURNS:
    //   Result<BpfProgram, io::Error> - compiled BPF bytecode
    // ═══════════════════════════════════════════════════════════════════════
    pub fn build(&self) -> Result<BpfProgram, io::Error> {
        // Build rules: syscall_number → Vec<SeccompRule>
        // Empty vec means "always allow with no extra conditions"
        let rules: Vec<(i64, Vec<SeccompRule>)> = self
            .allowed_syscalls
            .iter()
            .map(|&syscall| (syscall, vec![]))  // empty vec = no conditions
            .collect();

        // Get target architecture from current system
        // std::env::consts::ARCH is "x86_64" or "aarch64"
        let arch: TargetArch = std::env::consts::ARCH
            .try_into()
            .map_err(|e: seccompiler::BackendError| {
                io::Error::new(io::ErrorKind::Other, e.to_string())
            })?;

        // Create the filter with:
        // - rules: our whitelist (empty vec = always allow)
        // - mismatch_action: KillProcess (if syscall not in rules)
        // - match_action: Allow (what to do for whitelisted syscalls)
        // - target_arch: from current system
        let filter = SeccompFilter::new(
            rules.into_iter().collect(),  // Convert to BTreeMap
            SeccompAction::KillProcess,   // Default: kill if not whitelisted
            SeccompAction::Allow,         // What to do for whitelisted syscalls
            arch,
        ).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        // Compile to BPF bytecode
        // BpfProgram is Vec<sock_filter>
        let bpf_prog: BpfProgram = filter
            .try_into()
            .map_err(|e: seccompiler::BackendError| {
                io::Error::new(io::ErrorKind::Other, e.to_string())
            })?;

        Ok(bpf_prog)
    }

    // ═══════════════════════════════════════════════════════════════════════
    // apply() - Install the filter on the current process
    // ═══════════════════════════════════════════════════════════════════════
    // WHAT IT DOES:
    //   Loads the BPF filter into the kernel and activates it.
    //   After this call, any disallowed syscall will kill the process.
    //
    // ⚠️ WARNING:
    //   This is IRREVERSIBLE! Once applied, you cannot remove the filter.
    //   The filter is inherited by child processes (like Firecracker).
    //
    // USAGE:
    //   Call this RIGHT BEFORE spawning Firecracker, so the filter
    //   applies to it but not to your main NeuroVisor process.
    //
    // RETURNS:
    //   Result<(), io::Error> - Ok if filter was applied
    // ═══════════════════════════════════════════════════════════════════════
    pub fn apply(&self) -> Result<(), io::Error> {
        let bpf_prog = self.build()?;

        // Apply the filter using seccompiler
        // This makes a prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, ...) syscall
        // After this, any syscall not in our whitelist will KILL the process
        seccompiler::apply_filter(&bpf_prog)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        Ok(())
    }

    /// Get count of allowed syscalls
    pub fn allowed_count(&self) -> usize {
        self.allowed_syscalls.len()
    }

    /// Get the list of blocked syscalls (dangerous ones we DON'T allow)
    /// Useful for logging/debugging
    pub fn blocked_syscalls() -> Vec<(&'static str, i64)> {
        vec![
            ("execve", libc::SYS_execve),         // Run other programs
            ("execveat", libc::SYS_execveat),     // Run programs (modern)
            ("fork", libc::SYS_fork),             // Create child process
            ("clone", libc::SYS_clone),           // Create thread/process
            ("ptrace", libc::SYS_ptrace),         // Debug other processes
            ("mount", libc::SYS_mount),           // Mount filesystems
            ("umount2", libc::SYS_umount2),       // Unmount filesystems
            ("pivot_root", libc::SYS_pivot_root), // Change root filesystem
            ("chroot", libc::SYS_chroot),         // Change root directory
            ("setuid", libc::SYS_setuid),         // Change user ID
            ("setgid", libc::SYS_setgid),         // Change group ID
            ("init_module", libc::SYS_init_module),     // Load kernel module
            ("delete_module", libc::SYS_delete_module), // Unload kernel module
        ]
    }
}

impl Default for FirecrackerSeccomp {
    fn default() -> Self {
        Self::with_firecracker_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_filter() {
        // Test that we can build a filter without errors
        let filter = FirecrackerSeccomp::with_firecracker_defaults();
        let result = filter.build();
        assert!(result.is_ok(), "Failed to build filter: {:?}", result.err());
    }

    #[test]
    fn test_empty_filter() {
        // Empty filter should still build (just blocks everything)
        let filter = FirecrackerSeccomp::new();
        let result = filter.build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_custom_filter() {
        // Test adding custom syscalls
        let mut filter = FirecrackerSeccomp::new();
        filter
            .allow(libc::SYS_read)
            .allow(libc::SYS_write)
            .allow(libc::SYS_exit_group);

        assert_eq!(filter.allowed_count(), 3);

        let result = filter.build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_default_has_syscalls() {
        let filter = FirecrackerSeccomp::with_firecracker_defaults();
        // Should have many syscalls whitelisted
        assert!(filter.allowed_count() > 20);
    }
}
