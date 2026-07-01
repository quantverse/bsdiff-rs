#![allow(non_snake_case)]
/*-
 * Copyright 2003-2005 Colin Percival
 * Copyright 2012 Matthew Endsley
 * Modified 2017 Pieter-Jan Briers
 * All rights reserved
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted providing that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
 * ARE DISCLAIMED.  IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
 * DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS
 * OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
 * HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT,
 * STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING
 * IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
 * POSSIBILITY OF SUCH DAMAGE.
 */

use std::io;
use std::io::Write;

/// Diff an "old" and a "new" file, returning a patch.
///
/// The patch can be applied to the "old" file to return the new file, with `patch::patch()`.
/// `old` and `new` correspond to the "old" and "new" file respectively. The patch will be written into `writer`.
/// # Generic Parameters
///
/// * `T: Read` - Any readable source for patch data (e.g., `File`, `Cursor<Vec<u8>>`, `&[u8]`)
/// * `W: Write + DerefMut<Target = [u8]>` - Any writable buffer that can be treated as a mutable byte slice
///   (e.g., `Vec<u8>`, `AlignedVec`, `SmallVec`, custom buffer types)
///
/// # Examples
///
/// ## Basic usage with Vec<u8>
///
/// ```rust
/// use bsdiff::{diff, patch};
///
/// // Create some test data
/// let old_data = b"Hello, world!";
/// let new_data = b"Hello, Rust!";
///
/// // Generate a patch
/// let mut patch_data = Vec::new();
/// diff(old_data, new_data, &mut patch_data)?;
///
/// // Apply the patch to reconstruct the new data
/// let mut reconstructed = Vec::new();
/// patch(old_data, &mut patch_data.as_slice(), &mut reconstructed)?;
///
/// assert_eq!(reconstructed, new_data);
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// ## Usage with custom buffer types
///
/// The function works with any type that implements `Write + DerefMut<Target = [u8]>`:
///
/// ```rust
/// use bsdiff::{patch, diff};
/// use std::ops::DerefMut;
///
/// // Create some test data
/// let old_data = b"Hello, world!";
/// let new_data = b"Hello, Rust!";
///
/// // Generate a patch
/// let mut patch_data = Vec::new();
/// diff(old_data, new_data, &mut patch_data)?;
///
/// // Apply the patch to reconstruct the new data
/// let mut reconstructed: smallvec::SmallVec<[_; 1024]> = smallvec::smallvec![];
/// patch(old_data, &mut patch_data.as_slice(), &mut reconstructed)?;
///
/// assert_eq!(reconstructed.as_slice(), new_data);
/// // The function also works with other buffer types like AlignedVec
/// // or any custom type that implements the required traits
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn diff<T: Write>(old: &[u8], new: &[u8], writer: &mut T) -> io::Result<()> {
    bsdiff_internal(old, new, writer)
}

#[inline]
fn usz(i: isize) -> usize {
    debug_assert!(i >= 0);
    i as usize
}

fn matchlen(old: &[u8], new: &[u8]) -> usize {
    old.iter().zip(new).take_while(|(a, b)| a == b).count()
}

/// Number of leading bytes hashed to form a match anchor.
const HASH_LEN: usize = 8;
/// log2 of the hash-table size (number of buckets).
const HASH_BITS: u32 = 21;
/// Upper bound on the number of candidates inspected per lookup, to keep the worst
/// case (highly repetitive regions) bounded.
const MAX_CHAIN: usize = 32;

#[inline]
fn hash8(bytes: &[u8]) -> usize {
    // bytes.len() >= HASH_LEN is guaranteed by callers.
    let x = u64::from_le_bytes(bytes[..HASH_LEN].try_into().unwrap());
    (x.wrapping_mul(0x9E37_79B1_85EB_CA87) >> (64 - HASH_BITS)) as usize
}

/// A hash-chain match finder over `old`, replacing the suffix array.
///
/// Every position `i` of `old` is indexed by the hash of its first [`HASH_LEN`] bytes.
/// `head[bucket]` points at the most recent position with that hash and `prev[i]` links
/// to the previous one, so a lookup walks a bucket's chain (capped at [`MAX_CHAIN`]).
///
/// This finds *good* matches, not provably longest ones like a suffix array would, but
/// every returned match is verified byte-for-byte, so patches always round-trip. Building
/// it is far cheaper than a suffix array (a single linear pass, no sorting), which is what
/// makes sub-100ms diffs of multi-megabyte inputs possible.
struct Matcher {
    head: Vec<i32>,
    prev: Vec<i32>,
}

impl Matcher {
    fn build(old: &[u8]) -> Self {
        assert!(
            old.len() <= i32::MAX as usize,
            "bsdiff: inputs larger than 2 GiB are not supported"
        );
        let mut head = vec![-1i32; 1usize << HASH_BITS];
        let mut prev = vec![-1i32; old.len()];
        if old.len() >= HASH_LEN {
            let m = old.len() - HASH_LEN + 1;
            Self::index_positions(old, m, &mut head, &mut prev);
        }
        Matcher { head, prev }
    }

