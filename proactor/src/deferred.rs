use crate::CompletionEnvelope;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

pub struct DeferredQueue {
    heap: BinaryHeap<DeferredEntry>,
}

impl DeferredQueue {
    pub fn push(&mut self, sequence: u64, when: Instant, envelope: CompletionEnvelope) {
        self.heap.push(DeferredEntry {
            when,
            sequence,
            envelope,
        });
    }

    pub fn take_ready(&mut self, now: Instant) -> Vec<CompletionEnvelope> {
        let mut ready = Vec::new();
        while self
            .heap
            .peek()
            .is_some_and(|entry| entry.when <= now)
        {
            if let Some(entry) = self.heap.pop() {
                ready.push(entry.envelope);
            }
        }
        ready
    }

    pub fn time_until_next_deadline(&self, now: Instant) -> Option<Duration> {
        self.heap.peek().map(|entry| {
            if entry.when <= now {
                Duration::ZERO
            } else {
                entry.when.saturating_duration_since(now)
            }
        })
    }
}

impl Default for DeferredQueue {
    fn default() -> Self {
        Self {
            heap: BinaryHeap::new(),
        }
    }
}

struct DeferredEntry {
    when: Instant,
    sequence: u64,
    envelope: CompletionEnvelope,
}

impl PartialEq for DeferredEntry {
    fn eq(&self, other: &Self) -> bool {
        self.when == other.when && self.sequence == other.sequence
    }
}

impl Eq for DeferredEntry {}

impl PartialOrd for DeferredEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeferredEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .when
            .cmp(&self.when)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}
