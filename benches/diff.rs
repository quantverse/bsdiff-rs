use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};

fn make_pair(size: usize) -> (Vec<u8>, Vec<u8>) {
    let mut old = Vec::with_capacity(size);
    for i in 0..size {
        old.push(((i * 37 + i / 3 + (i >> 7) * 11) % 251) as u8);
    }

    let insert_len = (size / 128).clamp(512, 8 * 1024);
    let mut new = Vec::with_capacity(size + insert_len * 2);
    new.extend_from_slice(&old[..size / 4]);
    new.extend((0..insert_len).map(|i| (255 - (i * 13 % 251)) as u8));
    new.extend_from_slice(&old[size / 5..size * 2 / 3]);
    new.extend((0..insert_len).map(|i| ((i * 17 + 3) % 251) as u8));
    new.extend_from_slice(&old[size * 3 / 4..]);

    let edit_step = (new.len() / 64).max(1);
    for i in (insert_len..new.len()).step_by(edit_step) {
        new[i] ^= 0x5a;
    }

    (old, new)
}

fn bench_case(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    id: BenchmarkId,
    old: &[u8],
    new: &[u8],
) {
    group.throughput(Throughput::Bytes(new.len() as u64));
    group.bench_function(id, |b| {
        b.iter_batched(
            || Vec::with_capacity(new.len() / 2),
            |mut patch| {
                bsdiff::diff(black_box(old), black_box(new), &mut patch).unwrap();
                black_box(patch.len());
            },
            BatchSize::SmallInput,
        );
    });
}

fn diff_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff");
    group.sample_size(10);

    for size in [64 * 1024, 256 * 1024, 1024 * 1024] {
        let (old, new) = make_pair(size);
        bench_case(
            &mut group,
            BenchmarkId::new("synthetic", format!("{}KiB", size / 1024)),
            &old,
            &new,
        );
    }

    group.finish();
}

criterion_group!(benches, diff_benchmarks);
criterion_main!(benches);