    /// Insert positions `0..m` of `old` into the hash chains.
    #[cfg(feature = "parallel")]
    fn index_positions(old: &[u8], m: usize, head: &mut [i32], prev: &mut [i32]) {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicI32, Ordering};
        // Atomic views over buffers we own exclusively. Building the chains in parallel
        // makes the order within a bucket nondeterministic, which only affects *which*
        // equally-good candidate is found first, never correctness.
        let head_a: &[AtomicI32] = unsafe { &*(head as *const [i32] as *const [AtomicI32]) };
        let prev_a: &[AtomicI32] = unsafe { &*(prev as *const [i32] as *const [AtomicI32]) };
        (0..m).into_par_iter().for_each(|i| {
            let b = hash8(&old[i..]);
            let old_head = head_a[b].swap(i as i32, Ordering::Relaxed);
            prev_a[i].store(old_head, Ordering::Relaxed);
        });
    }

    #[cfg(not(feature = "parallel"))]
    fn index_positions(old: &[u8], m: usize, head: &mut [i32], prev: &mut [i32]) {
        for i in 0..m {
            let b = hash8(&old[i..]);
            prev[i] = head[b];
            head[b] = i as i32;
        }
    }

    /// Find a long match for `new[scan..]` within `old`, returning `(pos, len)` where
    /// `old[pos..pos + len] == new[scan..scan + len]`. Returns `(0, 0)` when no anchor
    /// matches (the byte is then emitted as a literal by the caller).
    #[inline]
    fn longest_match(&self, old: &[u8], new: &[u8], scan: usize) -> (usize, usize) {
        if scan + HASH_LEN > new.len() {
            return (0, 0);
        }
        let target = &new[scan..];
        let b = hash8(target);
        let mut p = self.head[b];
        let mut best_pos = 0usize;
        let mut best_len = 0usize;
        let mut chain = 0;
        while p >= 0 && chain < MAX_CHAIN {
            let pos = p as usize;
            // Prune candidates that cannot extend past the best match found so far.
            if best_len < target.len()
                && (best_len == 0
                    || (pos + best_len < old.len() && old[pos + best_len] == target[best_len]))
            {
                let l = matchlen(&old[pos..], target);
                if l > best_len {
                    best_len = l;
                    best_pos = pos;
                    if best_len == target.len() {
                        break;
                    }
                }
            }
            p = self.prev[pos];
            chain += 1;
        }
        (best_pos, best_len)
    }
}

#[inline]
fn offtout(x: isize, buf: &mut [u8]) {
    // so it works on 32-bit platforms
    let x64 = x as i64;
    if x64 >= 0 {
        buf.copy_from_slice(&x64.to_le_bytes());
    } else {
        let tmp = (-x64) as u64 | (1u64 << 63);
        buf.copy_from_slice(&tmp.to_le_bytes());
    }
}

/// Diff the whole of `new` against `old` and write the resulting patch records into `writer`.
///
/// The records are self-contained: applying them with a decoder whose `oldpos` starts at 0
/// reproduces `new` exactly. The return value is the decoder's `oldpos` *after* the last
/// record (still assuming it started at 0). That value is what the parallel stitcher uses to
/// emit a "seek back to 0" record between independently-diffed chunks.
fn diff_scan(old: &[u8], new: &[u8], matcher: &Matcher, writer: &mut dyn Write) -> io::Result<i64> {
    let mut buffer = Vec::new();
    // Tracks the decoder's `oldpos` as records are emitted, starting from 0.
    let mut decoder_oldpos: i64 = 0;

    let mut scan = 0;
    let mut len = 0usize;
    let mut pos = 0usize;
    let mut lastscan = 0;
    let mut lastpos = 0;
    let mut lastoffset = 0isize;
    while scan < new.len() {
        let mut oldscore = 0;
        scan += len;
        let mut scsc = scan;
        while scan < new.len() {
            let (p, l) = matcher.longest_match(old, new, scan);
            pos = p;
            len = l;
            while scsc < scan + len {
                if scsc as isize + lastoffset < old.len() as _
                    && (old[usz(scsc as isize + lastoffset)] == new[scsc])
                {
                    oldscore += 1;
                }
                scsc += 1;
            }
            if len == oldscore && (len != 0) || len > oldscore + 8 {
                break;
            }
            if scan as isize + lastoffset < old.len() as _
                && (old[usz(scan as isize + lastoffset)] == new[scan])
            {
                oldscore -= 1;
            }
            scan += 1;
        }
        if !(len != oldscore || scan == new.len()) {
            continue;
        }
        let mut s = 0;
        let mut Sf = 0;
        let mut lenf = 0usize;
        let mut i = 0usize;
        while lastscan + i < scan && (lastpos + i < old.len() as _) {
            if old[lastpos + i] == new[lastscan + i] {
                s += 1;
            }
            i += 1;
            if s * 2 - i as isize <= Sf * 2 - lenf as isize {
                continue;
            }
            Sf = s;
            lenf = i;
        }
        let mut lenb = 0;
        if scan < new.len() {
            let mut s = 0isize;
            let mut Sb = 0;
            let mut i = 1;
            while scan >= lastscan + i && (pos >= i) {
                if old[pos - i] == new[scan - i] {
                    s += 1;
                }
                if s * 2 - i as isize > Sb * 2 - lenb as isize {
                    Sb = s;
                    lenb = i;
                }
                i += 1;
            }
        }
        if lastscan + lenf > scan - lenb {
            let overlap = lastscan + lenf - (scan - lenb);
            let mut s = 0;
            let mut Ss = 0;
            let mut lens = 0;
            for i in 0..overlap {
                if new[lastscan + lenf - overlap + i] == old[lastpos + lenf - overlap + i] {
                    s += 1;
                }
                if new[scan - lenb + i] == old[pos - lenb + i] {
                    s -= 1;
                }
                if s > Ss {
                    Ss = s;
                    lens = i + 1;
                }
            }
            lenf = lenf + lens - overlap;
            lenb -= lens;
        }
        let seek = pos as isize - lenb as isize - (lastpos + lenf) as isize;
        let mut buf: [u8; 24] = [0; 24];
        offtout(lenf as _, &mut buf[..8]);
        offtout(
            scan as isize - lenb as isize - (lastscan + lenf) as isize,
            &mut buf[8..16],
        );
        offtout(seek, &mut buf[16..24]);
        writer.write_all(&buf[..24])?;

        buffer.clear();
        buffer.extend(
            new[lastscan..lastscan + lenf]
                .iter()
                .zip(&old[lastpos..lastpos + lenf])
                .map(|(n, o)| n.wrapping_sub(*o)),
        );
        writer.write_all(&buffer)?;

        let write_len = scan - lenb - (lastscan + lenf);
        let write_start = lastscan + lenf;
        writer.write_all(&new[write_start..write_start + write_len])?;

        // Mirror the decoder's `oldpos` update for this record: `+= mix_len` then `+= seek`.
        decoder_oldpos += lenf as i64 + seek as i64;

        lastscan = scan - lenb;
        lastpos = pos - lenb;
        lastoffset = pos as isize - scan as isize;
    }

    Ok(decoder_oldpos)
}

