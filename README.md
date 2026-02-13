# Martyr — Revocable Capability Guard

A single-pointer resource guard for Rust. One edge in, one kill to sever it.

## The Problem

`Arc<T>` gives every holder a pointer to `T`. Any holder can clone it, extend the lifetime indefinitely, and prevent cleanup. The resource's fate is decided by democratic consensus — the last `Arc` to drop wins.

**Martyr inverts this.** The resource has exactly one pointer to it, held inside `Martyr`. Everyone else points to the *shell* (`Arc<Martyr<T>>`), never to the resource itself. Any holder can `kill()` the resource unilaterally.

```text
Arc<T> (democracy):            Martyr<T> (dictatorship):

  A ─→ Arc ──┐                  A ─→ Arc ──┐
              ├──→ T                        ├──→ Martyr ──*mut T──→ T
  B ─→ Arc ──┘                  B ─→ Arc ──┘
                                             ↑
  Any holder can block           Only pointer to T.
  T's release.                   kill() severs it. T dies.
```

## Graph-Theoretic Model

Let `G = (V, E)` be the directed graph of heap memory. Resource `T` occupies subgraph `S ⊆ G`.

**Invariant**: exactly one edge enters `S` from outside — the `*mut T` inside `Martyr`.

- `kill()` nullifies the pointer → `S` becomes an isolated subgraph → `Box::from_raw` reclaims it.
- `invoke(f)` dereferences the pointer under a read lock. HRTB (`for<'a> FnOnce(&'a T) -> R`) ensures `&T` cannot escape the closure.
- `invoke_mut(f)` dereferences under a write lock with the same HRTB guarantee for `&mut T`.

## Usage

```rust
use mmg_martyr::Martyr;
use std::sync::Arc;

// Wrap a resource — T moves to heap, Martyr holds the only pointer.
let guard = Arc::new(Martyr::new(vec![1, 2, 3]));

// Access through closures. References cannot escape.
let sum = guard.invoke(|v| v.iter().sum::<i32>()).unwrap();
assert_eq!(sum, 6);

// Mutate through closures.
guard.invoke_mut(|v| v.push(4)).unwrap();

// Share the shell freely.
let clone = Arc::clone(&guard);
assert_eq!(clone.invoke(|v| v.len()).unwrap(), 4);

// Any holder can kill. Resource dies immediately.
guard.kill();
assert!(clone.invoke(|v| v.len()).is_err()); // ResourceKilled
```

## API

```rust
pub struct Martyr<T> { /* RwLock<*mut T> */ }

impl<T> Martyr<T> {
    /// Move T to heap. Establish the unique edge.
    pub fn new(resource: T) -> Self;

    /// Sever the unique edge. T is reclaimed immediately.
    /// Returns true if this call killed it, false if already dead.
    pub fn kill(&self) -> bool;

    /// Is the edge still intact?
    pub fn is_alive(&self) -> bool;

    /// Shared access through the unique edge. Read lock, concurrent.
    pub fn invoke<F, R>(&self, f: F) -> Result<R, ResourceKilled>
    where F: for<'a> FnOnce(&'a T) -> R;

    /// Exclusive access through the unique edge. Write lock, mutual exclusion.
    pub fn invoke_mut<F, R>(&self, f: F) -> Result<R, ResourceKilled>
    where F: for<'a> FnOnce(&'a mut T) -> R;
}

/// The unique edge has been severed. The resource subgraph is unreachable.
#[derive(Debug, PartialEq, thiserror::Error)]
#[error("resource killed")]
pub struct ResourceKilled;
```

## Safety

| Threat | Defense |
|--------|---------|
| Reference escape | HRTB `for<'a>` — compiler rejects captures |
| Data race | `RwLock` — shared reads, exclusive writes |
| Double free | `kill()` nullifies before freeing — panic-safe |
| Dangling pointer | Null check on every access — returns `Err(ResourceKilled)` |
| Internal leak | T's design contract (Turing-completeness boundary) |

The last row is the theoretical limit: if `T` internally leaks a pointer to a global (e.g., stuffs an `Arc::clone()` into a static), no type system in a Turing-complete language can prevent it. This is [Lampson's confinement problem](https://dl.acm.org/doi/10.1145/362375.362389) (1973). Martyr handles everything else.

## Why "Martyr"?

A martyr dies for what it protects. `Martyr<T>` shields `T` behind a single edge, serves every caller through closures, and when `kill()` is called — the resource dies with it, instantly and unconditionally.

## License

MIT
