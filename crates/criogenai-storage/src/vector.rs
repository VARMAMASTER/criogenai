//! Vector indexes kept in process memory (design 0025): [`FlatIndex`]
//! (exact) and [`HnswIndex`] (approximate, fast on large sets). Vectors are
//! normalised when added, so similarity is a dot product (cosine).
//!
//! The vectors themselves are stored elsewhere (the memory store, encrypted
//! when it is); an index is rebuilt from them when a collection opens.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::error::{StorageError, StorageErrorKind};
use crate::store::MAX_DIMENSIONS;

/// The most results one search returns.
pub const MAX_K: usize = 1_000;
/// The most vectors an index holds by default.
pub const DEFAULT_MAX_VECTORS: usize = 2_000_000;

/// An index of vectors by id, searched by cosine similarity.
pub trait VectorIndex: Send + Sync + std::fmt::Debug {
    /// Adds (or replaces) `id`'s vector.
    fn add(&mut self, id: u64, vector: &[f32]) -> Result<(), StorageError>;
    /// Removes `id`; `true` if it was there.
    fn remove(&mut self, id: u64) -> bool;
    /// The `k` most similar ids, best first, with their cosine similarity.
    fn search(&self, query: &[f32], k: usize) -> Vec<(u64, f32)>;
    /// Vectors held.
    fn len(&self) -> usize;
    /// Whether it holds none.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The dimensions, once a vector was added.
    fn dimensions(&self) -> Option<usize>;
}

fn invalid(message: impl Into<String>) -> StorageError {
    StorageError::new(StorageErrorKind::InvalidValue, message)
}

/// The vector scaled to unit length, after checking it.
fn normalized(vector: &[f32], dims: Option<usize>) -> Result<Vec<f32>, StorageError> {
    if vector.is_empty() || vector.len() > MAX_DIMENSIONS {
        return Err(invalid(format!(
            "a vector has 1 to {MAX_DIMENSIONS} dimensions, not {}",
            vector.len()
        )));
    }
    if let Some(dims) = dims
        && vector.len() != dims
    {
        return Err(invalid(format!(
            "this index holds {dims}-dimension vectors, not {}",
            vector.len()
        )));
    }
    if vector.iter().any(|v| !v.is_finite()) {
        return Err(invalid("a vector's values must be finite numbers"));
    }
    let norm = vector
        .iter()
        .map(|v| f64::from(*v) * f64::from(*v))
        .sum::<f64>()
        .sqrt();
    if norm == 0.0 {
        return Ok(vector.to_vec());
    }
    Ok(vector
        .iter()
        .map(|v| (f64::from(*v) / norm) as f32)
        .collect())
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    // Blocks of eight without index checks: the compiler turns the inner
    // loop into vector instructions.
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    let mut lanes = [0f32; 8];
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            lanes[i] += x[i] * y[i];
        }
    }
    let mut sum = lanes.iter().sum::<f32>();
    for (x, y) in ra.iter().zip(rb) {
        sum += x * y;
    }
    sum
}

/// A score with an id, ordered by score (then id, for a stable order).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Scored(f32, u32);

impl Eq for Scored {}

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0).then(other.1.cmp(&self.1))
    }
}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn check_query(query: &[f32], dims: Option<usize>) -> Option<Vec<f32>> {
    let dims = dims?;
    normalized(query, Some(dims)).ok()
}

// ---------- flat ----------

/// Exact search: every vector is compared with the query.
#[derive(Debug, Clone, Default)]
pub struct FlatIndex {
    dims: Option<usize>,
    /// Vectors back to back.
    data: Vec<f32>,
    ids: Vec<u64>,
    slots: HashMap<u64, usize>,
    max: usize,
}

impl FlatIndex {
    /// An empty index (at most [`DEFAULT_MAX_VECTORS`]).
    pub fn new() -> Self {
        Self {
            max: DEFAULT_MAX_VECTORS,
            ..Self::default()
        }
    }

    /// Sets the most vectors it may hold.
    pub fn max_vectors(mut self, max: usize) -> Self {
        self.max = max.max(1);
        self
    }
}

impl VectorIndex for FlatIndex {
    fn add(&mut self, id: u64, vector: &[f32]) -> Result<(), StorageError> {
        let v = normalized(vector, self.dims)?;
        let dims = v.len();
        if let Some(&slot) = self.slots.get(&id) {
            self.data[slot * dims..(slot + 1) * dims].copy_from_slice(&v);
            return Ok(());
        }
        if self.ids.len() >= self.max {
            return Err(invalid(format!("the index is full ({} vectors)", self.max)));
        }
        self.dims = Some(dims);
        self.slots.insert(id, self.ids.len());
        self.ids.push(id);
        self.data.extend_from_slice(&v);
        Ok(())
    }