/// Emit a control record that consumes no data and moves the decoder's `oldpos` by `seek`.
#[cfg(feature = "parallel")]
fn write_seek_record(writer: &mut dyn Write, seek: isize) -> io::Result<()> {
    let mut buf = [0u8; 24];
    offtout(0, &mut buf[..8]);
    offtout(0, &mut buf[8..16]);
    offtout(seek, &mut buf[16..24]);
    writer.write_all(&buf)
}

/// Parallel scan: split `new` into contiguous chunks, diff each chunk independently against
/// `old` (on its own thread), then stitch the resulting sub-patches back into one stream.
///
/// Each chunk's records assume the decoder starts at `oldpos == 0`, so before every chunk
/// after the first we inject a zero-length record that seeks `oldpos` back to 0. The result
/// reconstructs `new` byte-for-byte; the only cost of chunking is slightly reduced compression
/// (cross-chunk match offsets are not shared), never correctness.
#[cfg(feature = "parallel")]
fn parallel_scan(old: &[u8], new: &[u8], matcher: &Matcher, writer: &mut dyn Write) -> io::Result<()> {
    use rayon::prelude::*;

    let n = new.len();
    if n == 0 {
        return Ok(());
    }

    // Keep chunks large enough that per-chunk overhead stays negligible, but small enough
    // to give every worker several chunks for load balancing.
    const MIN_CHUNK: usize = 256 * 1024;
    let threads = rayon::current_num_threads().max(1);
    let target_chunks = (threads * 4).max(1);
    let chunk = ((n + target_chunks - 1) / target_chunks).max(MIN_CHUNK);

    let ranges: Vec<(usize, usize)> = (0..n)
        .step_by(chunk)
        .map(|start| (start, (start + chunk).min(n)))
        .collect();

    if ranges.len() == 1 {
        diff_scan(old, new, matcher, writer)?;
        return Ok(());
    }

    let results: Vec<(Vec<u8>, i64)> = ranges
        .par_iter()
        .map(|&(start, end)| {
            let mut buf = Vec::new();
            // Writing into a Vec is infallible, so this never errors.
            let end_pos = diff_scan(old, &new[start..end], matcher, &mut buf)
                .expect("writing a diff into a Vec cannot fail");
            (buf, end_pos)
        })
        .collect();

    let mut prev_end: i64 = 0;
    for (idx, (bytes, end_pos)) in results.into_iter().enumerate() {
        if idx > 0 {
            // The decoder's oldpos is currently `prev_end`; reset it to 0 for this chunk.
            write_seek_record(writer, -(prev_end as isize))?;
        }
        writer.write_all(&bytes)?;
        prev_end = end_pos;
    }

    Ok(())
}

fn bsdiff_internal(old: &[u8], new: &[u8], writer: &mut dyn Write) -> io::Result<()> {
    let matcher = Matcher::build(old);

    #[cfg(feature = "parallel")]
    parallel_scan(old, new, &matcher, writer)?;
    #[cfg(not(feature = "parallel"))]
    diff_scan(old, new, &matcher, writer)?;

    Ok(())
}
