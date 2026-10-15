//! Embedding models: text in, vectors out (design 0008 §16.1), for
//! semantic memory search.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::{ModelError, ModelErrorKind};
use crate::model::BoxFuture;

/// An embedding model: turns texts into vectors whose closeness reflects
/// closeness of meaning. Implemented by
/// [`ProviderEmbedder`](crate::ProviderEmbedder) (OpenAI-compatible
/// servers) and [`FakeEmbedder`] for tests.
pub trait Embedder: Send + Sync + 'static {
    /// Provider name, e.g. `openai`.
    fn provider(&self) -> &str;

    /// Model name, e.g. `text-embedding-3-small`.
    fn model(&self) -> &str;

    /// One vector per text, in the same order.
    fn embed<'a>(&'a self, texts: &'a [String])
    -> BoxFuture<'a, Result<Vec<Vec<f32>>, ModelError>>;
}

impl<E: Embedder + ?Sized> Embedder for Arc<E> {
    fn provider(&self) -> &str {
        (**self).provider()
    }

    fn model(&self) -> &str {
        (**self).model()
    }

    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, ModelError>> {
        (**self).embed(texts)
    }
}

/// The dimensions of [`FakeEmbedder`] vectors.
pub const FAKE_DIMENSIONS: usize = 256;

/// A deterministic embedder for tests (never a real model): each word of a
/// text adds to one of 256 dimensions (by hash), and the vector is scaled
/// to unit length. Texts that share words are close; [`synonym`](Self::synonym)
/// makes different words count as the same, to test search by meaning.
#[derive(Debug, Clone, Default)]
pub struct FakeEmbedder {
    synonyms: HashMap<String, String>,
    failing: bool,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl FakeEmbedder {
    /// An embedder with no synonyms.
    pub fn new() -> Self {
        Self::default()
    }

    /// Treats `word` as `same_as` (both lower-cased).
    pub fn synonym(mut self, word: &str, same_as: &str) -> Self {
        self.synonyms
            .insert(word.to_lowercase(), same_as.to_lowercase());
        self
    }

    /// An embedder whose every call fails (`server`).
    pub fn failing() -> Self {
        Self {
            failing: true,
            ..Self::default()
        }
    }

    /// The texts of every call so far, in order.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The vector of one text.
    pub fn vector(&self, text: &str) -> Vec<f32> {
        let mut vector = vec![0f32; FAKE_DIMENSIONS];
        for word in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            let word = word.to_lowercase();
            let word = self.synonyms.get(&word).unwrap_or(&word);
            vector[fnv(word) % FAKE_DIMENSIONS] += 1.0;
        }
        let length = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if length > 0.0 {
            vector.iter_mut().for_each(|x| *x /= length);
        }
        vector
    }
}

/// FNV-1a: stable across runs and platforms.
fn fnv(word: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in word.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash as usize
}

impl Embedder for FakeEmbedder {
    fn provider(&self) -> &str {
        "fake"
    }

    fn model(&self) -> &str {
        "fake-embedding"
    }

    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, ModelError>> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(texts.to_vec());
        let result = if self.failing {
            Err(
                ModelError::new(ModelErrorKind::Server, "fake embedder failure")
                    .for_model("fake", "fake-embedding"),
            )
        } else {
            Ok(texts.iter().map(|text| self.vector(text)).collect())
        };
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[tokio::test]
    async fn fake_vectors_are_unit_length_and_close_for_shared_words() {
        let embedder = FakeEmbedder::new().synonym("antibiotics", "penicillin");
        let texts = vec![
            "Severe penicillin allergy".to_string(),
            "Which antibiotics should I avoid?".to_string(),
            "Lives in Hyderabad".to_string(),
            String::new(),
        ];
        let vectors = embedder.embed(&texts).await.unwrap();
        assert_eq!(vectors.len(), 4);
        assert!(vectors.iter().all(|v| v.len() == FAKE_DIMENSIONS));
        assert!((cosine(&vectors[0], &vectors[0]) - 1.0).abs() < 1e-5);
        assert!(vectors[3].iter().all(|x| *x == 0.0));
        // The synonym makes the question close to the allergy only.
        assert!(cosine(&vectors[1], &vectors[0]) > cosine(&vectors[1], &vectors[2]));
        assert_eq!(embedder.vector("A b"), embedder.vector("b a"));
        assert_eq!(embedder.calls(), vec![texts]);

        let err = FakeEmbedder::failing().embed(&[]).await.unwrap_err();
        assert_eq!(err.kind, ModelErrorKind::Server);
    }
}
