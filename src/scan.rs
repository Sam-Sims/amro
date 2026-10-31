use clap::ValueEnum;
use fearless_simd::{Simd, SimdBase, u8x16, u8x32};
use serde::Serialize;

const STANDARD: ByteSet = ByteSet::new(b"ATCGN");
const STANDARD_IN_ANY_CASE: ByteSet = ByteSet::new(b"ATCGNatcgn");
const IUPAC: ByteSet = ByteSet::new(b"ACGTURYSWKMBDHVN");
const IUPAC_IN_ANY_CASE: ByteSet = ByteSet::new(b"ACGTURYSWKMBDHVNacgturyswkmbdhvn");
// tab and printable ASCII
const HEADER: ByteSet = ByteSet::new(
    b"\t !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~",
);
// do not need full 256 byte validation so rest are stored as 0s and thus non ASCII is rejected
const BITMASK_LOOKUP: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 0, 0, 0, 0, 0, 0, 0, 0];

/// Set by `--alphabet`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, strum::Display)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum SequenceBytes {
    /// Uppercase A, C, G, T and N only.
    #[default]
    Standard,
    /// Uppercase IUPAC nucleotide codes (A C G T U R Y S W K M B D H V N).
    Iupac,
}

impl SequenceBytes {
    pub fn upper_case(self) -> ByteSet {
        match self {
            Self::Standard => STANDARD,
            Self::Iupac => IUPAC,
        }
    }

    pub fn any_case(self) -> ByteSet {
        match self {
            Self::Standard => STANDARD_IN_ANY_CASE,
            Self::Iupac => IUPAC_IN_ANY_CASE,
        }
    }
}

/// Set by `--quality-encoding`.
/// Display includes the valid range so the human report can show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, strum::Display)]
#[serde(rename_all = "kebab-case")]
pub enum QualityEncoding {
    /// Bytes ! to ~ (33 to 126)
    #[default]
    #[strum(to_string = "phred33 range ('!' to '~')")]
    Phred33,
    /// Bytes @ to ~ (64 to 126)
    #[strum(to_string = "phred64 range ('@' to '~')")]
    Phred64,
}

impl QualityEncoding {
    fn lowest(self) -> u8 {
        match self {
            Self::Phred33 => b'!',
            Self::Phred64 => b'@',
        }
    }

    #[inline(always)]
    pub fn find_invalid<S: Simd>(self, simd: S, quality: &[u8]) -> Option<usize> {
        let lowest = self.lowest();
        // if the field is shorter than 32 bytes cant go the SIMD route
        // in these cases found_invalid_byte is set to true so the normal scan runs
        let found_invalid_byte = match quality.last_chunk() {
            Some(tail) => {
                // split into 32byte chunks and go wide
                let (blocks, _) = quality.as_chunks();
                has_invalid_quality_byte(simd, blocks, tail, lowest)
            }
            None => true,
        };

        // the SIMD scan only tells us if there was an invalid byte
        // since most FASTQ files are going to be valid the SIMD is the faster path - and will
        // typically be the only one run, however a byte is invalid we need to actually find it
        // so we can report it, so a rescan is required. Since we fail fast on first error anyway
        // this at most happens once per file.
        if found_invalid_byte {
            // same shifted range idea as the SIMD version
            quality
                .iter()
                .position(|&byte| byte.wrapping_sub(lowest) > b'~' - lowest)
        } else {
            None
        }
    }
}

/// A set of ASCII bytes stored in a 16row bitmap
///
/// Implements the universal apporach described in
/// <http://0x80.pl/notesen/2018-10-18-simd-byte-lookup.html#universal-algorithm>
/// without the `bitmap_8_15` portion
#[derive(Debug, Clone, Copy)]
pub struct ByteSet {
    bitmap: [u8; 16],
}

impl ByteSet {
    const fn new(bytes: &[u8]) -> Self {
        let mut bitmap = [0; 16];
        let mut index = 0;

        while index < bytes.len() {
            let byte = bytes[index];
            assert!(byte.is_ascii());
            let lo_nibble = byte & 0x0f;
            let hi_nibble = byte >> 4;
            bitmap[lo_nibble as usize] |= BITMASK_LOOKUP[hi_nibble as usize];
            index += 1;
        }
        Self { bitmap }
    }

