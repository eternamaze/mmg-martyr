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
    storage: RwLock<SlotMap<ResourceKey, SovereignCell<T>>>,
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
        let key = map.insert(SovereignCell {
            instance: resource,
            status: Arc::new(ResourceStatus {
                visitor_count: AtomicIsize::new(0),
                is_killed: AtomicBool::new(false),
            }),
        });

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
        
        if let Some(cell) = map.remove(lease.key) {
            // 1. Signal Kill
            cell.status.is_killed.store(true, Ordering::SeqCst);

            // 2. Check for lingering visitors
            let visitors = cell.status.visitor_count.load(Ordering::SeqCst);
            if visitors > 0 {
                // Punishment
                panic!("💥 [Martyr] Force kill failed cleanly! {} visitors lingering. System self-destruct.", visitors);
            }

            // 3. Resource is dropped here as `cell` goes out of scope.
            tracing::info!("✅ [Martyr] Resource killed and dropped.");
        }
    }
}

/// A Lease is a safe handle to a sovereign resource.
/// It does not own the resource, but allows controlled access.
#[derive(Clone)]
pub struct Lease<T, D: Discipline = PanicDiscipline> {
    registry: Weak<RegistryInternal<T>>,
    key: ResourceKey,
    _marker: PhantomData<D>,
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
        let map = registry.storage.read();
        let cell = map.get(self.key).ok_or(AccessError::ResourceNotFound)?;

        // 1. Check-in
        cell.status.visitor_count.fetch_add(1, Ordering::SeqCst);
        
        // RAII guard for Check-out
        let _guard = VisitorGuard {
            status: &cell.status,
        };

        // 2. Check if killed
        if cell.status.is_killed.load(Ordering::SeqCst) {
            D::punish(action);
        }

        // 3. Execute
        Ok(f(&cell.instance))
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
