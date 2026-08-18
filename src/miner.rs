use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use sha1::Sha1Core;
use sha1::digest::core_api::Block;
use sha2::Sha256VarCore;

use crate::git::ObjectFormat;

pub(crate) struct MineRequest<'a> {
    pub(crate) format: ObjectFormat,
    pub(crate) object_prefix: &'a [u8],
    pub(crate) target: u32,
    pub(crate) prefix_len: u8,
    pub(crate) threads: usize,
}

pub(crate) struct MineOutcome {
    pub(crate) nonce: u64,
    pub(crate) oid: String,
    pub(crate) attempts: u64,
    pub(crate) elapsed: Duration,
}

struct WorkerOutcome {
    attempts: u64,
    match_found: Option<(u64, [u32; 8])>,
}

pub(crate) fn mine(request: MineRequest<'_>) -> Result<MineOutcome> {
    let started = Instant::now();
    let (nonce, words, attempts) = match request.format {
        ObjectFormat::Sha1 => mine_prepared(PreparedSha1::new(request.object_prefix), &request)?,
        ObjectFormat::Sha256 => {
            mine_prepared(PreparedSha256::new(request.object_prefix), &request)?
        }
    };
    let digest = words_to_bytes(&words, request.format);
    Ok(MineOutcome {
        nonce,
        oid: hex_encode(&digest),
        attempts,
        elapsed: started.elapsed(),
    })
}

trait PreparedHash: Clone + Send {
    fn hash_nonce(&mut self, nonce: u64) -> [u32; 8];
}

fn mine_prepared<H>(prepared: H, request: &MineRequest<'_>) -> Result<(u64, [u32; 8], u64)>
where
    H: PreparedHash,
{
    let stopped = AtomicBool::new(false);
    let workers = request.threads;

    let outcomes = thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for worker in 0..workers {
            let mut worker_hash = prepared.clone();
            let stopped = &stopped;
            handles.push(scope.spawn(move || {
                let mut nonce = worker as u64;
                let step = workers as u64;
                let mut attempts = 0_u64;
                let mut match_found = None;

                'search: loop {
                    // Amortize cache-coherent reads of the stop flag. Once a
                    // peer wins, at most 256 extra hashes are computed here.
                    for _ in 0..256 {
                        let words = worker_hash.hash_nonce(nonce);
                        attempts += 1;

                        if prefix_matches(words[0], request.target, request.prefix_len)
                            && stopped
                                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                                .is_ok()
                        {
                            match_found = Some((nonce, words));
                            break 'search;
                        }

                        let Some(next) = nonce.checked_add(step) else {
                            break 'search;
                        };
                        nonce = next;
                    }
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                }

                WorkerOutcome {
                    attempts,
                    match_found,
                }
            }));
        }

        handles
            .into_iter()
            .map(|handle| handle.join().expect("a mining worker panicked"))
            .collect::<Vec<_>>()
    });

    let attempts = outcomes.iter().map(|outcome| outcome.attempts).sum();
    let winner = outcomes.into_iter().find_map(|outcome| outcome.match_found);

    match winner {
        Some((nonce, digest)) => Ok((nonce, digest, attempts)),
        None => bail!("the nonce space was exhausted without finding a matching commit"),
    }
}

#[derive(Clone)]
struct PreparedSha1 {
    state: [u32; 5],
    tail: PreparedTail,
}

impl PreparedSha1 {
    fn new(prefix: &[u8]) -> Self {
        let mut state = [
            0x6745_2301,
            0xefcd_ab89,
            0x98ba_dcfe,
            0x1032_5476,
            0xc3d2_e1f0,
        ];
        let complete_len = prefix.len() / 64 * 64;
        compress_sha1(&mut state, &prefix[..complete_len]);
        Self {
            state,
            tail: PreparedTail::new(&prefix[complete_len..], prefix.len()),
        }
    }
}

