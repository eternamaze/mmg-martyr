#![allow(clippy::disallowed_types)]

use parking_lot::RwLock;
use slotmap::{new_key_type, SlotMap};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Weak};

// Define the key type internally, but don't expose it as the primary way to access.
new_key_type! { struct ResourceKey; }

/// Discipline defines how to handle violations (e.g., accessing a killed resource).
pub trait Discipline: Send + Sync + 'static {
    fn punish(action: &'static str) -> !;
}

/// Default discipline: Panic.
pub struct PanicDiscipline;

impl Discipline for PanicDiscipline {
    fn punish(action: &'static str) -> ! {
        panic!("🔥 [Martyr] Sovereign violation! Action: {}", action);
    }
}

/// Internal status of a resource.
struct ResourceStatus {
    visitor_count: AtomicIsize,
    is_killed: AtomicBool,
}

/// The cell holding the resource and its status.
struct SovereignCell<T> {
    instance: T,
    status: Arc<ResourceStatus>,
}

/// Internal storage.
struct RegistryInternal<T> {
    // Use Arc<SovereignCell> to allow access without holding the map lock for the entire duration.
    // This ensures force_kill can acquire the write lock immediately even if a visitor is looping.
    storage: RwLock<SlotMap<ResourceKey, Arc<SovereignCell<T>>>>,
}

/// The Sovereign container. Manages the lifecycle of resources `T`.
pub struct Sovereign<T, D: Discipline = PanicDiscipline> {
    internal: Arc<RegistryInternal<T>>,
    _marker: PhantomData<D>,
}

impl<T, D: Discipline> Default for Sovereign<T, D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, D: Discipline> Sovereign<T, D> {
    pub fn new() -> Self {
        Self {
            internal: Arc::new(RegistryInternal {
                storage: RwLock::new(SlotMap::with_key()),
            }),
            _marker: PhantomData,
        }
    }

    /// Register a resource and get a Lease (handle) to it.
    pub fn register(&self, resource: T) -> Lease<T, D> {
        let mut map = self.internal.storage.write();
        let key = map.insert(Arc::new(SovereignCell {
            instance: resource,
            status: Arc::new(ResourceStatus {
                visitor_count: AtomicIsize::new(0),
                is_killed: AtomicBool::new(false),
            }),
        }));

        Lease {
            registry: Arc::downgrade(&self.internal),
            key,
            _marker: PhantomData,
        }
    }

    /// Forcefully kill a resource associated with the given Lease.
    /// This will prevent future access and panic if there are active visitors.
    pub fn force_kill(&self, lease: &Lease<T, D>) {
        let mut map = self.internal.storage.write();
        
        // Remove the cell from the map immediately.
        // Even if visitors are holding Arc<Cell>, they can't prevent us from removing the entry.
        if let Some(cell) = map.remove(lease.key) {
            // 1. Signal Kill
            cell.status.is_killed.store(true, Ordering::SeqCst);

            // 2. Check for lingering visitors
            let visitors = cell.status.visitor_count.load(Ordering::SeqCst);
            if visitors > 0 {
                // Punishment: The visitor is still running (maybe in a loop).
                // We panic here to crash the thread/process.
                panic!("💥 [Martyr] Force kill executed! {} visitors lingering. System self-destruct.", visitors);
            }

            // 3. Resource logic drop.
            // Note: Since visitors might hold Arc<Cell>, the physical drop of T happens when the last visitor exits (or crashes).
            // But logically, it is killed.
            tracing::info!("✅ [Martyr] Resource killed.");
        }
    }
}

/// A Lease is a safe handle to a sovereign resource.
/// It does not own the resource, but allows controlled access.
pub struct Lease<T, D: Discipline = PanicDiscipline> {
    registry: Weak<RegistryInternal<T>>,
    key: ResourceKey,
    _marker: PhantomData<D>,
}

impl<T, D: Discipline> Clone for Lease<T, D> {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            key: self.key,
            _marker: PhantomData,
        }
    }
}

impl<T, D: Discipline> Lease<T, D> {
    /// Access the resource safely.
    /// The closure `f` is executed within a "Sentry" context.
    /// The resource reference `&T` cannot escape the closure.
    pub fn access<F, R>(&self, action: &'static str, f: F) -> Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        let registry = self.registry.upgrade().ok_or(AccessError::RegistryDropped)?;
        
        // 1. Get the cell. We only hold the read lock briefly to clone the Arc.
        let cell = {
            let map = registry.storage.read();
            map.get(self.key).cloned().ok_or(AccessError::ResourceNotFound)?
        };

        // 2. Check-in
        cell.status.visitor_count.fetch_add(1, Ordering::SeqCst);
        
        // RAII guard for Check-out
        let _guard = VisitorGuard {
            status: &cell.status,
        };

        // 3. Check if killed (Before execution)
        if cell.status.is_killed.load(Ordering::SeqCst) {
            D::punish(action);
        }

        // 4. Execute
        // Note: If force_kill happens during f(), it will set is_killed and panic.
        // But since we are in f(), we won't see the panic from force_kill thread unless force_kill thread panics the whole process.
        // However, force_kill WILL succeed in removing the key and detecting us.
        let result = f(&cell.instance);

        // 5. Check if killed (After execution - optional but good for detecting if we were killed during exec)
        if cell.status.is_killed.load(Ordering::SeqCst) {
             // If we survived the execution but were killed in the meantime, we should probably acknowledge it.
             // But strictly speaking, we finished successfully.
             // Let's stick to the entry check for now.
        }

        Ok(result)
    }
}


struct VisitorGuard<'a> {
    status: &'a ResourceStatus,
}

impl<'a> Drop for VisitorGuard<'a> {
    fn drop(&mut self) {
        self.status.visitor_count.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(thiserror::Error, Debug)]
pub enum AccessError {
    #[error("Sovereign registry has been dropped")]
    RegistryDropped,
    #[error("Resource not found or already killed")]
    ResourceNotFound,
}
