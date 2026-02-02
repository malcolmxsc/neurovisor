//! VM Pool - thread-safe pool of pre-warmed VMs
//!
//! The VMPool maintains a set of pre-warmed VMs ready for instant assignment.
//! When a request comes in, it acquires a VM from the pool. When done, the VM
//! is destroyed and the pool is replenished in the background.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  VM Pool                                                        │
//! │                                                                 │
//! │  ┌───────────────┐     ┌───────────────┐     ┌───────────────┐ │
//! │  │   VM Ready    │     │   VM Ready    │     │   VM Ready    │ │
//! │  │   (warm)      │     │   (warm)      │     │   (warm)      │ │
//! │  └───────────────┘     └───────────────┘     └───────────────┘ │
//! │         │                                                       │
//! │         ▼                                                       │
//! │    acquire() ──► VM assigned to request                        │
//! │         │                                                       │
//! │         ▼                                                       │
//! │    release() ──► VM destroyed, replenish triggered             │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Duration;

use super::handle::VMHandle;
use super::manager::VMManager;
use crate::metrics::{POOL_WARM_VMS, POOL_ACTIVE_VMS, VM_ACQUIRE_DURATION};

/// Statistics about the pool state
#[derive(Debug, Clone)]
pub struct PoolStats {
    /// Number of VMs in the warm pool (ready for assignment)
    pub warm_count: usize,
    /// Number of VMs currently active (assigned to requests)
    pub active_count: usize,
    /// Target warm pool size
    pub target_warm_size: usize,
    /// Maximum pool size
    pub max_pool_size: usize,
}

/// Thread-safe pool of pre-warmed VMs
pub struct VMPool {
    /// VM manager for creating/destroying VMs
    manager: Arc<VMManager>,
    /// Pool of warm (ready) VMs waiting for assignment
    warm_pool: Arc<Mutex<Vec<VMHandle>>>,
    /// Count of currently active VMs (for limiting)
    active_count: Arc<Mutex<usize>>,
    /// Target number of warm VMs to maintain
    target_warm_size: usize,
    /// Maximum total VMs (warm + active)
    max_pool_size: usize,
}

impl VMPool {
    /// Create a new VM pool
    ///
    /// # Arguments
    /// * `manager` - VMManager for creating/destroying VMs
    /// * `target_warm_size` - Number of VMs to keep pre-warmed (e.g., 3)
    /// * `max_pool_size` - Maximum total VMs allowed (e.g., 10)
    pub fn new(
        manager: Arc<VMManager>,
        target_warm_size: usize,
        max_pool_size: usize,
    ) -> Self {
        Self {
            manager,
            warm_pool: Arc::new(Mutex::new(Vec::with_capacity(target_warm_size))),
            active_count: Arc::new(Mutex::new(0)),
            target_warm_size,
            max_pool_size,
        }
    }

    /// Initialize the pool by pre-warming VMs
    ///
    /// Creates `target_warm_size` VMs at startup so they're ready for requests.
    pub async fn initialize(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("[INFO] PRE-WARMING {} VMs...", self.target_warm_size);

        for i in 0..self.target_warm_size {
            match self.manager.create_vm().await {
                Ok(handle) => {
                    let mut pool = self.warm_pool.lock().await;
                    pool.push(handle);
                    POOL_WARM_VMS.set(pool.len() as f64);
                    println!("[INFO]    Warm VM {}/{} ready", i + 1, self.target_warm_size);
                }
                Err(e) => {
                    eprintln!("[WARN] Failed to pre-warm VM {}: {}", i + 1, e);
                    // Continue trying to warm remaining VMs
                }
            }
        }

        let pool = self.warm_pool.lock().await;
        println!("[INFO] ✅ POOL INITIALIZED ({} warm VMs)", pool.len());
        Ok(())
    }

