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
//! No decompression step for the plain `Image` itself: unlike
//! x86_64's bzImage (a *compressed* payload the guest's own embedded
//! stub would normally decompress, which a direct-64-bit-entry boot
//! never runs -- see `crate::boot`'s own module docs), "the AArch64
//! kernel does not currently provide a decompressor" at all (ibid.)
//! -- a compressed `Image.gz` target requires the *bootloader* to
//! gunzip it first, but the plain `Image` form this module loads is
//! already directly executable as-is.
//!
//! One real wrinkle found booting actual distro kernel packages
//! rather than a bare `Image` file, though: `/boot/vmlinuz-*` as
//! shipped by real Ubuntu and CentOS Stream aarch64 kernel packages
//! turned out to be **EFI zboot**-wrapped (`CONFIG_EFI_ZBOOT`) -- a
//! small, valid PE/COFF "EFI application" (magic `"MZ"`, then `"zimg"`
//! at offset 4) whose own header names a compressed payload offset/
//! size/algorithm, meant to be decompressed by a real UEFI
//! bootloader's own EFI stub, not by a direct-kernel-boot VMM like
//! this one at all. [`unwrap_efi_zboot`] undoes exactly that
//! wrapping, so this backend can boot real, unmodified distro kernel
//! packages directly instead of requiring a pre-extracted plain
//! `Image` file prepared by hand. The named compression algorithm
//! itself isn't consistent either: CentOS Stream 10's own
//! `kernel-core` RPM uses `gzip`, Ubuntu's own current (26.04/
//! resolute) `linux-image-unsigned-*` `.deb` uses `zstd` instead
//! (its own *older*, 24.04/noble packaging used a bare `gzip` stream
//! with no zboot wrapping at all -- real distro packaging choices
//! keep changing release to release; both `unwrap_efi_zboot` and
//! [`load_image`]'s own bare-stream fallback handle whichever a given
//! package turns out to use). Also found, and worth calling out
//! explicitly: Ubuntu's *signed* kernel-image variant
//! (`linux-image-<ver>-generic`, what `linux-image-generic` actually
//! depends on, i.e. what a real `apt-get install linux-image-generic`
//! installs) ships a non-zboot, seemingly-broken `vmlinuz` on
//! `ports.ubuntu.com` specifically -- arm64 has no real Secure Boot
//! signing infrastructure the way amd64 does, so this looks like a
//! non-functional template artifact of that architecture, not
//! something this module should try to unwrap. The *unsigned*
//! variant (`linux-image-unsigned-<ver>-generic`) is the one that's
//! actually a real, bootable zboot image, and what
//! `ci/fetch-aarch64-kernel.sh` fetches for Ubuntu accordingly.

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

/// Errors parsing an `Image`'s header, or loading/unwrapping one.
#[derive(Debug, thiserror::Error)]
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
    /// An EFI zboot header was found, but its own `payload_offset`/
    /// `payload_size` fields don't fit within the actual file.
    #[error(
        "EFI zboot header names a payload [{offset:#x}, {offset:#x}+{size:#x}) past the end of a {file_len}-byte file"
    )]
    ZbootPayloadOutOfBounds {
        /// The header's own `payload_offset` field.
        offset: u32,
        /// The header's own `payload_size` field.
        size: u32,
        /// The actual length of the file.
        file_len: usize,
    },
    /// An EFI zboot header was found, naming a compression algorithm
    /// this module doesn't implement (only `gzip` and `zstd` are;
    /// every real aarch64 distro kernel package examined so far --
    /// CentOS Stream's own `gzip`, Ubuntu's own `zstd` -- uses one of
    /// the two).
    #[error(
        "EFI zboot payload uses unsupported compression {0:?} (only \"gzip\"/\"zstd\" are implemented)"
    )]
    UnsupportedZbootCompression(String),
    /// The EFI zboot payload claimed to be `gzip` but failed to
    /// decompress as one.
    #[error("EFI zboot gzip payload failed to decompress: {0}")]
    ZbootGzip(#[from] std::io::Error),
    /// The EFI zboot payload claimed to be `zstd` but failed to
    /// decompress as one.
    #[error("EFI zboot zstd payload failed to decompress: {0}")]
    ZbootZstd(String),
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

/// EFI zboot header magic: `"MZ"` (the shared PE/COFF-and-arm64-Image
/// magic both formats start with) followed by `"zimg"` at offset 4.
const ZBOOT_MAGIC: &[u8; 8] = b"MZ\0\0zimg";

/// The plain gzip magic (`RFC 1952`): real distro packaging isn't
/// consistent about *which* wrapping a `vmlinuz` file uses -- Ubuntu's
/// own aarch64 kernel package ships a plain gzip stream directly (no
/// EFI zboot header at all), while CentOS Stream's uses EFI zboot
/// (see [`unwrap_efi_zboot`]'s own docs) -- confirmed directly against
/// both projects' real packages, not assumed.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Loads a real arm64 kernel package's `vmlinuz`/`Image` file,
/// transparently undoing whichever wrapping (if any) it turns out to
/// use -- EFI zboot ([`unwrap_efi_zboot`]) or a bare gzip stream (both
/// confirmed necessary against real distro packages; see this
/// module's own docs). Returns the plain bytes [`ImageHeader::parse`]
/// expects, unmodified if `data` was already a plain `Image`.
pub fn load_image(data: &[u8]) -> Result<Vec<u8>, ImageError> {
    if let Some(unwrapped) = unwrap_efi_zboot(data)? {
        return Ok(unwrapped);
    }

    if data.len() >= 2 && data[0..2] == GZIP_MAGIC {
        let mut decoder = flate2::read::GzDecoder::new(data);
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut out)?;
        return Ok(out);
    }

    Ok(data.to_vec())
}