impl PreparedHash for PreparedSha1 {
    #[inline]
    fn hash_nonce(&mut self, nonce: u64) -> [u32; 8] {
        self.tail.set_nonce(nonce);
        let mut state = self.state;
        compress_sha1(
            &mut state,
            &self.tail.blocks_as_bytes()[..self.tail.block_count * 64],
        );
        let mut output = [0_u32; 8];
        output[..5].copy_from_slice(&state);
        output
    }
}

#[derive(Clone)]
struct PreparedSha256 {
    state: [u32; 8],
    tail: PreparedTail,
}

impl PreparedSha256 {
    fn new(prefix: &[u8]) -> Self {
        let mut state = [
            0x6a09_e667,
            0xbb67_ae85,
            0x3c6e_f372,
            0xa54f_f53a,
            0x510e_527f,
            0x9b05_688c,
            0x1f83_d9ab,
            0x5be0_cd19,
        ];
        let complete_len = prefix.len() / 64 * 64;
        sha2::compress256(&mut state, sha256_blocks(&prefix[..complete_len]));
        Self {
            state,
            tail: PreparedTail::new(&prefix[complete_len..], prefix.len()),
        }
    }
}

impl PreparedHash for PreparedSha256 {
    #[inline]
    fn hash_nonce(&mut self, nonce: u64) -> [u32; 8] {
        self.tail.set_nonce(nonce);
        let mut state = self.state;
        sha2::compress256(
            &mut state,
            sha256_blocks(&self.tail.blocks_as_bytes()[..self.tail.block_count * 64]),
        );
        state
    }
}

#[derive(Clone)]
struct PreparedTail {
    blocks: [[u8; 64]; 2],
    block_count: usize,
    nonce_offset: usize,
}

impl PreparedTail {
    fn new(prefix_tail: &[u8], prefix_len: usize) -> Self {
        debug_assert!(prefix_tail.len() < 64);
        let message_len = prefix_len + 17;
        let used = prefix_tail.len() + 17;
        let block_count = if used + 9 <= 64 { 1 } else { 2 };
        let mut blocks = [[0_u8; 64]; 2];
        blocks[0][..prefix_tail.len()].copy_from_slice(prefix_tail);
        set_tail_byte(&mut blocks, prefix_tail.len() + 16, b'\n');
        set_tail_byte(&mut blocks, used, 0x80);
        blocks[block_count - 1][56..].copy_from_slice(&((message_len as u64) * 8).to_be_bytes());
        Self {
            blocks,
            block_count,
            nonce_offset: prefix_tail.len(),
        }
    }

    #[inline]
    fn set_nonce(&mut self, nonce: u64) {
        let encoded = encode_nonce(nonce);
        let nonce_offset = self.nonce_offset;
        let bytes = self.blocks_as_bytes_mut();
        bytes[nonce_offset..nonce_offset + encoded.len()].copy_from_slice(&encoded);
    }

    #[inline]
    fn blocks_as_bytes(&self) -> &[u8; 128] {
        // SAFETY: an array of two 64-byte arrays has exactly the same layout
        // and alignment as one 128-byte array.
        unsafe { &*std::ptr::from_ref(&self.blocks).cast() }
    }

    #[inline]
    fn blocks_as_bytes_mut(&mut self) -> &mut [u8; 128] {
        // SAFETY: see `blocks_as_bytes`; mutable access is exclusive here.
        unsafe { &mut *std::ptr::from_mut(&mut self.blocks).cast() }
    }
}

#[inline]
fn sha1_blocks(bytes: &[u8]) -> &[Block<Sha1Core>] {
    debug_assert_eq!(bytes.len() % 64, 0);
    // SAFETY: RustCrypto defines `Block<Sha1Core>` as a transparent 64-byte
    // byte array. The crate's compression API performs the inverse cast.
    unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast(), bytes.len() / 64) }
}

#[inline]
fn compress_sha1(state: &mut [u32; 5], bytes: &[u8]) {
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("sha2") {
        // SAFETY: runtime feature detection above guarantees that the SHA-1
        // instructions used by this backend are available.
        unsafe {
            crate::sha1_arm::compress(state, bytes);
        }
        return;
    }

    sha1::compress(state, sha1_blocks(bytes));
}

