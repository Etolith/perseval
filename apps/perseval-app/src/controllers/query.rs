use std::collections::HashMap;
use std::hash::Hash;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestGeneration(pub u64);

#[derive(Debug, Clone)]
pub struct ScopedRequestTracker<K> {
    next: u64,
    active: HashMap<K, RequestGeneration>,
}

impl<K> Default for ScopedRequestTracker<K> {
    fn default() -> Self {
        Self {
            next: 0,
            active: HashMap::new(),
        }
    }
}

impl<K> ScopedRequestTracker<K>
where
    K: Eq + Hash,
{
    pub fn begin(&mut self, key: K) -> RequestGeneration {
        self.next = self.next.saturating_add(1);
        let generation = RequestGeneration(self.next);
        self.active.insert(key, generation);
        generation
    }

    pub fn is_current(&self, key: &K, generation: RequestGeneration) -> bool {
        self.active.get(key) == Some(&generation)
    }

    pub fn finish(&mut self, key: &K, generation: RequestGeneration) -> bool {
        if !self.is_current(key, generation) {
            return false;
        }
        self.active.remove(key);
        true
    }

    pub fn cancel(&mut self, key: &K) {
        self.active.remove(key);
    }

    pub fn clear(&mut self) {
        self.active.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_results_cannot_finish_a_newer_request() {
        let mut tracker = ScopedRequestTracker::default();
        let old = tracker.begin("inbox");
        let current = tracker.begin("inbox");

        assert!(!tracker.finish(&"inbox", old));
        assert!(tracker.is_current(&"inbox", current));
        assert!(tracker.finish(&"inbox", current));
    }
}
