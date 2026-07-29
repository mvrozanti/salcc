//! Score cache keyed by blake3(block text) with an (message_id, block_index)
//! secondary index. `get_or_score` dedupes in-flight work: the first caller
//! for a key runs the scoring future while later callers for the same key
//! await its watch channel — one underlying API call per unique block, ever.
//! Errors are not cached (a later attempt may succeed); waiters of a failed
//! flight receive the error. Bounded at 256 entries: when exceeded, the
//! oldest half (insertion order) is dropped along with its block-index
//! entries. Returned bool is cache_hit: true when served without running
//! the caller's future (completed entry or joined in-flight request).

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::Arc;

use tokio::sync::{Mutex, watch};

use crate::types::SpanScore;

pub const CACHE_CAPACITY: usize = 256;

type Key = [u8; 32];
type Slot = Option<Result<Arc<Vec<SpanScore>>, Arc<str>>>;

pub fn cache_key(text: &str) -> Key {
    blake3::hash(text.as_bytes()).into()
}

#[derive(Default)]
pub struct ScoreCache {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    by_hash: HashMap<Key, Arc<Vec<SpanScore>>>,
    order: VecDeque<Key>,
    by_block: HashMap<(String, usize), Key>,
    in_flight: HashMap<Key, watch::Receiver<Slot>>,
}

impl Inner {
    fn insert(&mut self, key: Key, spans: Arc<Vec<SpanScore>>, message_id: &str, block_index: usize) {
        if self.by_hash.insert(key, spans).is_none() {
            self.order.push_back(key);
        }
        self.by_block.insert((message_id.to_string(), block_index), key);
        if self.by_hash.len() > CACHE_CAPACITY {
            let evicted: HashSet<Key> = self.order.drain(..self.order.len() / 2).collect();
            self.by_hash.retain(|k, _| !evicted.contains(k));
            self.by_block.retain(|_, k| !evicted.contains(k));
            tracing::debug!(evicted = evicted.len(), "score cache evicted oldest half");
        }
    }
}

impl ScoreCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.by_hash.len()
    }

    pub async fn get(&self, text: &str) -> Option<Arc<Vec<SpanScore>>> {
        self.inner.lock().await.by_hash.get(&cache_key(text)).cloned()
    }

    pub async fn get_by_block(
        &self,
        message_id: &str,
        block_index: usize,
    ) -> Option<Arc<Vec<SpanScore>>> {
        let inner = self.inner.lock().await;
        let key = inner.by_block.get(&(message_id.to_string(), block_index))?;
        inner.by_hash.get(key).cloned()
    }

    pub async fn get_or_score<F, Fut>(
        &self,
        message_id: &str,
        block_index: usize,
        text: &str,
        score: F,
    ) -> anyhow::Result<(Arc<Vec<SpanScore>>, bool)>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<Vec<SpanScore>>>,
    {
        let key = cache_key(text);
        let tx = {
            let mut inner = self.inner.lock().await;
            if let Some(spans) = inner.by_hash.get(&key) {
                let spans = spans.clone();
                inner
                    .by_block
                    .insert((message_id.to_string(), block_index), key);
                return Ok((spans, true));
            }
            if let Some(rx) = inner.in_flight.get(&key) {
                let mut rx = rx.clone();
                drop(inner);
                return self.await_in_flight(&mut rx, key, message_id, block_index).await;
            }
            let (tx, rx) = watch::channel(None);
            inner.in_flight.insert(key, rx);
            tx
        };
        let result = score().await;
        let mut inner = self.inner.lock().await;
        inner.in_flight.remove(&key);
        match result {
            Ok(spans) => {
                let spans = Arc::new(spans);
                inner.insert(key, spans.clone(), message_id, block_index);
                let _ = tx.send(Some(Ok(spans.clone())));
                Ok((spans, false))
            }
            Err(err) => {
                let _ = tx.send(Some(Err(Arc::from(err.to_string()))));
                Err(err)
            }
        }
    }

    async fn await_in_flight(
        &self,
        rx: &mut watch::Receiver<Slot>,
        key: Key,
        message_id: &str,
        block_index: usize,
    ) -> anyhow::Result<(Arc<Vec<SpanScore>>, bool)> {
        let outcome = {
            let slot = rx
                .wait_for(|slot| slot.is_some())
                .await
                .map_err(|_| anyhow::anyhow!("in-flight scoring request was dropped"))?;
            slot.clone().expect("wait_for guarantees Some")
        };
        match outcome {
            Ok(spans) => {
                let mut inner = self.inner.lock().await;
                inner
                    .by_block
                    .insert((message_id.to_string(), block_index), key);
                Ok((spans, true))
            }
            Err(message) => Err(anyhow::anyhow!("{message}")),
        }
    }
}
