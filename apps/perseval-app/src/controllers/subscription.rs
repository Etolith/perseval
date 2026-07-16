#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceStatus {
    InOrder,
    Duplicate,
    Gap { expected: u64, received: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SequenceTracker {
    last_committed: u64,
}

impl SequenceTracker {
    pub fn from_snapshot(sequence: u64) -> Self {
        Self {
            last_committed: sequence,
        }
    }

    pub fn last_committed(&self) -> u64 {
        self.last_committed
    }

    pub fn observe(&mut self, sequence: u64) -> SequenceStatus {
        if sequence <= self.last_committed {
            return SequenceStatus::Duplicate;
        }
        let expected = self.last_committed.saturating_add(1);
        self.last_committed = sequence;
        if sequence == expected {
            SequenceStatus::InOrder
        } else {
            SequenceStatus::Gap {
                expected,
                received: sequence,
            }
        }
    }

    pub fn resync(&mut self, sequence: u64) {
        self.last_committed = sequence;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_and_gaps_are_explicit() {
        let mut tracker = SequenceTracker::from_snapshot(10);
        assert_eq!(tracker.observe(10), SequenceStatus::Duplicate);
        assert_eq!(tracker.observe(11), SequenceStatus::InOrder);
        assert_eq!(
            tracker.observe(14),
            SequenceStatus::Gap {
                expected: 12,
                received: 14
            }
        );
    }
}
