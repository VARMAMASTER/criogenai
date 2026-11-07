//! Semantic memory search without an index (design 0008 §16): how long one
//! query takes as a namespace grows. `cargo run -p criogen-bench --release
//! --bin vectors [-- 1000 10000 50000]`. Random 768-dimension vectors (the
//! size of common embedding models), SQLite in a temporary file and in
//! memory, top 5. Prints the median of 15 queries per size. Then the
//! in-memory indexes of design 0025 (`--indexes`): build time, query time
//! and recall@10 of HNSW against the exact flat index.

use std::time::Instant;

use criogen_storage::vector::{FlatIndex, HnswIndex, VectorIndex};
use criogen_storage::{
    Embedding, InMemoryStore, MemoryStore, Namespace, SearchQuery, SqliteMemoryStore,
};
use serde_json::json;

const DIMS: usize = 768;
const QUERIES: usize = 15;

/// xorshift: deterministic vectors, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32 - 0.5
    }

    fn vector(&mut self) -> Vec<f32> {
        (0..DIMS).map(|_| self.next()).collect()
    }
}

async fn fill(store: &dyn MemoryStore, ns: &Namespace, n: usize, rng: &mut Rng) {
    for i in 0..n {
        store
            .put_embedded(
                ns,
                &format!("fact-{i}"),
                json!(format!("fact number {i}")),
                None,
                Embedding::new("bench/768", rng.vector()),
            )
            .await
            .expect("put");
    }
}

async fn median_ms(store: &dyn MemoryStore, ns: &Namespace, rng: &mut Rng) -> f64 {
    let mut times = Vec::new();
    for _ in 0..QUERIES {
        let query = SearchQuery::new()
            .similar_to(Embedding::new("bench/768", rng.vector()))
            .limit(5);
        let started = Instant::now();
        let found = store.search(ns, &query).await.expect("search");
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(found.len(), 5);
    }
    times.sort_by(f64::total_cmp);
    times[QUERIES / 2]
}

fn indexes(sizes: &[usize], clustered: bool) {
    let mut rng = Rng(0x5851_f42d_4c95_7f2d);
    // Real embeddings gather around topics: 500 centres, each vector a centre
    // plus noise (`--clustered`); uniform random vectors are the worst case.
    let centres: Vec<Vec<f32>> = (0..500).map(|_| rng.vector()).collect();
    let sample = move |rng: &mut Rng| -> Vec<f32> {
        let v = rng.vector();
        if !clustered {
            return v;
        }
        let c = &centres[(rng.next().abs() * 1000.0) as usize % centres.len()];
        c.iter().zip(&v).map(|(c, n)| c + n * 0.35).collect()
    };
    println!(
        "{} vectors",
        if clustered {
            "clustered"
        } else {
            "uniform random"
        }
    );
    println!("items\tflat build s\tflat query ms\thnsw build s\thnsw query ms\thnsw recall@10");
    for &n in sizes {
        let data: Vec<Vec<f32>> = (0..n).map(|_| sample(&mut rng)).collect();
        let queries: Vec<Vec<f32>> = (0..50).map(|_| sample(&mut rng)).collect();
        let started = Instant::now();
        let mut flat = FlatIndex::new();
        for (i, v) in data.iter().enumerate() {
            flat.add(i as u64, v).expect("add");
        }
        let flat_build = started.elapsed().as_secs_f64();
        let started = Instant::now();
        let mut hnsw = HnswIndex::default();
        for (i, v) in data.iter().enumerate() {
            hnsw.add(i as u64, v).expect("add");
        }
        let hnsw_build = started.elapsed().as_secs_f64();
        let time = |index: &dyn VectorIndex| {
            let mut times: Vec<f64> = queries
                .iter()
                .map(|q| {
                    let started = Instant::now();
                    let _ = index.search(q, 10);
                    started.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            times.sort_by(f64::total_cmp);
            times[times.len() / 2]
        };
        let (flat_ms, hnsw_ms) = (time(&flat), time(&hnsw));
        let mut found = 0;
        for q in &queries {
            let truth: Vec<u64> = flat.search(q, 10).into_iter().map(|h| h.0).collect();
            found += hnsw
                .search(q, 10)
                .iter()
                .filter(|h| truth.contains(&h.0))
                .count();
        }
        let recall = found as f64 / (queries.len() * 10) as f64;
        println!("{n}\t{flat_build:.2}\t{flat_ms:.2}\t{hnsw_build:.2}\t{hnsw_ms:.2}\t{recall:.3}");
    }
}

#[tokio::main]
async fn main() {
    if std::env::args().any(|a| a == "--indexes") {
        let sizes: Vec<usize> = std::env::args()
            .skip(1)
            .filter_map(|a| a.parse().ok())
            .collect();
        let clustered = std::env::args().any(|a| a == "--clustered");
        indexes(
            if sizes.is_empty() {
                &[10_000, 50_000, 100_000]
            } else {
                &sizes
            },
            clustered,
        );
        return;
    }
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let sizes = if sizes.is_empty() {
        vec![1_000, 10_000, 50_000]
    } else {
        sizes
    };
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    println!("items\tin-memory ms\tsqlite file ms");
    for n in sizes {
        let ns = Namespace::new(["bench", "user"]).expect("namespace");
        let memory = InMemoryStore::new();
        fill(&memory, &ns, n, &mut rng).await;
        let in_memory = median_ms(&memory, &ns, &mut rng).await;

        let dir = std::env::temp_dir().join(format!("criogen-vec-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let url = format!("sqlite://{}", dir.join("m.db").display()).replace('\\', "/");
        let sqlite = SqliteMemoryStore::connect(&url).await.expect("sqlite");
        fill(&sqlite, &ns, n, &mut rng).await;
        let file = median_ms(&sqlite, &ns, &mut rng).await;
        drop(sqlite);
        let _ = std::fs::remove_dir_all(&dir);
        println!("{n}\t{in_memory:.2}\t{file:.2}");
    }
}