    fn remove(&mut self, id: u64) -> bool {
        let (Some(slot), Some(dims)) = (self.slots.remove(&id), self.dims) else {
            return false;
        };
        // Move the last vector into the hole.
        let last = self.ids.len() - 1;
        if slot != last {
            let moved = self.ids[last];
            self.data
                .copy_within(last * dims..(last + 1) * dims, slot * dims);
            self.ids[slot] = moved;
            self.slots.insert(moved, slot);
        }
        self.ids.pop();
        self.data.truncate(last * dims);
        true
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(u64, f32)> {
        let Some(q) = check_query(query, self.dims) else {
            return Vec::new();
        };
        let k = k.min(MAX_K);
        if k == 0 {
            return Vec::new();
        }
        let dims = q.len();
        // A min-heap of the best k.
        let mut best: BinaryHeap<std::cmp::Reverse<Scored>> = BinaryHeap::with_capacity(k + 1);
        for (slot, chunk) in self.data.chunks_exact(dims).enumerate() {
            let s = Scored(dot(&q, chunk), slot as u32);
            if best.len() < k {
                best.push(std::cmp::Reverse(s));
            } else if best.peek().is_some_and(|w| s > w.0) {
                best.pop();
                best.push(std::cmp::Reverse(s));
            }
        }
        let mut out: Vec<Scored> = best.into_iter().map(|r| r.0).collect();
        out.sort_by(|a, b| b.cmp(a));
        out.into_iter()
            .map(|s| (self.ids[s.1 as usize], s.0))
            .collect()
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn dimensions(&self) -> Option<usize> {
        self.dims
    }
}

// ---------- HNSW ----------

/// How an [`HnswIndex`] is built and searched.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct HnswConfig {
    /// Links per node above layer 0 (twice as many on layer 0). Default 16.
    pub m: usize,
    /// Candidates kept while inserting. Default 100.
    pub ef_construction: usize,
    /// Candidates kept while searching (at least `k`). Default 64.
    pub ef_search: usize,
    /// The most vectors. Default [`DEFAULT_MAX_VECTORS`].
    pub max_vectors: usize,
    /// The seed of the level draw (builds are reproducible).
    pub seed: u64,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 100,
            ef_search: 64,
            max_vectors: DEFAULT_MAX_VECTORS,
            seed: 0x2545_f491_4f6c_dd1d,
        }
    }
}

impl HnswConfig {
    /// Sets `ef_search`.
    pub fn ef_search(mut self, ef: usize) -> Self {
        self.ef_search = ef.max(1);
        self
    }
    /// Sets `m`.
    pub fn m(mut self, m: usize) -> Self {
        self.m = m.clamp(2, 64);
        self
    }
    /// Sets `ef_construction`.
    pub fn ef_construction(mut self, ef: usize) -> Self {
        self.ef_construction = ef.max(1);
        self
    }
    /// Sets the most vectors.
    pub fn max_vectors(mut self, max: usize) -> Self {
        self.max_vectors = max.max(1);
        self
    }
}

#[derive(Debug, Clone)]
struct Node {
    id: u64,
    vector: Vec<f32>,
    /// Neighbours per layer, 0 first.
    links: Vec<Vec<u32>>,
    deleted: bool,
}

/// Approximate search on a Hierarchical Navigable Small World graph.
#[derive(Debug, Clone)]
pub struct HnswIndex {
    config: HnswConfig,
    dims: Option<usize>,
    nodes: Vec<Node>,
    by_id: HashMap<u64, u32>,
    entry: Option<u32>,
    top: usize,
    deleted: usize,
    rng: u64,
}

impl Default for HnswIndex {
    fn default() -> Self {
        Self::new(HnswConfig::default())
    }
}

impl HnswIndex {
    /// An empty index.
    pub fn new(config: HnswConfig) -> Self {
        Self {
            rng: config.seed.max(1),
            config,
            dims: None,
            nodes: Vec::new(),
            by_id: HashMap::new(),
            entry: None,
            top: 0,
            deleted: 0,
        }
    }

