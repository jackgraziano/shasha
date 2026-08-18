//! ARMv8 SHA-1 compression using the architecture's crypto extension.

use std::arch::aarch64::{
    uint32x4_t, vaddq_u32, vdupq_n_u32, vgetq_lane_u32, vld1q_u8, vld1q_u32, vreinterpretq_u32_u8,
    vrev32q_u8, vsha1cq_u32, vsha1h_u32, vsha1mq_u32, vsha1pq_u32, vsha1su0q_u32, vsha1su1q_u32,
    vst1q_u32,
};

/// Compress complete SHA-1 blocks.
///
/// # Safety
///
/// The caller must ensure the CPU supports the ARM SHA2 feature, which also
/// contains the SHA-1 instructions, and `bytes.len()` must be a multiple of 64.
#[target_feature(enable = "sha2")]
pub(crate) unsafe fn compress(state: &mut [u32; 5], bytes: &[u8]) {
    debug_assert_eq!(bytes.len() % 64, 0);

    for block in bytes.chunks_exact(64) {
        // SAFETY: state and each four-word schedule window are valid for the
        // vector loads/stores, and this function enables the required target
        // feature. Unaligned vector loads are supported on AArch64.
        let mut abcd = unsafe { vld1q_u32(state.as_ptr()) };
        let mut messages = [
            load_message(block, 0),
            load_message(block, 16),
            load_message(block, 32),
            load_message(block, 48),
        ];
        let saved_abcd = abcd;
        let saved_e = state[4];
        let mut e = saved_e;

        for group in 0..20 {
            let round = group * 4;
            let constant = match round {
                0..20 => 0x5a82_7999,
                20..40 => 0x6ed9_eba1,
                40..60 => 0x8f1b_bcdc,
                _ => 0xca62_c1d6,
            };
            let slot = group & 3;
            let words = messages[slot];
            let wk = vaddq_u32(words, vdupq_n_u32(constant));
            let next_e = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
            abcd = sha1_round(abcd, e, wk, round);
            e = next_e;

            if group < 16 {
                let first = vsha1su0q_u32(
                    messages[slot],
                    messages[(slot + 1) & 3],
                    messages[(slot + 2) & 3],
                );
                messages[slot] = vsha1su1q_u32(first, messages[(slot + 3) & 3]);
            }
        }

        abcd = vaddq_u32(abcd, saved_abcd);
        unsafe {
            vst1q_u32(state.as_mut_ptr(), abcd);
        }
        state[4] = e.wrapping_add(saved_e);
    }
}

#[inline]
#[target_feature(enable = "sha2")]
fn load_message(block: &[u8], offset: usize) -> uint32x4_t {
    let bytes = unsafe { vld1q_u8(block.as_ptr().add(offset)) };
    vreinterpretq_u32_u8(vrev32q_u8(bytes))
}

#[inline]
#[target_feature(enable = "sha2")]
fn sha1_round(abcd: uint32x4_t, e: u32, wk: uint32x4_t, round: usize) -> uint32x4_t {
    match round {
        0..20 => vsha1cq_u32(abcd, e, wk),
        20..40 | 60..80 => vsha1pq_u32(abcd, e, wk),
        _ => vsha1mq_u32(abcd, e, wk),
    }
}
