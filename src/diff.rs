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

/// Build the suffix array of `old`.
///
/// Returns an index array `I` of length `old.len() + 1` where `I[0] == old.len()`
/// (the empty/sentinel suffix, which is lexicographically smallest) followed by the
/// positions of every suffix of `old` in ascending lexicographic order. This layout
/// matches what [`search`] expects and what the original `qsufsort` produced, so the
/// rest of the algorithm is unchanged.
///
/// The heavy lifting is delegated to `cdivsufsort` (a port of libdivsufsort), which
/// constructs the suffix array in `O(n)` space and near-linear time — dramatically
/// faster than the original `O(n log n)` Larsson–Sadakane sorter for large inputs.
fn build_suffix_array(old: &[u8]) -> Vec<i32> {
    let n = old.len();
    assert!(
        n <= i32::MAX as usize,
        "bsdiff: inputs larger than 2 GiB are not supported"
    );
    let mut I = vec![0i32; n + 1];
    I[0] = n as i32;
    if n > 0 {
        cdivsufsort::sort_in_place(old, &mut I[1..]);
    }
    I
}

fn matchlen(old: &[u8], new: &[u8]) -> usize {
    old.iter().zip(new).take_while(|(a, b)| a == b).count()
}

fn search(I: &[i32], old: &[u8], new: &[u8]) -> (usize, usize) {
    if I.len() < 3 {
        let x = matchlen(&old[I[0] as usize..], new);
        let y = matchlen(&old[I[I.len() - 1] as usize..], new);
        if x > y {
            (I[0] as usize, x)
        } else {
            (I[I.len() - 1] as usize, y)
        }
    } else {
        let mid = (I.len() - 1) / 2;
        let left = &old[I[mid] as usize..];
        let right = new;
        let len_to_check = left.len().min(right.len());
        if left[..len_to_check] < right[..len_to_check] {
            search(&I[mid..], old, new)
        } else {
            search(&I[..=mid], old, new)
        }
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
fn diff_scan(old: &[u8], new: &[u8], I: &[i32], writer: &mut dyn Write) -> io::Result<i64> {
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
            let (p, l) = search(&I[..=old.len()], old, &new[scan..]);
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
fn parallel_scan(old: &[u8], new: &[u8], I: &[i32], writer: &mut dyn Write) -> io::Result<()> {
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
        diff_scan(old, new, I, writer)?;
        return Ok(());
    }

    let results: Vec<(Vec<u8>, i64)> = ranges
        .par_iter()
        .map(|&(start, end)| {
            let mut buf = Vec::new();
            // Writing into a Vec is infallible, so this never errors.
            let end_pos = diff_scan(old, &new[start..end], I, &mut buf)
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
    let I = build_suffix_array(old);

    #[cfg(feature = "parallel")]
    parallel_scan(old, new, &I, writer)?;
    #[cfg(not(feature = "parallel"))]
    diff_scan(old, new, &I, writer)?;

    Ok(())
}
