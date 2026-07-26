// SPDX-License-Identifier: Apache-2.0

//! arm64 direct-kernel boot: the aarch64 analogue of `crate::boot`
//! (x86_64/KVM's Linux 64-bit boot protocol / `bzImage` unwrapping).
//!
//! arm64 Linux has no equivalent of the x86 boot-params/GDT/page-
//! table dance at all -- per the real kernel documentation
//! (`Documentation/arch/arm64/booting.rst`): the (already
//! decompressed) `Image` carries its own 64-byte header
//! (`text_offset`/`image_size`/`flags`, magic `"ARM\x64"`), is placed
//! `text_offset` bytes past a 2 MiB-aligned base and entered directly
//! with the device tree blob's address in `x0`, `x1`-`x3` zeroed, all
//! DAIF exception masks set, and the MMU off -- the last two already
//! true of every vCPU this backend creates (`hvf::vcpu` never touches
//! `SCTLR_EL1`, so it stays at its post-reset off state; `CPSR` is set
//! explicitly by every caller, matching the phase-2/3 smoke tests).
//!
//! No decompression step of this module's own: unlike x86_64's
//! bzImage (a *compressed* payload the guest's own embedded stub
//! would normally decompress, which a direct-64-bit-entry boot never
//! runs -- see `crate::boot`'s own module docs), "the AArch64 kernel
//! does not currently provide a decompressor" at all (ibid.) --
//! meaning a compressed `Image.gz` target requires the *bootloader*
//! to gunzip it first, but the plain `Image` target (what every stock
//! distro kernel package actually ships as `/boot/vmlinuz-*` on
//! aarch64, unlike some other packaging's `Image.gz`/EFI zboot
//! wrapper) is already the same plain, uncompressed, directly-
//! executable form this module loads as-is.

/// Required alignment of the 2 MiB-aligned base the `Image` is placed
/// `text_offset` bytes past.
const IMAGE_BASE_ALIGNMENT: u64 = 2 * 1024 * 1024;

/// The real, documented `magic` field value (`"ARM\x64"`, little
/// endian).
const IMAGE_MAGIC: u32 = 0x644d_5241;

/// Fields read directly out of the `Image`'s own 64-byte header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageHeader {
    /// Byte offset from a 2 MiB-aligned base address the `Image`
    /// itself must be loaded at.
    pub text_offset: u64,
    /// The `Image`'s own effective size, in bytes (may legitimately
    /// be `0` on kernels older than v3.17 -- see [`ImageHeader::parse`]).
    pub image_size: u64,
    /// The raw `flags` field (kernel endianness, page size hint,
    /// physical placement hint) -- not currently interpreted any
    /// further by this module; every distro kernel this project
    /// targets is little-endian (bit 0 clear), which is all this
    /// loader assumes.
    pub flags: u64,
}

/// Errors parsing an `Image`'s header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    /// Fewer than 64 bytes -- not even large enough to hold the
    /// header.
    #[error("Image is only {0} bytes, smaller than the 64-byte header")]
    TooShort(usize),
    /// The 64-byte header's own `magic` field wasn't `"ARM\x64"` --
    /// not a valid arm64 `Image` (e.g. still gzip- or EFI-zboot-
    /// wrapped; see this module's own docs on why that's out of
    /// scope here).
    #[error(
        "bad Image magic {0:#010x}, expected {IMAGE_MAGIC:#010x} (\"ARM\\x64\") -- not a plain, uncompressed arm64 Image"
    )]
    BadMagic(u32),
}

impl ImageHeader {
    /// Parses `image`'s own 64-byte header (`image` may be longer --
    /// only the header is read here).
    pub fn parse(image: &[u8]) -> Result<Self, ImageError> {
        if image.len() < 64 {
            return Err(ImageError::TooShort(image.len()));
        }

        let magic = u32::from_le_bytes(image[56..60].try_into().unwrap());
        if magic != IMAGE_MAGIC {
            return Err(ImageError::BadMagic(magic));
        }

        Ok(ImageHeader {
            text_offset: u64::from_le_bytes(image[8..16].try_into().unwrap()),
            image_size: u64::from_le_bytes(image[16..24].try_into().unwrap()),
            flags: u64::from_le_bytes(image[24..32].try_into().unwrap()),
        })
    }

    /// The guest physical address the `Image` must be loaded at and
    /// entered, given `ram_base` (rounded *down* to the required
    /// 2 MiB alignment first, matching the spec's own "2 MiB aligned
    /// base" wording -- this backend's own `layout::RAM_BASE` already
    /// satisfies it exactly, so this is a no-op in practice, not a
    /// silent behavior change).
    pub fn entry_address(&self, ram_base: u64) -> u64 {
        let aligned_base = ram_base - (ram_base % IMAGE_BASE_ALIGNMENT);
        aligned_base + self.text_offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(text_offset: u64, image_size: u64, flags: u64, magic: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; 64];
        bytes[0..4].copy_from_slice(&0x1234_5678u32.to_le_bytes()); // code0, unused
        bytes[4..8].copy_from_slice(&0x9abc_def0u32.to_le_bytes()); // code1, unused
        bytes[8..16].copy_from_slice(&text_offset.to_le_bytes());
        bytes[16..24].copy_from_slice(&image_size.to_le_bytes());
        bytes[24..32].copy_from_slice(&flags.to_le_bytes());
        bytes[56..60].copy_from_slice(&magic.to_le_bytes());
        bytes
    }

    #[test]
    fn parses_a_well_formed_header() {
        let bytes = header_bytes(0x8_0000, 0x0200_0000, 0xa, IMAGE_MAGIC);
        let header = ImageHeader::parse(&bytes).unwrap();
        assert_eq!(header.text_offset, 0x8_0000);
        assert_eq!(header.image_size, 0x0200_0000);
        assert_eq!(header.flags, 0xa);
    }

    #[test]
    fn rejects_a_short_buffer() {
        let err = ImageHeader::parse(&[0u8; 63]).unwrap_err();
        assert_eq!(err, ImageError::TooShort(63));
    }

    #[test]
    fn rejects_a_bad_magic() {
        let bytes = header_bytes(0, 0, 0, 0xdead_beef);
        let err = ImageHeader::parse(&bytes).unwrap_err();
        assert_eq!(err, ImageError::BadMagic(0xdead_beef));
    }

    #[test]
    fn entry_address_adds_text_offset_to_an_aligned_base() {
        let header = ImageHeader::parse(&header_bytes(0, 0, 0, IMAGE_MAGIC)).unwrap();
        assert_eq!(header.entry_address(0x4000_0000), 0x4000_0000);

        let header = ImageHeader::parse(&header_bytes(0x8_0000, 0, 0, IMAGE_MAGIC)).unwrap();
        assert_eq!(header.entry_address(0x4000_0000), 0x4008_0000);
    }
}