    fn level(&mut self) -> usize {
        // xorshift, then -ln(U) / ln(m).
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let u = ((self.rng >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0);
        let ml = 1.0 / (self.config.m as f64).ln();
        ((-u.ln() * ml).floor() as usize).min(16)
    }

    fn sim(&self, q: &[f32], n: u32) -> f32 {
        dot(q, &self.nodes[n as usize].vector)
    }

    /// Greedy search on one layer from `entry`, keeping `ef` candidates.
    fn search_layer(&self, q: &[f32], entry: &[u32], ef: usize, layer: usize) -> Vec<Scored> {
        let mut visited = vec![false; self.nodes.len()];
        for &e in entry {
            visited[e as usize] = true;
        }
        // Candidates to expand: best first. Results: worst on top.
        let mut candidates: BinaryHeap<Scored> = BinaryHeap::new();
        let mut results: BinaryHeap<std::cmp::Reverse<Scored>> = BinaryHeap::new();
        for &e in entry {
            let s = Scored(self.sim(q, e), e);
            candidates.push(s);
            results.push(std::cmp::Reverse(s));
        }
        while let Some(c) = candidates.pop() {
            let worst = results.peek().map_or(f32::NEG_INFINITY, |w| w.0.0);
            if c.0 < worst && results.len() >= ef {
                break;
            }
            let links = &self.nodes[c.1 as usize].links;
            let Some(neighbours) = links.get(layer) else {
                continue;
            };
            for &n in neighbours {
                if std::mem::replace(&mut visited[n as usize], true) {
                    continue;
                }
                let s = Scored(self.sim(q, n), n);
                let worst = results.peek().map_or(f32::NEG_INFINITY, |w| w.0.0);
                if results.len() < ef || s.0 > worst {
                    candidates.push(s);
                    results.push(std::cmp::Reverse(s));
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
        let mut out: Vec<Scored> = results.into_iter().map(|r| r.0).collect();
        out.sort_by(|a, b| b.cmp(a));
        out
    }

    /// The best `max` of `candidates` (sorted best first), preferring ones
    /// not closer to an already chosen neighbour than to the new node (the
    /// paper's heuristic), filled up with the rest.
    fn select(&self, candidates: &[Scored], max: usize) -> Vec<u32> {
        let mut chosen: Vec<Scored> = Vec::with_capacity(max);
        let mut skipped = Vec::new();
        for &c in candidates {
            if chosen.len() == max {
                break;
            }
            let v = &self.nodes[c.1 as usize].vector;
            if chosen
                .iter()
                .all(|s| dot(v, &self.nodes[s.1 as usize].vector) < c.0)
            {
                chosen.push(c);
            } else {
                skipped.push(c);
            }
        }
        for c in skipped {
            if chosen.len() == max {
                break;
            }
            chosen.push(c);
        }
        chosen.into_iter().map(|s| s.1).collect()
    }

    fn insert(&mut self, id: u64, vector: Vec<f32>) {
        let level = self.level();
        let node = self.nodes.len() as u32;
        self.nodes.push(Node {
            id,
            vector,
            links: vec![Vec::new(); level + 1],
            deleted: false,
        });
        self.by_id.insert(id, node);
        let Some(mut entry) = self.entry else {
            self.entry = Some(node);
            self.top = level;
            return;
        };
        let q = self.nodes[node as usize].vector.clone();
        // Descend greedily through the layers above the new node's.
        for layer in (level + 1..=self.top).rev() {
            if let Some(best) = self.search_layer(&q, &[entry], 1, layer).first() {
                entry = best.1;
            }
        }
        let mut entries = vec![entry];
        for layer in (0..=level.min(self.top)).rev() {
            let found = self.search_layer(&q, &entries, self.config.ef_construction, layer);
            let max = if layer == 0 {
                self.config.m * 2
            } else {
                self.config.m
            };
            let neighbours = self.select(&found, self.config.m);
            self.nodes[node as usize].links[layer] = neighbours.clone();
            for n in neighbours {
                let links = &mut self.nodes[n as usize].links;
                if let Some(list) = links.get_mut(layer) {
                    list.push(node);
                    if list.len() > max {
                        // Keep the closest `max` of n's links (the cheap rule:
                        // the full heuristic here dominated build time).
                        let nodes = &self.nodes;
                        let nv = &nodes[n as usize].vector;
                        let mut scored: Vec<Scored> = nodes[n as usize].links[layer]
                            .iter()
                            .map(|&o| Scored(dot(nv, &nodes[o as usize].vector), o))
                            .collect();
                        scored.sort_by(|a, b| b.cmp(a));
                        scored.truncate(max);
                        self.nodes[n as usize].links[layer] =
                            scored.into_iter().map(|s| s.1).collect();
                    }
                }
            }
            entries = found.iter().map(|s| s.1).collect();
        }
        if level > self.top {
            self.top = level;
            self.entry = Some(node);
        }
    }

    /// Builds the graph again from the live nodes.
    fn rebuild(&mut self) {
        let live: Vec<(u64, Vec<f32>)> = self
            .nodes
            .drain(..)
            .filter(|n| !n.deleted)
            .map(|n| (n.id, n.vector))
            .collect();
        self.by_id.clear();
        self.entry = None;
        self.top = 0;
        self.deleted = 0;
        self.rng = self.config.seed.max(1);
        for (id, v) in live {
            self.insert(id, v);
        }
    }
}

impl VectorIndex for HnswIndex {
    fn add(&mut self, id: u64, vector: &[f32]) -> Result<(), StorageError> {
        let v = normalized(vector, self.dims)?;
        if self.by_id.contains_key(&id) {
            self.remove(id);
        }
        if self.by_id.len() >= self.config.max_vectors {
            return Err(invalid(format!(
                "the index is full ({} vectors)",
                self.config.max_vectors
            )));
        }
        self.dims = Some(v.len());
        self.insert(id, v);
        Ok(())
    }

    fn remove(&mut self, id: u64) -> bool {
        let Some(node) = self.by_id.remove(&id) else {
            return false;
        };
        self.nodes[node as usize].deleted = true;
        self.deleted += 1;
        if self.by_id.is_empty() {
            self.nodes.clear();
            self.entry = None;
            self.top = 0;
            self.deleted = 0;
        } else if self.deleted * 10 > self.nodes.len() * 3 {
            self.rebuild();
        }
        true
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(u64, f32)> {
        let (Some(q), Some(mut entry)) = (check_query(query, self.dims), self.entry) else {
            return Vec::new();
        };
        let k = k.min(MAX_K);
        if k == 0 {
            return Vec::new();
        }
        for layer in (1..=self.top).rev() {
            if let Some(best) = self.search_layer(&q, &[entry], 1, layer).first() {
                entry = best.1;
            }
        }
        // Deleted nodes are walked through but not returned: widen the search
        // by how many there are.
        let ef = self.config.ef_search.max(k) + self.deleted.min(k * 4);
        self.search_layer(&q, &[entry], ef, 0)
            .into_iter()
            .filter(|s| !self.nodes[s.1 as usize].deleted)
            .take(k)
            .map(|s| (self.nodes[s.1 as usize].id, s.0))
            .collect()
    }

    fn len(&self) -> usize {
        self.by_id.len()
    }

    fn dimensions(&self) -> Option<usize> {
        self.dims
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn vectors(n: usize, dims: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                (0..dims)
                    .map(|_| {
                        s ^= s << 13;
                        s ^= s >> 7;
                        s ^= s << 17;
                        (s >> 40) as f32 / (1u64 << 24) as f32 - 0.5
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn flat_ranks_by_cosine_and_removes() {
        let mut index = FlatIndex::new();
        index.add(1, &[1.0, 0.0]).unwrap();
        index.add(2, &[0.0, 1.0]).unwrap();
        index.add(3, &[1.0, 1.0]).unwrap();
        let hits = index.search(&[2.0, 0.1], 2);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [1, 3]);
        assert!(index.remove(1) && !index.remove(1));
        assert_eq!(index.search(&[1.0, 0.0], 1)[0].0, 3);
        assert!(index.add(4, &[1.0]).is_err(), "other dimensions");
        assert!(index.add(5, &[f32::NAN, 1.0]).is_err());
        assert!(index.search(&[1.0], 1).is_empty());
    }

    #[test]
    fn hnsw_finds_what_flat_finds() {
        let data = vectors(5_000, 32, 7);
        let queries = vectors(50, 32, 99);
        let mut flat = FlatIndex::new();
        let mut hnsw = HnswIndex::default();
        for (i, v) in data.iter().enumerate() {
            flat.add(i as u64, v).unwrap();
            hnsw.add(i as u64, v).unwrap();
        }
        let mut found = 0;
        for q in &queries {
            let truth: HashSet<u64> = flat.search(q, 10).into_iter().map(|h| h.0).collect();
            found += hnsw
                .search(q, 10)
                .iter()
                .filter(|h| truth.contains(&h.0))
                .count();
        }
        let recall = found as f64 / (queries.len() * 10) as f64;
        assert!(recall >= 0.95, "recall@10 {recall}");
        // An exact copy of a stored vector is found first.
        assert_eq!(hnsw.search(&data[1234], 1)[0].0, 1234);
    }

    #[test]
    fn hnsw_removal_rebuild_and_reproducible_builds() {
        let data = vectors(400, 16, 3);
        let build = || {
            let mut h = HnswIndex::default();
            for (i, v) in data.iter().enumerate() {
                h.add(i as u64, v).unwrap();
            }
            h
        };
        let (a, b) = (build(), build());
        assert_eq!(a.search(&data[5], 5), b.search(&data[5], 5));
        let mut h = build();
        for i in 0..200 {
            assert!(h.remove(i));
        }
        assert_eq!(h.len(), 200);
        assert!(h.search(&data[3], 10).iter().all(|(id, _)| *id >= 200));
        assert_eq!(h.search(&data[300], 1)[0].0, 300);
        assert!(
            HnswIndex::new(HnswConfig::default().max_vectors(1))
                .add(1, &[1.0])
                .is_ok()
        );
        let mut full = HnswIndex::new(HnswConfig::default().max_vectors(1));
        full.add(1, &[1.0]).unwrap();
        assert!(full.add(2, &[1.0]).is_err());
    }
}
