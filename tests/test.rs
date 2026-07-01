use std::io::ErrorKind;

fn assert_round_trip(old: &[u8], new: &[u8]) {
    let mut patch = Vec::new();
    bsdiff::diff(old, new, &mut patch).unwrap();

    let mut patched = Vec::with_capacity(new.len());
    bsdiff::patch(old, &mut patch.as_slice(), &mut patched).unwrap();
    assert_eq!(patched, new);
}

#[test]
fn test_round_trip_edge_cases() {
    assert_round_trip(&[], &[]);
    assert_round_trip(&[], b"new data");
    assert_round_trip(b"old data", &[]);
    assert_round_trip(b"aaaaabaaaaacaaaaad", b"aaaaacaaaaabaaaaae");
}

#[test]
fn test_round_trip_large_repetitive_input() {
    let mut old = Vec::with_capacity(96 * 1024);
    for i in 0..96 * 1024 {
        old.push(((i * 37 + i / 3) % 251) as u8);
    }

    let mut new = Vec::with_capacity(old.len() + 1024);
    new.extend_from_slice(&old[..24 * 1024]);
    new.extend((0..512).map(|i| (255 - i % 251) as u8));
    new.extend_from_slice(&old[20 * 1024..64 * 1024]);
    new.extend((0..512).map(|i| (i % 251) as u8));
    new.extend_from_slice(&old[68 * 1024..]);

    assert_round_trip(&old, &new);
}

#[test]
fn test_it() {
    // The test files are just build artifacts I had lying around.
    // Quite large and probably *some* similarities.
    let one = std::fs::read("tests/test_1").unwrap();
    let two = std::fs::read("tests/test_2").unwrap();

    let mut patch = Vec::with_capacity(two.len());
    bsdiff::diff(&one, &two, &mut patch).unwrap();

    // Round-trip is the real contract and holds for every build.
    let mut patched = Vec::with_capacity(two.len());
    bsdiff::patch(&one, &mut patch.as_slice(), &mut patched).unwrap();
    assert!(patched == two);

    // Exact patch bytes are only deterministic for the single-threaded build;
    // the parallel path splits `new` into a thread-count-dependent number of chunks.
    #[cfg(not(feature = "parallel"))]
    {
        let expected = std::fs::read("tests/expected_diff").unwrap();
        assert_eq!(expected, patch);
    }
}

#[test]
fn test_truncated_patch() {
    let one = vec![1, 2, 3];
    let two = [1, 2, 3, 4];
    let mut buf = Vec::new();

    bsdiff::diff(&one, &two, &mut buf).unwrap();

    let mut patched = Vec::new();
    while buf.len() > 1 {
        buf.pop();
        let error = bsdiff::patch(&one, &mut buf.as_slice(), &mut patched).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }
}
