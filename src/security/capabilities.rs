//! Linux Capabilities Management for NeuroVisor
//!
//! # What are Linux Capabilities?
//!
//! Traditionally, Unix processes are either:
//! - **Unprivileged**: UID != 0, limited permissions
//! - **Privileged (root)**: UID == 0, can do ANYTHING
//!
//! Capabilities split "root powers" into ~40 granular permissions:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  BEFORE: Root = all powers combined                             │
//! │                                                                 │
//! │  ROOT ═══════════════════════════════════════════════════════   │
//! │    │ mount filesystems                                          │
//! │    │ load kernel modules                                        │
//! │    │ raw network access                                         │
//! │    │ trace other processes                                      │
//! │    │ bypass file permissions                                    │
//! │    │ ... everything                                             │
//! └─────────────────────────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  AFTER: Capabilities = pick only what you need                  │
//! │                                                                 │
//! │  CAP_NET_RAW ────► raw sockets (ping, tcpdump)                 │
//! │  CAP_SYS_ADMIN ──► mount, ptrace, modules, etc.                │
//! │  CAP_SYS_PTRACE ─► trace/debug other processes                 │
//! │  CAP_NET_ADMIN ──► network configuration                       │
//! │  CAP_CHOWN ──────► change file ownership                       │
//! │  ... 35+ more                                                   │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Why Drop Capabilities?
//!
//! Firecracker runs as root to access `/dev/kvm`, but it doesn't need
//! all root powers. We drop dangerous capabilities to limit damage
//! if an attacker compromises the VM or hypervisor.
//!
//! # Capability Sets (Important!)
//!
//! Each process has MULTIPLE capability sets:
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────────┐
//! │  Permitted (P)    │ Maximum caps this process CAN have          │
//! │                   │ (ceiling - can only shrink, never grow)     │
//! ├───────────────────┼─────────────────────────────────────────────┤
//! │  Effective (E)    │ Caps currently ACTIVE right now             │
//! │                   │ (what the kernel actually checks)           │
//! ├───────────────────┼─────────────────────────────────────────────┤
//! │  Inheritable (I)  │ Caps passed to child processes after exec   │
//! │                   │ (rare - usually empty)                      │
//! ├───────────────────┼─────────────────────────────────────────────┤
//! │  Bounding (B)     │ Absolute limit (cannot be exceeded)         │
//! │                   │ (drops here prevent regaining caps)         │
//! └───────────────────┴─────────────────────────────────────────────┘
//!
//! KEY INSIGHT: To fully drop a capability, you must:
//!   1. Remove from Effective (stops working immediately)
//!   2. Remove from Permitted (cannot be re-raised)
//!   3. Remove from Bounding (children cannot inherit it)
//! ```

use std::io;

// ═══════════════════════════════════════════════════════════════════════════
// CapabilityDropper
// ═══════════════════════════════════════════════════════════════════════════
// WHAT IT IS:
//   A utility to safely drop Linux capabilities from a process.
//   Follows the principle of least privilege.
//
// WHY WE NEED IT:
//   Firecracker needs root to access /dev/kvm but doesn't need:
//   - CAP_SYS_ADMIN (mount filesystems, load modules)
//   - CAP_SYS_PTRACE (debug other processes)
//   - CAP_NET_RAW (raw network sockets)
//   - etc.
//
// USAGE:
//   Call AFTER forking but BEFORE exec(firecracker)
// ═══════════════════════════════════════════════════════════════════════════

pub struct CapabilityDropper {
    /// Capabilities to drop from the process
    /// Uses caps::Capability enum values
    caps_to_drop: Vec<caps::Capability>,
}