    /// Acquire a VM from the pool
    ///
    /// Returns a pre-warmed VM ready for use. If the pool is empty,
    /// returns an error (caller should retry or wait).
    pub async fn acquire(&self) -> Result<VMHandle, Box<dyn std::error::Error + Send + Sync>> {
        let start = Instant::now();

        // Check if we're at max capacity
        {
            let active = self.active_count.lock().await;
            let pool = self.warm_pool.lock().await;
            if *active + pool.len() >= self.max_pool_size && pool.is_empty() {
                return Err("Pool at maximum capacity, no VMs available".into());
            }
        }

        // Try to get a VM from the warm pool
        let handle = {
            let mut pool = self.warm_pool.lock().await;
            match pool.pop() {
                Some(mut vm) => {
                    vm.mark_active();
                    POOL_WARM_VMS.set(pool.len() as f64);
                    vm
                }
                None => {
                    return Err("No warm VMs available".into());
                }
            }
        };

        // Increment active count
        {
            let mut active = self.active_count.lock().await;
            *active += 1;
            POOL_ACTIVE_VMS.set(*active as f64);
        }

        let acquire_duration = start.elapsed();
        VM_ACQUIRE_DURATION.observe(acquire_duration.as_secs_f64());

        println!("[INFO] VM {} ACQUIRED (took {:.3}ms)", handle.vm_id, acquire_duration.as_secs_f64() * 1000.0);

        Ok(handle)
    }

    /// Release a VM back to the pool (destroys it and triggers replenish)
    ///
    /// VMs are not reused - they're destroyed after each request for isolation.
    /// The pool replenisher will create new warm VMs to replace them.
    pub async fn release(&self, handle: VMHandle) {
        let vm_id = handle.vm_id.clone();

        // Decrement active count
        {
            let mut active = self.active_count.lock().await;
            if *active > 0 {
                *active -= 1;
            }
            POOL_ACTIVE_VMS.set(*active as f64);
        }

        // Destroy the VM
        if let Err(e) = self.manager.destroy_vm(handle).await {
            eprintln!("[WARN] Failed to destroy VM {}: {}", vm_id, e);
        }

        println!("[INFO] VM {} RELEASED", vm_id);
    }

    /// Replenish the warm pool if below target
    ///
    /// Called periodically by the background replenisher task.
    pub async fn replenish(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Check if we need to create more VMs
        let (current_warm, current_active) = {
            let pool = self.warm_pool.lock().await;
            let active = self.active_count.lock().await;
            (pool.len(), *active)
        };

        // Don't exceed max pool size
        let total = current_warm + current_active;
        if total >= self.max_pool_size {
            return Ok(());
        }

        // Calculate how many VMs to create
        let needed = self.target_warm_size.saturating_sub(current_warm);
        let can_create = self.max_pool_size.saturating_sub(total);
        let to_create = needed.min(can_create);

        if to_create == 0 {
            return Ok(());
        }

        // Create new VMs
        for _ in 0..to_create {
            match self.manager.create_vm().await {
                Ok(handle) => {
                    let mut pool = self.warm_pool.lock().await;
                    pool.push(handle);
                    POOL_WARM_VMS.set(pool.len() as f64);
                }
                Err(e) => {
                    eprintln!("[WARN] Failed to create warm VM: {}", e);
                    // Don't fail the whole replenish, try again later
                    break;
                }
            }
        }

        Ok(())
    }

    /// Get current pool statistics
    pub async fn stats(&self) -> PoolStats {
        let pool = self.warm_pool.lock().await;
        let active = self.active_count.lock().await;

        PoolStats {
            warm_count: pool.len(),
            active_count: *active,
            target_warm_size: self.target_warm_size,
            max_pool_size: self.max_pool_size,
        }
    }

    /// Graceful shutdown: destroy all VMs in the pool
    pub async fn shutdown(&self) {
        println!("[INFO] SHUTTING DOWN VM POOL...");

        // Destroy all warm VMs
        let vms: Vec<VMHandle> = {
            let mut pool = self.warm_pool.lock().await;
            pool.drain(..).collect()
        };

        for handle in vms {
            if let Err(e) = self.manager.destroy_vm(handle).await {
                eprintln!("[WARN] Failed to destroy VM during shutdown: {}", e);
            }
        }

        POOL_WARM_VMS.set(0.0);
        println!("[INFO] ✅ VM POOL SHUTDOWN COMPLETE");
    }

    /// Start the background replenisher task
    ///
    /// This task runs periodically and ensures the warm pool stays at target size.
    pub fn start_replenisher(pool: Arc<VMPool>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));

            loop {
                interval.tick().await;

                if let Err(e) = pool.replenish().await {
                    eprintln!("[WARN] Pool replenish failed: {}", e);
                }
            }
        })
    }
}
