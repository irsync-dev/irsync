mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{connect_pair, loopback_pair, within};
use irsync::fs::paths::RelPath;
use irsync::hasher::Hasher;
use irsync::index::Index;
use irsync::net::frame::read_msg;
use irsync::send::Sender;
use irsync_proto::chunk::{digest_bytes, FileDigest};
use irsync_proto::v1::{DataChunk, DataStreamHeader};

const MIB: usize = 1024 * 1024;

type Received = Vec<(DataStreamHeader, Vec<Vec<u8>>)>;

fn data(n: usize, seed: u8) -> Vec<u8> {
    let mut x = seed as u32 + 1;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 24) as u8
        })
        .collect()
}

/// Accepts uni streams until the headers account for `total` bytes.
fn receive(server: iroh::endpoint::Connection, total: u64) -> tokio::task::JoinHandle<Received> {
    tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let acceptor = tokio::spawn(async move {
            while let Ok(mut s) = server.accept_uni().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let h: DataStreamHeader = read_msg(&mut s).await.unwrap().unwrap();
                    let mut chunks = Vec::new();
                    while let Some(c) = read_msg::<DataChunk, _>(&mut s).await.unwrap() {
                        chunks.push(c.data);
                    }
                    let _ = tx.send((h, chunks));
                });
            }
        });
        let mut out: Received = Vec::new();
        let mut got = 0;
        while got < total {
            let (h, c) = rx.recv().await.unwrap();
            got += h.length;
            out.push((h, c));
        }
        acceptor.abort();
        out
    })
}

struct Fixture {
    root: tempfile::TempDir,
    _state: tempfile::TempDir,
    hasher: Arc<Hasher>,
    client: iroh::endpoint::Connection,
    server: iroh::endpoint::Connection,
    _eps: (iroh::Endpoint, iroh::Endpoint),
}

async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let idx = Index::open(state.path(), root.path())
        .unwrap()
        .into_handle();
    let hasher = Arc::new(Hasher::new(idx, root.path().to_path_buf(), 2));
    let (a, b) = loopback_pair().await;
    let (client, server) = connect_pair(&a, &b).await;
    Fixture {
        root,
        _state: state,
        hasher,
        client,
        server,
        _eps: (a, b),
    }
}

fn put(f: &Fixture, name: &str, bytes: &[u8]) -> (RelPath, FileDigest) {
    std::fs::write(f.root.path().join(name), bytes).unwrap();
    (RelPath::new(name.as_bytes()).unwrap(), digest_bytes(bytes))
}

fn sender(f: &Fixture, parallel: usize, split: u64, bw: Option<u64>) -> Sender {
    Sender::new(
        f.client.clone(),
        [7; 16],
        f.root.path().to_path_buf(),
        f.hasher.clone(),
        parallel,
        split,
        bw,
    )
}

#[tokio::test]
async fn whole_file_arrives_in_chunk_frames() {
    let f = fixture().await;
    let bytes = data(MIB / 2, 1);
    let (p, d) = put(&f, "a.bin", &bytes);
    let idxs: Vec<u32> = (0..d.chunks.len() as u32).collect();
    let rx = receive(f.server.clone(), d.size);
    within(sender(&f, 4, 64 << 20, None).send_chunks(3, &p, &d, &idxs))
        .await
        .unwrap();
    let got = within(rx).await.unwrap();
    assert_eq!(got.len(), 1);
    let (h, chunks) = &got[0];
    assert_eq!(h.session_id, vec![7; 16]);
    assert_eq!(
        (h.file_id, h.offset, h.length, h.first_chunk_idx),
        (3, 0, d.size, 0)
    );
    assert_eq!(chunks.len(), d.chunks.len());
    assert_eq!(chunks.concat(), bytes);
}

#[tokio::test]
async fn large_file_split_into_ranges_under_threshold() {
    let f = fixture().await;
    let bytes = data(5 * MIB, 2);
    let (p, d) = put(&f, "big.bin", &bytes);
    let idxs: Vec<u32> = (0..d.chunks.len() as u32).collect();
    let rx = receive(f.server.clone(), d.size);
    within(sender(&f, 4, MIB as u64, None).send_chunks(1, &p, &d, &idxs))
        .await
        .unwrap();
    let mut got = within(rx).await.unwrap();
    assert!(got.len() >= 5, "only {} streams", got.len());
    got.sort_by_key(|(h, _)| h.offset);
    let mut all = Vec::new();
    for (h, chunks) in &got {
        assert!(h.length <= MIB as u64);
        assert_eq!(h.offset as usize, all.len());
        all.extend(chunks.concat());
    }
    assert_eq!(all, bytes);
}

#[tokio::test]
async fn parallel_bound_respected() {
    let f = fixture().await;
    let s = Arc::new(sender(&f, 2, 1, None));
    let mut total = 0;
    let mut jobs = Vec::new();
    for i in 0..10u64 {
        let bytes = data(300 * 1024, i as u8);
        let (p, d) = put(&f, &format!("f{i}"), &bytes);
        total += d.size;
        jobs.push((i, p, d));
    }
    let rx = receive(f.server.clone(), total);
    let mut set = tokio::task::JoinSet::new();
    for (i, p, d) in jobs {
        let s = s.clone();
        set.spawn(async move {
            let idxs: Vec<u32> = (0..d.chunks.len() as u32).collect();
            s.send_chunks(i, &p, &d, &idxs).await
        });
    }
    within(async {
        while let Some(r) = set.join_next().await {
            r.unwrap().unwrap();
        }
    })
    .await;
    within(rx).await.unwrap();
    let max = s.max_concurrency();
    assert!((1..=2).contains(&max), "max concurrency {max}");
}

#[tokio::test]
async fn bwlimit_caps_rate() {
    let f = fixture().await;
    let bytes = data(MIB, 3);
    let (p, d) = put(&f, "slow.bin", &bytes);
    let idxs: Vec<u32> = (0..d.chunks.len() as u32).collect();
    let rx = receive(f.server.clone(), d.size);
    let s = sender(&f, 4, 64 << 20, Some(256 * 1024));
    let t = Instant::now();
    tokio::time::timeout(Duration::from_secs(10), s.send_chunks(1, &p, &d, &idxs))
        .await
        .expect("bwlimit send timed out")
        .unwrap();
    let el = t.elapsed();
    tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap();
    assert!(el >= Duration::from_millis(3500), "too fast: {el:?}");
    assert!(el < Duration::from_secs(10), "too slow: {el:?}");
}

#[tokio::test]
async fn changed_file_is_invalid_data() {
    let f = fixture().await;
    let (p, d) = put(&f, "c.bin", &data(MIB / 2, 4));
    std::fs::write(f.root.path().join("c.bin"), data(MIB / 2, 5)).unwrap();
    let idxs: Vec<u32> = (0..d.chunks.len() as u32).collect();
    let e = within(sender(&f, 4, 64 << 20, None).send_chunks(1, &p, &d, &idxs))
        .await
        .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    assert!(e.to_string().contains("c.bin"));
}
