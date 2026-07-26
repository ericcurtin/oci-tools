//! Guest memory: the one `vm-memory` type alias the whole crate uses
//! (no dirty-page bitmaps — this VMM takes no snapshots) and the
//! x86_64 region layout.
//!
//! The [`GuestMemoryMmap`] alias itself has no x86_64-specific content
//! at all (`vm-memory`'s `backend-mmap` builds cleanly on macOS too)
//! and is left available on every target this crate builds `virtio`
//! for -- `hvf::virtio_mmio` uses it directly rather than a
//! second, duplicate alias. `ram_regions`/`create` (the x86_64 memory
//! *layout*, e.g. the 32-bit MMIO gap) are still Linux/x86_64-only,
//! since `hvf`'s own memory layout (`hvf::layout`) is entirely
//! different (see `docs/design/0249`).

/// Guest memory map without dirty-bitmap tracking.
pub type GuestMemoryMmap = vm_memory::GuestMemoryMmap<()>;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod x86_64_layout {
    use vm_memory::GuestAddress;

    use super::GuestMemoryMmap;
    use crate::arch::layout;

    /// Errors building guest memory.
    #[derive(Debug, thiserror::Error)]
    pub enum MemoryError {
        /// mmap-backed region creation failed.
        #[error("creating guest memory regions: {0}")]
        Create(#[from] vm_memory::mmap::FromRangesError),
        /// Guest memory access failed.
        #[error("guest memory access: {0}")]
        Access(#[from] vm_memory::GuestMemoryError),
    }

    /// The RAM regions for `mem_size` bytes of guest memory: everything
    /// below the 32-bit MMIO gap in one region, the remainder above 4 GiB
    /// — the gap itself belongs to PCI BARs, the IOAPIC, and the LAPICs.
    pub fn ram_regions(mem_size: usize) -> Vec<(GuestAddress, usize)> {
        let below_gap = layout::MMIO32_MEM_START as usize;
        if mem_size <= below_gap {
            vec![(GuestAddress(0), mem_size)]
        } else {
            vec![
                (GuestAddress(0), below_gap),
                (
                    GuestAddress(layout::FIRST_ADDR_PAST_32BITS),
                    mem_size - below_gap,
                ),
            ]
        }
    }

    /// Build the guest memory from [`ram_regions`].
    pub fn create(mem_size: usize) -> Result<GuestMemoryMmap, MemoryError> {
        let regions = ram_regions(mem_size);
        Ok(GuestMemoryMmap::from_ranges(&regions)?)
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use x86_64_layout::{MemoryError, create, ram_regions};
