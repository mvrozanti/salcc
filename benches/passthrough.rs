//! Passthrough seam latency bench: 4 KiB chunks through the reader->writer
//! mpsc channel and the ChunkTransform seam with the identity transform,
//! mirroring the allocation profile of the real hot path. Budget from
//! CLAUDE.md: p99 chunk-in -> chunk-out under 1ms.

use std::sync::mpsc;
use std::thread;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use salcc::proxy::io_loop::{ChunkTransform, IdentityTransform};

const CHUNK_LEN: usize = 4096;

fn chunk_roundtrip(c: &mut Criterion) {
    let (work_tx, work_rx) = mpsc::channel::<Vec<u8>>();
    let (done_tx, done_rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut transform = IdentityTransform;
        while let Ok(chunk) = work_rx.recv() {
            let mut styled = Vec::with_capacity(chunk.len());
            transform.transform(&chunk, &mut styled);
            if done_tx.send(styled).is_err() {
                break;
            }
        }
    });
    let mut group = c.benchmark_group("passthrough");
    group.throughput(Throughput::Bytes(CHUNK_LEN as u64));
    group.bench_function("identity_4k_channel_roundtrip", |b| {
        let mut carry = vec![0xa5u8; CHUNK_LEN];
        b.iter(|| {
            work_tx.send(std::mem::take(&mut carry)).unwrap();
            carry = done_rx.recv().unwrap();
            assert_eq!(carry.len(), CHUNK_LEN);
        });
    });
    group.finish();
}

criterion_group!(benches, chunk_roundtrip);
criterion_main!(benches);