#[inline]
fn sha256_blocks(bytes: &[u8]) -> &[Block<Sha256VarCore>] {
    debug_assert_eq!(bytes.len() % 64, 0);
    // SAFETY: `Block<Sha256VarCore>` is likewise a transparent 64-byte array.
    unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast(), bytes.len() / 64) }
}

#[inline]
fn set_tail_byte(blocks: &mut [[u8; 64]; 2], offset: usize, value: u8) {
    blocks[offset / 64][offset % 64] = value;
}

fn prefix_matches(first_word: u32, target: u32, prefix_len: u8) -> bool {
    first_word >> (32 - u32::from(prefix_len) * 4) == target
}

fn words_to_bytes(words: &[u32; 8], format: ObjectFormat) -> Vec<u8> {
    let word_count = match format {
        ObjectFormat::Sha1 => 5,
        ObjectFormat::Sha256 => 8,
    };
    let mut bytes = Vec::with_capacity(word_count * 4);
    for word in &words[..word_count] {
        bytes.extend_from_slice(&word.to_be_bytes());
    }
    bytes
}

pub(crate) fn encode_nonce(nonce: u64) -> [u8; 16] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = [0_u8; 16];
    for byte_index in 0..8 {
        let shift = (7 - byte_index) * 8;
        let byte = ((nonce >> shift) & 0xff) as usize;
        encoded[byte_index * 2] = HEX[byte >> 4];
        encoded[byte_index * 2 + 1] = HEX[byte & 0x0f];
    }
    encoded
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_encoding_is_fixed_width_hex() {
        assert_eq!(&encode_nonce(0), b"0000000000000000");
        assert_eq!(&encode_nonce(0x1234_abcd), b"000000001234abcd");
        assert_eq!(&encode_nonce(u64::MAX), b"ffffffffffffffff");
    }

    #[test]
    fn matching_uses_the_requested_leading_nibbles() {
        assert!(prefix_matches(0xabcd_ef12, 0xabcde, 5));
        assert!(!prefix_matches(0xabcd_ef12, 0xabcdf, 5));
        assert!(prefix_matches(0xabcd_ef12, 0xa, 1));
    }

    #[test]
    fn prepared_hashes_match_standard_digest_implementations() {
        use sha1::Digest;

        for prefix_len in [0, 1, 46, 47, 63, 64, 65, 110, 111, 127, 128, 250] {
            let prefix = vec![b'x'; prefix_len];
            for nonce in [0, 1, 0x1234_abcd, u64::MAX] {
                let mut message = prefix.clone();
                message.extend_from_slice(&encode_nonce(nonce));
                message.push(b'\n');

                let sha1_words = PreparedSha1::new(&prefix).hash_nonce(nonce);
                assert_eq!(
                    words_to_bytes(&sha1_words, ObjectFormat::Sha1),
                    sha1::Sha1::digest(&message).as_slice()
                );

                let sha256_words = PreparedSha256::new(&prefix).hash_nonce(nonce);
                assert_eq!(
                    words_to_bytes(&sha256_words, ObjectFormat::Sha256),
                    sha2::Sha256::digest(&message).as_slice()
                );
            }
        }
    }

    #[test]
    fn mines_a_small_sha1_prefix() {
        let outcome = mine(MineRequest {
            format: ObjectFormat::Sha1,
            object_prefix: b"commit 17\0nonce goes here ",
            target: 0xa,
            prefix_len: 1,
            threads: 2,
        })
        .unwrap();
        assert!(outcome.oid.starts_with('a'));
    }

    #[test]
    fn mines_a_small_sha256_prefix() {
        let outcome = mine(MineRequest {
            format: ObjectFormat::Sha256,
            object_prefix: b"commit 17\0nonce goes here ",
            target: 0xb,
            prefix_len: 1,
            threads: 2,
        })
        .unwrap();
        assert!(outcome.oid.starts_with('b'));
    }
}