    pub fn contains(self, byte: u8) -> bool {
        let lo_nibble = byte & 0x0f;
        let hi_nibble = byte >> 4;
        let bitset = self.bitmap[usize::from(lo_nibble)];
        let bitmask = BITMASK_LOOKUP[usize::from(hi_nibble)];
        bitset & bitmask != 0
    }

    #[inline(always)]
    pub fn find_rejected<S: Simd>(self, simd: S, bytes: &[u8]) -> Option<usize> {
        // same logic as QualityEncoding::find_invalid()
        let found_invalid_byte = match bytes.last_chunk() {
            Some(tail) => {
                let (blocks, _) = bytes.as_chunks();
                has_rejected_byte(simd, &self.bitmap, blocks, tail)
            }
            None => true,
        };
        if found_invalid_byte {
            bytes.iter().position(|&byte| !self.contains(byte))
        } else {
            None
        }
    }
}

/// Returns the offset of the first header byte that is neither printable ASCII nor a tab.
#[inline(always)]
pub fn find_invalid_header_byte<S: Simd>(simd: S, header: &[u8]) -> Option<usize> {
    // headers can be shorter than one block so a short header padded with spaces takes
    // the SIMD path instead - and space is valid anyway
    let mut padded = [b' '; 32];
    let (blocks, tail): (&[[u8; 32]], &[u8; 32]) = if let Some(tail) = header.last_chunk() {
        (header.as_chunks().0, tail)
    } else {
        padded[..header.len()].copy_from_slice(header);
        (&[], &padded)
    };
    if has_rejected_byte(simd, &HEADER.bitmap, blocks, tail) {
        header.iter().position(|&byte| !HEADER.contains(byte))
    } else {
        None
    }
}

#[inline(always)]
fn has_invalid_quality_byte<S: Simd>(
    simd: S,
    blocks: &[[u8; 32]],
    tail: &[u8; 32],
    lowest: u8,
) -> bool {
    // subtracting the lowest possible byte shifts the valid ascii range
    // to 0-93 or 0-62 depending on phred encoding
    // so lows holds lowest valid byte and highs the highest one after the shift
    let lows = simd.splat_u8x32(lowest);
    let highs = simd.splat_u8x32(b'~' - lowest);

    // might as well set highest to the tail portion first instead of starting from 0
    // since will need to be checked anyway
    let mut highest = simd.sub_u8x32(u8x32::load_array_ref(simd, tail), lows);

    for block in blocks {
        // wrapping sub ensures that invalid bytes below the allowed range wrap around
        // to being above the range
        let shifted = simd.sub_u8x32(u8x32::load_array_ref(simd, block), lows);
        // storse the highest value seen so far and means we only need to do 1 comparison to check
        // if theyre valid at the end without checking in the loop
        highest = simd.max_u8x32(highest, shifted);
    }
    // a byte is invalid when shifted value is above highs - so if any lane is true the field
    // is invalid
    simd.any_true_mask8x32(simd.simd_gt_u8x32(highest, highs))
}

#[inline(always)]
fn has_rejected_byte<S: Simd>(
    simd: S,
    bitmap: &[u8; 16],
    blocks: &[[u8; 32]],
    tail: &[u8; 32],
) -> bool {
    // implments the universal approach outlined here in http://0x80.pl/notesen/2018-10-18-simd-byte-lookup.html
    // but only checking valid ASCII not 256 bytes
    let bitmap = u8x32::block_splat(u8x16::load_array_ref(simd, bitmap));
    let bitmask_lookup = u8x32::block_splat(u8x16::load_array_ref(simd, &BITMASK_LOOKUP));
    let low_bits = simd.splat_u8x32(0xf);

    let lookup = |input: &[u8; 32]| {
        let input = u8x32::load_array_ref(simd, input);
        let lo_nibble = simd.and_u8x32(input, low_bits);
        let hi_nibble = simd.shr_u8x32(input, 4);
        let bitset = simd.swizzle_dyn_within_blocks_u8x32(bitmap, lo_nibble);
        let bitmask = simd.swizzle_dyn_within_blocks_u8x32(bitmask_lookup, hi_nibble);
        simd.and_u8x32(bitset, bitmask)
    };

    let mut lowest = lookup(tail);
    for block in blocks {
        lowest = simd.min_u8x32(lowest, lookup(block));
    }
    simd.any_true_mask8x32(simd.simd_eq_u8x32(lowest, simd.splat_u8x32(0)))
}
