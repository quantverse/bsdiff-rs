# bsdiff-rs

[![GitHub](https://img.shields.io/badge/github-bsdiff-8da0cb?logo=github)](https://github.com/space-wizards/bsdiff-rs)
[![crates.io version](https://img.shields.io/crates/v/bsdiff.svg)](https://crates.io/crates/bsdiff)
[![docs.rs docs](https://docs.rs/bsdiff/badge.svg)](https://docs.rs/bsdiff)
[![crates.io version](https://img.shields.io/crates/l/bsdiff.svg)](https://github.comspace-wizards/bsdiff-rss/blob/main/LICENSE-APACHE)
[![CI build](https://github.com/space-wizards/bsdiff-rs/actions/workflows/rust.yml/badge.svg)](https://github.com/space-wizards/bsdiff-rs/actions)

Bsdiff is a method of diffing files. This crate is a port of a [bsdiff library](https://github.com/mendsley/bsdiff).
High performance patching. All written in safe
Rust.

It is usually a good idea to use bsdiff alongside a compression algorithm like bzip2.

## Usage

```rust
fn main() {
    let one = vec![1, 2, 3, 4, 5];
    let two = vec![1, 2, 4, 6];
    let mut patch = Vec::new();

    bsdiff::diff(&one, &two, &mut patch).unwrap();

    let mut patched = Vec::with_capacity(two.len());
    bsdiff::patch(&one, &mut patch.as_slice(), &mut patched).unwrap();
    assert_eq!(patched, two);
}
```

## Diffing Files

```rust
fn diff_files(file_a: &str, file_b: &str, patch_file: &str) -> std::io::Result<()> {
    let old = std::fs::read(file_a)?;
    let new = std::fs::read(file_b)?;
    let mut patch = Vec::new();

    bsdiff::diff(&old, &new, &mut patch)?;
    // TODO: compress `patch` here
    std::fs::write(patch_file, &patch)
}
```

## Patching Files

```rust
fn patch_file(file_a: &str, patch_file: &str, file_b: &str) -> std::io::Result<()> {
    let old = std::fs::read(file_a)?;
    let patch = std::fs::read(patch_file)?;
    // TODO: decompress `patch` here
    let mut new = Vec::new();

    bsdiff::patch(&old, &mut patch.as_slice(), &mut new)?;
    std::fs::write(file_b, &new)
}
```

## Performance

Match finding uses a hash-chain index over `old` (every position is keyed by the hash
of its first few bytes, à la LZ77/zstd) instead of the original `qsufsort` suffix array.
Building the index is a single linear pass rather than a full suffix sort, which is far
cheaper for multi-megabyte inputs. Every candidate match is still verified byte-for-byte,
so patches always round-trip; matches are *good* rather than provably longest, so patch
size may differ slightly from the classic algorithm.

The default `parallel` feature builds the index concurrently and splits `new` into
contiguous fixed-size chunks that are diffed in parallel with Rayon, stitching the
sub-patches back into one stream. On a many-core machine this diffs multi-megabyte
inputs several times faster than the original.

Determinism: both builds produce bit-identical output for identical inputs — patch
bytes never depend on thread count, core count, or scheduling. The two builds differ
from *each other* (chunking inserts seek records between chunk sub-patches), so pick
one build per artifact pipeline when comparing patches by hash.

To compare the two builds, save a Criterion baseline with default features disabled,
then run the default benchmark against that baseline:

```sh
cargo bench --bench diff --no-default-features -- --save-baseline serial
cargo bench --bench diff -- --baseline serial
```
