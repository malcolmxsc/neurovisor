//! Security policy definitions for eBPF LSM enforcement
//!
//! This module defines what file paths should be blocked for VM processes.

/// Default blocked paths for Firecracker VM processes.
///
/// These paths are sensitive and should never be accessible from
/// within a VM's Firecracker process context.
pub const DEFAULT_BLOCKED_PATHS: &[&str] = &[
    // Sensitive system files
    "/etc/shadow",
    "/etc/gshadow",
    "/etc/sudoers",

    // Kernel memory and debugging
    "/proc/kcore",
    "/proc/kmem",
    "/dev/mem",
    "/dev/kmem",
    "/dev/port",

    // System configuration that could leak info
    "/etc/ssh/ssh_host",  // SSH host keys (prefix match)
    "/root/.ssh",

    // Firecracker jailer paths (prevent escape attempts)
    "/run/firecracker",

    // Docker/container sockets (if present)
    "/var/run/docker.sock",
    "/run/containerd",
];

/// Security policy for eBPF LSM enforcement
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    /// Paths to block (exact match or prefix)
    pub blocked_paths: Vec<String>,
    /// Whether to log blocked attempts
    pub log_blocked: bool,
    /// Whether to actually enforce (false = audit mode)
    pub enforce: bool,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            blocked_paths: DEFAULT_BLOCKED_PATHS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            log_blocked: true,
            enforce: true,
        }
    }
}

impl SecurityPolicy {
    /// Create a new security policy with default blocked paths
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an audit-only policy (logs but doesn't block)
    pub fn audit_only() -> Self {
        Self {
            enforce: false,
            ..Self::default()
        }
    }

    /// Add a path to the blocked list
    pub fn block_path(&mut self, path: &str) -> &mut Self {
        self.blocked_paths.push(path.to_string());
        self
    }

    /// Remove a path from the blocked list
    pub fn allow_path(&mut self, path: &str) -> &mut Self {
        self.blocked_paths.retain(|p| p != path);
        self
    }

    /// Check if a path should be blocked
    pub fn is_blocked(&self, path: &str) -> bool {
        for blocked in &self.blocked_paths {
            // Prefix match
            if path.starts_with(blocked) {
                return true;
            }
        }
        false
    }

    /// Convert paths to fixed-size byte arrays for eBPF map
    pub fn paths_as_bytes(&self) -> Vec<[u8; 64]> {
        self.blocked_paths
            .iter()
            .map(|path| {
                let mut arr = [0u8; 64];
                let bytes = path.as_bytes();
                let len = bytes.len().min(64);
                arr[..len].copy_from_slice(&bytes[..len]);
                arr
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = SecurityPolicy::default();
        assert!(policy.is_blocked("/etc/shadow"));
        assert!(policy.is_blocked("/proc/kcore"));
        assert!(!policy.is_blocked("/etc/passwd"));
        assert!(!policy.is_blocked("/tmp/foo"));
    }

    #[test]
    fn test_prefix_match() {
        let policy = SecurityPolicy::default();
        // /etc/ssh/ssh_host is a prefix
        assert!(policy.is_blocked("/etc/ssh/ssh_host_rsa_key"));
        assert!(policy.is_blocked("/etc/ssh/ssh_host_ed25519_key"));
    }

    #[test]
    fn test_custom_policy() {
        let mut policy = SecurityPolicy::new();
        policy.block_path("/custom/blocked");

        assert!(policy.is_blocked("/custom/blocked"));
        assert!(policy.is_blocked("/custom/blocked/subdir"));
    }

    #[test]
    fn test_paths_as_bytes() {
        let mut policy = SecurityPolicy::new();
        policy.blocked_paths = vec!["/etc/shadow".to_string()];

        let bytes = policy.paths_as_bytes();
        assert_eq!(bytes.len(), 1);
        assert_eq!(&bytes[0][..11], b"/etc/shadow");
    }
}