/// Undoes EFI zboot wrapping (see this module's own docs) if `data` is
/// zboot-wrapped at all. Returns `Ok(None)` (not an error) if `data`
/// doesn't start with the zboot magic -- the caller's own `data` is
/// presumably already a plain `Image` in that case, for
/// [`ImageHeader::parse`] to judge.
pub fn unwrap_efi_zboot(data: &[u8]) -> Result<Option<Vec<u8>>, ImageError> {
    if data.len() < 32 || &data[0..8] != ZBOOT_MAGIC {
        return Ok(None);
    }

    let payload_offset = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let payload_size = u32::from_le_bytes(data[12..16].try_into().unwrap());
    let compression = &data[24..32];
    let compression_str = compression[..compression
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(compression.len())]
        .to_vec();

    let start = payload_offset as usize;
    let end = start.saturating_add(payload_size as usize);
    let payload = data
        .get(start..end)
        .ok_or(ImageError::ZbootPayloadOutOfBounds {
            offset: payload_offset,
            size: payload_size,
            file_len: data.len(),
        })?;

    match compression_str.as_slice() {
        b"gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(payload);
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut out)?;
            Ok(Some(out))
        }
        // Ubuntu's own real aarch64 kernel package (confirmed against
        // the actual, current 26.04/resolute `linux-image-unsigned-*`
        // package -- see this module's own docs) uses this, not
        // `gzip`, despite `gzip` being what its own *older* (24.04/
        // noble) packaging used instead (as a bare stream, no zboot
        // wrapping at all then) -- real distro packaging choices
        // change across releases, so both are supported rather than
        // assuming either is the one true answer.
        b"zstd" => {
            let mut decoder = ruzstd::decoding::StreamingDecoder::new(payload)
                .map_err(|e| ImageError::ZbootZstd(e.to_string()))?;
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut out)
                .map_err(|e| ImageError::ZbootZstd(e.to_string()))?;
            Ok(Some(out))
        }
        _ => Err(ImageError::UnsupportedZbootCompression(
            String::from_utf8_lossy(&compression_str).into_owned(),
        )),
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
        assert!(matches!(err, ImageError::TooShort(63)));
    }

    #[test]
    fn rejects_a_bad_magic() {
        let bytes = header_bytes(0, 0, 0, 0xdead_beef);
        let err = ImageHeader::parse(&bytes).unwrap_err();
        assert!(matches!(err, ImageError::BadMagic(0xdead_beef)));
    }

    #[test]
    fn entry_address_adds_text_offset_to_an_aligned_base() {
        let header = ImageHeader::parse(&header_bytes(0, 0, 0, IMAGE_MAGIC)).unwrap();
        assert_eq!(header.entry_address(0x4000_0000), 0x4000_0000);

        let header = ImageHeader::parse(&header_bytes(0x8_0000, 0, 0, IMAGE_MAGIC)).unwrap();
        assert_eq!(header.entry_address(0x4000_0000), 0x4008_0000);
    }

    /// Builds a synthetic EFI-zboot-wrapped file around `inner`
    /// (gzip-compressed), matching the real header layout found in
    /// actual Ubuntu/CentOS Stream aarch64 kernel packages: `"MZ\0\0zimg"`
    /// magic, then `payload_offset`/`payload_size` (u32 LE), then 8
    /// reserved bytes, then an 8-byte NUL-padded compression-algorithm
    /// name, then arbitrary padding before the payload itself.
    fn zboot_wrap(inner: &[u8], compression: &[u8; 8]) -> Vec<u8> {
        use std::io::Write;

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(inner).unwrap();
        let payload = gz.finish().unwrap();

        let payload_offset: u32 = 64; // arbitrary padding before the payload, like the real format has.
        let mut wrapped = vec![0u8; payload_offset as usize];
        wrapped[0..8].copy_from_slice(ZBOOT_MAGIC);
        wrapped[8..12].copy_from_slice(&payload_offset.to_le_bytes());
        wrapped[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        wrapped[24..32].copy_from_slice(compression);
        wrapped.extend_from_slice(&payload);
        wrapped
    }

    #[test]
    fn unwrap_efi_zboot_recovers_the_original_gzip_payload() {
        let inner = header_bytes(0x8_0000, 0x0100_0000, 0, IMAGE_MAGIC);
        let wrapped = zboot_wrap(&inner, b"gzip\0\0\0\0");

        let unwrapped = unwrap_efi_zboot(&wrapped)
            .unwrap()
            .expect("should detect zboot wrapping");
        assert_eq!(unwrapped, inner);

        // load_image should transparently do the same thing.
        let loaded = load_image(&wrapped).unwrap();
        assert_eq!(loaded, inner);

        // ...and the result should parse as a normal Image header.
        let header = ImageHeader::parse(&unwrapped).unwrap();
        assert_eq!(header.text_offset, 0x8_0000);
    }

    #[test]
    fn load_image_also_unwraps_a_bare_gzip_stream() {
        // Matches Ubuntu's own real aarch64 kernel packaging: a plain
        // gzip stream, no EFI zboot header at all.
        use std::io::Write;
        let inner = header_bytes(0, 0, 0, IMAGE_MAGIC);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&inner).unwrap();
        let wrapped = gz.finish().unwrap();

        assert_eq!(load_image(&wrapped).unwrap(), inner);
    }

    #[test]
    fn unwrap_efi_zboot_returns_none_for_a_plain_image() {
        let plain = header_bytes(0, 0, 0, IMAGE_MAGIC);
        assert_eq!(unwrap_efi_zboot(&plain).unwrap(), None);

        // load_image should pass plain (non-zboot) data through unchanged.
        assert_eq!(load_image(&plain).unwrap(), plain);
    }

    #[test]
    fn unwrap_efi_zboot_recovers_a_zstd_compressed_payload() {
        // Matches Ubuntu's own current real aarch64 kernel packaging
        // (26.04/resolute's `linux-image-unsigned-*`): zboot-wrapped,
        // but with `zstd` rather than `gzip` compression.
        let inner = header_bytes(0x8_0000, 0x0100_0000, 0, IMAGE_MAGIC);
        let payload = ruzstd::encoding::compress_to_vec(
            inner.as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );

        let payload_offset: u32 = 64;
        let mut wrapped = vec![0u8; payload_offset as usize];
        wrapped[0..8].copy_from_slice(ZBOOT_MAGIC);
        wrapped[8..12].copy_from_slice(&payload_offset.to_le_bytes());
        wrapped[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        wrapped[24..32].copy_from_slice(b"zstd\0\0\0\0");
        wrapped.extend_from_slice(&payload);

        let unwrapped = unwrap_efi_zboot(&wrapped)
            .unwrap()
            .expect("should detect zboot wrapping");
        assert_eq!(unwrapped, inner);

        let loaded = load_image(&wrapped).unwrap();
        assert_eq!(loaded, inner);
    }

    #[test]
    fn unwrap_efi_zboot_rejects_unsupported_compression() {
        let inner = header_bytes(0, 0, 0, IMAGE_MAGIC);
        // Neither real compression this module implements (`gzip`,
        // `zstd`) -- `lz4` is a real zboot-supported option upstream
        // too (`CONFIG_KERNEL_LZ4`), just not one any real distro
        // package examined so far actually uses, so not implemented
        // here.
        let wrapped = zboot_wrap(&inner, b"lz4\0\0\0\0\0");
        let err = unwrap_efi_zboot(&wrapped).unwrap_err();
        assert!(matches!(err, ImageError::UnsupportedZbootCompression(ref s) if s == "lz4"));
    }

    #[test]
    fn unwrap_efi_zboot_rejects_an_out_of_bounds_payload() {
        let mut wrapped = vec![0u8; 64];
        wrapped[0..8].copy_from_slice(ZBOOT_MAGIC);
        wrapped[8..12].copy_from_slice(&64u32.to_le_bytes());
        wrapped[12..16].copy_from_slice(&0xffff_ffffu32.to_le_bytes()); // Absurdly large payload_size.
        wrapped[24..32].copy_from_slice(b"gzip\0\0\0\0");

        let err = unwrap_efi_zboot(&wrapped).unwrap_err();
        assert!(matches!(err, ImageError::ZbootPayloadOutOfBounds { .. }));
    }
}
