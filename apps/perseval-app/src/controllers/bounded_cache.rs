use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

#[derive(Debug, Clone)]
pub struct BoundedPageCache<K, V> {
    capacity: usize,
    pages: HashMap<K, V>,
    recency: VecDeque<K>,
}

impl<K, V> BoundedPageCache<K, V>
where
    K: Clone + Eq + Hash,
{
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            pages: HashMap::new(),
            recency: VecDeque::new(),
        }
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        self.touch(&key);
        self.pages.insert(key.clone(), value);
        if self.pages.len() <= self.capacity {
            return None;
        }
        let evicted_key = self.recency.pop_front()?;
        self.pages
            .remove(&evicted_key)
            .map(|value| (evicted_key, value))
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        if !self.pages.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.pages.get(key)
    }

    pub fn peek(&self, key: &K) -> Option<&V> {
        self.pages.get(key)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.pages.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.pages.values()
    }

    pub fn clear(&mut self) {
        self.pages.clear();
        self.recency.clear();
    }

    fn touch(&mut self, key: &K) {
        self.recency.retain(|candidate| candidate != key);
        self.recency.push_back(key.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn least_recently_used_page_is_evicted() {
        let mut cache = BoundedPageCache::new(2);
        cache.insert(1, "one");
        cache.insert(2, "two");
        assert_eq!(cache.get(&1), Some(&"one"));

        assert_eq!(cache.insert(3, "three"), Some((2, "two")));
        assert!(cache.contains_key(&1));
        assert!(cache.contains_key(&3));
        assert!(!cache.contains_key(&2));
    }

    #[test]
    fn capacity_is_never_unbounded_or_zero() {
        assert_eq!(BoundedPageCache::<u8, u8>::new(0).capacity(), 1);
    }
}