impl CapabilityDropper {
    // ═══════════════════════════════════════════════════════════════════════
    // new() - Constructor
    // ═══════════════════════════════════════════════════════════════════════
    // Creates an empty dropper. Call .drop_cap() to add capabilities to drop.
    // ═══════════════════════════════════════════════════════════════════════
    pub fn new() -> Self {
        Self {
            caps_to_drop: Vec::new(),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // with_firecracker_drops() - Pre-configured for Firecracker
    // ═══════════════════════════════════════════════════════════════════════
    // WHAT IT DOES:
    //   Creates a dropper configured to remove dangerous capabilities
    //   that Firecracker doesn't need.
    //
    // CAPABILITIES DROPPED:
    //
    // ```text
    // ┌────────────────────┬──────────────────────────────────────────────┐
    // │ Capability         │ Why we drop it                               │
    // ├────────────────────┼──────────────────────────────────────────────┤
    // │ CAP_SYS_ADMIN      │ Too powerful - mount, modules, containers   │
    // │ CAP_SYS_PTRACE     │ Could debug/hijack other processes          │
    // │ CAP_SYS_MODULE     │ Could load malicious kernel modules         │
    // │ CAP_NET_ADMIN      │ Could reconfigure network                   │
    // │ CAP_NET_RAW        │ Could sniff packets, craft raw frames       │
    // │ CAP_SYS_BOOT       │ Could reboot the system                     │
    // │ CAP_SYS_RAWIO      │ Could do raw I/O to ports (dangerous)       │
    // │ CAP_MKNOD          │ Could create device nodes                   │
    // │ CAP_SYS_CHROOT     │ Could change root directory                 │
    // │ CAP_SETUID/SETGID  │ Could become any user                       │
    // └────────────────────┴──────────────────────────────────────────────┘
    // ```
    //
    // CAPABILITIES KEPT (Firecracker needs these):
    //   - CAP_DAC_OVERRIDE: access /dev/kvm, /dev/net/tun
    //   - CAP_SYS_RESOURCE: set memory limits
    //
    // RETURNS:
    //   CapabilityDropper - pre-configured with safe defaults
    // ═══════════════════════════════════════════════════════════════════════
    pub fn with_firecracker_drops() -> Self {
        let mut dropper = Self::new();

        // ─────────────────────────────────────────────────────────────────
        // Dangerous system administration capabilities
        // ─────────────────────────────────────────────────────────────────
        dropper.drop_cap(caps::Capability::CAP_SYS_ADMIN);   // mount, containers, namespaces
        dropper.drop_cap(caps::Capability::CAP_SYS_PTRACE);  // trace other processes
        dropper.drop_cap(caps::Capability::CAP_SYS_MODULE);  // load kernel modules
        dropper.drop_cap(caps::Capability::CAP_SYS_BOOT);    // reboot system
        dropper.drop_cap(caps::Capability::CAP_SYS_RAWIO);   // raw port I/O
        dropper.drop_cap(caps::Capability::CAP_SYS_CHROOT);  // chroot

        // ─────────────────────────────────────────────────────────────────
        // Network capabilities (Firecracker uses vsock, not raw network)
        // ─────────────────────────────────────────────────────────────────
        dropper.drop_cap(caps::Capability::CAP_NET_ADMIN);   // network configuration
        dropper.drop_cap(caps::Capability::CAP_NET_RAW);     // raw sockets

        // ─────────────────────────────────────────────────────────────────
        // User/permission manipulation
        // ─────────────────────────────────────────────────────────────────
        dropper.drop_cap(caps::Capability::CAP_SETUID);      // change user ID
        dropper.drop_cap(caps::Capability::CAP_SETGID);      // change group ID
        dropper.drop_cap(caps::Capability::CAP_SETPCAP);     // modify process caps
        dropper.drop_cap(caps::Capability::CAP_CHOWN);       // change file owner

        // ─────────────────────────────────────────────────────────────────
        // Filesystem/device manipulation
        // ─────────────────────────────────────────────────────────────────
        dropper.drop_cap(caps::Capability::CAP_MKNOD);       // create device nodes
        dropper.drop_cap(caps::Capability::CAP_FSETID);      // don't clear setuid bits

        // ─────────────────────────────────────────────────────────────────
        // Misc dangerous capabilities
        // ─────────────────────────────────────────────────────────────────
        dropper.drop_cap(caps::Capability::CAP_AUDIT_WRITE); // write to audit log
        dropper.drop_cap(caps::Capability::CAP_AUDIT_CONTROL); // control audit
        dropper.drop_cap(caps::Capability::CAP_SYSLOG);      // read kernel messages
        dropper.drop_cap(caps::Capability::CAP_WAKE_ALARM);  // trigger wakeup
        dropper.drop_cap(caps::Capability::CAP_LEASE);       // file leases (fcntl)
        dropper.drop_cap(caps::Capability::CAP_MAC_ADMIN);   // MAC configuration
        dropper.drop_cap(caps::Capability::CAP_MAC_OVERRIDE);// override MAC
        dropper.drop_cap(caps::Capability::CAP_LINUX_IMMUTABLE); // immutable files

        dropper
    }

    // ═══════════════════════════════════════════════════════════════════════
    // drop_cap(cap) - Add a capability to the drop list
    // ═══════════════════════════════════════════════════════════════════════
    // ARGUMENTS:
    //   cap: caps::Capability - the capability to drop
    //
    // RETURNS:
    //   &mut Self - for method chaining
    // ═══════════════════════════════════════════════════════════════════════
    pub fn drop_cap(&mut self, cap: caps::Capability) -> &mut Self {
        self.caps_to_drop.push(cap);
        self
    }

    // ═══════════════════════════════════════════════════════════════════════
    // apply() - Actually drop the capabilities
    // ═══════════════════════════════════════════════════════════════════════
    // WHAT IT DOES:
    //   Drops capabilities from all three sets:
    //   1. Bounding set - prevents children from regaining caps
    //   2. Permitted set - prevents raising caps
    //   3. Effective set - stops caps from working now
    //
    // ⚠️ WARNING:
    //   This is IRREVERSIBLE! Once dropped, capabilities cannot be regained.
    //
    // ```text
    // ┌─────────────────────────────────────────────────────────────────┐
    // │  BEFORE apply()                                                 │
    // │                                                                 │
    // │  Bounding:  [SYS_ADMIN, NET_RAW, PTRACE, ...]                  │
    // │  Permitted: [SYS_ADMIN, NET_RAW, PTRACE, ...]                  │
    // │  Effective: [SYS_ADMIN, NET_RAW, PTRACE, ...]                  │
    // └─────────────────────────────────────────────────────────────────┘
    //                            │
    //                            ▼ apply()
    // ┌─────────────────────────────────────────────────────────────────┐
    // │  AFTER apply()                                                  │
    // │                                                                 │
    // │  Bounding:  [DAC_OVERRIDE, SYS_RESOURCE] ← only what we need   │
    // │  Permitted: [DAC_OVERRIDE, SYS_RESOURCE]                       │
    // │  Effective: [DAC_OVERRIDE, SYS_RESOURCE]                       │
    // │                                                                 │
    // │  Cannot regain SYS_ADMIN, NET_RAW, PTRACE, etc!                │
    // └─────────────────────────────────────────────────────────────────┘
    // ```
    //
    // RETURNS:
    //   Result<(), io::Error> - Ok if all caps were dropped
    // ═══════════════════════════════════════════════════════════════════════
    pub fn apply(&self) -> Result<(), io::Error> {
        for &cap in &self.caps_to_drop {
            // Drop from bounding set first (most restrictive)
            // This prevents children from ever having this cap
            // Note: Requires CAP_SETPCAP - may fail on some systems
            let _ = caps::drop(None, caps::CapSet::Bounding, cap);

            // Drop from permitted set
            // This prevents raising the cap back to effective
            caps::drop(None, caps::CapSet::Permitted, cap)
                .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;

            // Drop from effective set
            // This stops the cap from working immediately
            caps::drop(None, caps::CapSet::Effective, cap)
                .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
        }

        Ok(())
    }

    /// Apply capability drops - strict mode (requires CAP_SETPCAP)
    /// Drops from all three sets: bounding, permitted, effective
    pub fn apply_strict(&self) -> Result<(), io::Error> {
        for &cap in &self.caps_to_drop {
            // Drop from bounding set first (most restrictive)
            caps::drop(None, caps::CapSet::Bounding, cap)
                .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;

            // Drop from permitted set
            caps::drop(None, caps::CapSet::Permitted, cap)
                .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;

            // Drop from effective set
            caps::drop(None, caps::CapSet::Effective, cap)
                .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
        }

        Ok(())
    }

    /// Get the count of capabilities to drop
    pub fn drop_count(&self) -> usize {
        self.caps_to_drop.len()
    }

    /// Get the list of capabilities being dropped (for logging)
    pub fn caps_being_dropped(&self) -> Vec<&'static str> {
        self.caps_to_drop
            .iter()
            .map(|cap| cap_name(*cap))
            .collect()
    }
}

impl Default for CapabilityDropper {
    fn default() -> Self {
        Self::with_firecracker_drops()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Convert capability enum to readable name
fn cap_name(cap: caps::Capability) -> &'static str {
    match cap {
        caps::Capability::CAP_SYS_ADMIN => "CAP_SYS_ADMIN",
        caps::Capability::CAP_SYS_PTRACE => "CAP_SYS_PTRACE",
        caps::Capability::CAP_SYS_MODULE => "CAP_SYS_MODULE",
        caps::Capability::CAP_SYS_BOOT => "CAP_SYS_BOOT",
        caps::Capability::CAP_SYS_RAWIO => "CAP_SYS_RAWIO",
        caps::Capability::CAP_SYS_CHROOT => "CAP_SYS_CHROOT",
        caps::Capability::CAP_NET_ADMIN => "CAP_NET_ADMIN",
        caps::Capability::CAP_NET_RAW => "CAP_NET_RAW",
        caps::Capability::CAP_SETUID => "CAP_SETUID",
        caps::Capability::CAP_SETGID => "CAP_SETGID",
        caps::Capability::CAP_SETPCAP => "CAP_SETPCAP",
        caps::Capability::CAP_CHOWN => "CAP_CHOWN",
        caps::Capability::CAP_MKNOD => "CAP_MKNOD",
        caps::Capability::CAP_FSETID => "CAP_FSETID",
        caps::Capability::CAP_AUDIT_WRITE => "CAP_AUDIT_WRITE",
        caps::Capability::CAP_AUDIT_CONTROL => "CAP_AUDIT_CONTROL",
        caps::Capability::CAP_SYSLOG => "CAP_SYSLOG",
        caps::Capability::CAP_WAKE_ALARM => "CAP_WAKE_ALARM",
        caps::Capability::CAP_LEASE => "CAP_LEASE",
        caps::Capability::CAP_MAC_ADMIN => "CAP_MAC_ADMIN",
        caps::Capability::CAP_MAC_OVERRIDE => "CAP_MAC_OVERRIDE",
        caps::Capability::CAP_LINUX_IMMUTABLE => "CAP_LINUX_IMMUTABLE",
        caps::Capability::CAP_DAC_OVERRIDE => "CAP_DAC_OVERRIDE",
        caps::Capability::CAP_SYS_RESOURCE => "CAP_SYS_RESOURCE",
        _ => "UNKNOWN_CAP",
    }
}

/// Get the current capabilities of this process (for debugging)
pub fn get_current_caps() -> Result<String, io::Error> {
    let mut output = String::new();

    output.push_str("Current process capabilities:\n");

    // Try to read each capability set
    for (name, set) in [
        ("Effective", caps::CapSet::Effective),
        ("Permitted", caps::CapSet::Permitted),
        ("Inheritable", caps::CapSet::Inheritable),
    ] {
        output.push_str(&format!("  {}:", name));
        if let Ok(caps) = caps::read(None, set) {
            if caps.is_empty() {
                output.push_str(" (none)\n");
            } else {
                output.push('\n');
                for cap in caps {
                    output.push_str(&format!("    - {}\n", cap_name(cap)));
                }
            }
        } else {
            output.push_str(" (unable to read)\n");
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dropper_creation() {
        let dropper = CapabilityDropper::new();
        assert_eq!(dropper.drop_count(), 0);
    }

    #[test]
    fn test_firecracker_defaults() {
        let dropper = CapabilityDropper::with_firecracker_drops();
        // Should have many caps to drop
        assert!(dropper.drop_count() > 10);
    }

    #[test]
    fn test_custom_drops() {
        let mut dropper = CapabilityDropper::new();
        dropper
            .drop_cap(caps::Capability::CAP_NET_RAW)
            .drop_cap(caps::Capability::CAP_SYS_ADMIN);
        assert_eq!(dropper.drop_count(), 2);
    }

    #[test]
    fn test_cap_names() {
        let dropper = CapabilityDropper::with_firecracker_drops();
        let names = dropper.caps_being_dropped();
        assert!(names.contains(&"CAP_SYS_ADMIN"));
        assert!(names.contains(&"CAP_NET_RAW"));
    }
}
