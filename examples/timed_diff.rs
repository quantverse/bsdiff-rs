use std::time::Instant;

fn main() -> std::io::Result<()> {
    let old = std::fs::read("SNAPSHOT_RAW.bin")?;
    let new = std::fs::read("SNAPSHOT2_RAW.bin")?;

    let mut patch = Vec::new();
    let start = Instant::now();
    bsdiff::diff(&old, &new, &mut patch)?;
    let elapsed = start.elapsed();

    // Correctness guard: the patch must reconstruct `new` exactly.
    let mut reconstructed = Vec::with_capacity(new.len());
    bsdiff::patch(&old, &mut patch.as_slice(), &mut reconstructed)?;
    let ok = reconstructed == new;

    println!("old:   {} bytes", old.len());
    println!("new:   {} bytes", new.len());
    println!("patch: {} bytes", patch.len());
    println!("round-trip: {}", if ok { "OK" } else { "MISMATCH" });
    println!("diff took: {:.3?}", elapsed);
    assert!(ok, "round-trip mismatch: patch does not reconstruct new");
    Ok(())
}
