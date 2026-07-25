//! Guest memory: the one `vm-memory` type alias the whole crate uses
//! (no dirty-page bitmaps — this VMM takes no snapshots) and the
//! x86_64 region layout.

use vm_memory::{GuestAddress, GuestMemoryError};

use crate::arch::layout;

/// Guest memory map without dirty-bitmap tracking.
pub type GuestMemoryMmap = vm_memory::GuestMemoryMmap<()>;

/// Errors building guest memory.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// mmap-backed region creation failed.
    #[error("creating guest memory regions: {0}")]
    Create(#[from] vm_memory::mmap::FromRangesError),
    /// Guest memory access failed.
    #[error("guest memory access: {0}")]
    Access(#[from] GuestMemoryError),
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
