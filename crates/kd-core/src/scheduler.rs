//! Deterministic, cycle-driven event scheduler.
//!
//! Every console core drives its hardware from a single `u64` master clock.
//! Hardware blocks (PPU line timing, timers, DMA, FIFO audio, NDS IPC, disc
//! reads, ...) register future events keyed by an [`EventKind`]. Each kind
//! occupies one *slot*: re-scheduling a kind replaces its previous deadline and
//! cancelling it invalidates any pending heap entry. Invalidated entries are
//! dropped lazily when they reach the top of the heap, so both operations are
//! `O(log n)` without having to search the heap.
//!
//! Ties are broken by insertion order, which keeps execution deterministic
//! across runs and therefore safe for save states, replays and netplay.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::fmt::Debug;

/// A schedulable hardware event. Each core defines its own enum.
pub trait EventKind: Copy + Eq + Debug {
    /// Number of distinct slots (one per independently schedulable event).
    const SLOTS: usize;
    /// Index of this event's slot, `< SLOTS`.
    fn slot(self) -> usize;
}

#[derive(Debug, Clone, Copy)]
struct Entry<K> {
    at: u64,
    seq: u64,
    generation: u32,
    kind: K,
}

impl<K> PartialEq for Entry<K> {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at && self.seq == other.seq
    }
}
impl<K> Eq for Entry<K> {}
impl<K> PartialOrd for Entry<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<K> Ord for Entry<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.at.cmp(&other.at).then(self.seq.cmp(&other.seq))
    }
}

/// Min-heap priority queue keyed by master clock cycle.
#[derive(Debug, Clone)]
pub struct Scheduler<K: EventKind> {
    now: u64,
    seq: u64,
    heap: BinaryHeap<Reverse<Entry<K>>>,
    /// Current generation for each slot; heap entries with an older
    /// generation are stale.
    generation: Vec<u32>,
    /// Deadline of the live entry in each slot, if any.
    deadline: Vec<Option<u64>>,
}

impl<K: EventKind> Default for Scheduler<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: EventKind> Scheduler<K> {
    pub fn new() -> Self {
        Self {
            now: 0,
            seq: 0,
            heap: BinaryHeap::with_capacity(K::SLOTS * 2),
            generation: vec![0; K::SLOTS],
            deadline: vec![None; K::SLOTS],
        }
    }

    /// Current master clock cycle.
    #[inline]
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Advance the master clock. Does not dispatch events; call
    /// [`Scheduler::pop_due`] afterwards.
    #[inline]
    pub fn advance(&mut self, cycles: u64) {
        self.now += cycles;
    }

    /// Jump the clock forward to `cycle` (never backwards).
    #[inline]
    pub fn advance_to(&mut self, cycle: u64) {
        if cycle > self.now {
            self.now = cycle;
        }
    }

    /// Schedule `kind` at absolute cycle `at`, replacing any pending instance.
    pub fn schedule_at(&mut self, at: u64, kind: K) {
        let slot = kind.slot();
        debug_assert!(slot < K::SLOTS, "event slot {slot} out of range for {kind:?}");
        self.generation[slot] = self.generation[slot].wrapping_add(1);
        self.deadline[slot] = Some(at);
        self.seq += 1;
        self.heap.push(Reverse(Entry { at, seq: self.seq, generation: self.generation[slot], kind }));
    }

    /// Schedule `kind` `delta` cycles after the current clock.
    #[inline]
    pub fn schedule_in(&mut self, delta: u64, kind: K) {
        self.schedule_at(self.now + delta, kind);
    }

    /// Cancel any pending instance of `kind`.
    pub fn cancel(&mut self, kind: K) {
        let slot = kind.slot();
        if self.deadline[slot].take().is_some() {
            self.generation[slot] = self.generation[slot].wrapping_add(1);
        }
    }

    /// Absolute deadline of `kind` if it is pending.
    #[inline]
    pub fn deadline_of(&self, kind: K) -> Option<u64> {
        self.deadline[kind.slot()]
    }

    fn discard_stale(&mut self) {
        while let Some(Reverse(top)) = self.heap.peek() {
            let slot = top.kind.slot();
            if self.generation[slot] == top.generation && self.deadline[slot].is_some() {
                break;
            }
            self.heap.pop();
        }
    }

    /// Cycle of the earliest live event.
    pub fn next_deadline(&mut self) -> Option<u64> {
        self.discard_stale();
        self.heap.peek().map(|Reverse(e)| e.at)
    }

    /// Cycles until the next event (0 if overdue), or `None` when idle.
    pub fn cycles_until_next(&mut self) -> Option<u64> {
        self.next_deadline().map(|at| at.saturating_sub(self.now))
    }

    /// Pop the earliest event whose deadline is `<= now`, returning the
    /// cycle it was scheduled for (which may be in the past, so handlers can
    /// compensate for overshoot) and its kind.
    pub fn pop_due(&mut self) -> Option<(u64, K)> {
        self.discard_stale();
        match self.heap.peek() {
            Some(Reverse(e)) if e.at <= self.now => {
                let Reverse(e) = self.heap.pop().expect("peeked entry");
                self.deadline[e.kind.slot()] = None;
                Some((e.at, e.kind))
            }
            _ => None,
        }
    }

    /// Number of live events (stale heap entries are not counted).
    pub fn pending(&self) -> usize {
        self.deadline.iter().filter(|d| d.is_some()).count()
    }

    /// Live events as `(deadline, kind)` sorted by deadline; used by save
    /// states and the mobile debugger.
    pub fn snapshot(&self) -> Vec<(u64, K)> {
        let mut live: Vec<Entry<K>> = self
            .heap
            .iter()
            .map(|Reverse(e)| *e)
            .filter(|e| {
                let s = e.kind.slot();
                self.generation[s] == e.generation && self.deadline[s] == Some(e.at)
            })
            .collect();
        live.sort();
        live.into_iter().map(|e| (e.at, e.kind)).collect()
    }

    /// Rebuild from a snapshot (save-state load).
    pub fn restore(&mut self, now: u64, events: &[(u64, K)]) {
        *self = Self::new();
        self.now = now;
        for &(at, kind) in events {
            self.schedule_at(at, kind);
        }
    }

    /// Rebase all timestamps so that `now` becomes small again. Cores call
    /// this once per frame to keep the clock comfortably inside `u64` and so
    /// that save states stay compact. Returns the amount subtracted.
    pub fn rebase(&mut self) -> u64 {
        let base = self.now;
        let events = self.snapshot();
        let shifted: Vec<(u64, K)> = events.into_iter().map(|(at, k)| (at.saturating_sub(base), k)).collect();
        self.restore(0, &shifted);
        base
    }
}

/// Event kinds shared by more than one core. Cores with additional hardware
/// define their own enum; this one is used by the generic tests and by simple
/// front-end timers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommonEvent {
    HDraw,
    HBlank,
    VBlank,
    TimerOverflow(u8),
    DmaTransfer(u8),
    AudioSample,
    AudioFifo(u8),
    NdsIpcSync,
    DiscRead,
    FrameEnd,
}

impl EventKind for CommonEvent {
    const SLOTS: usize = 24;
    fn slot(self) -> usize {
        match self {
            CommonEvent::HDraw => 0,
            CommonEvent::HBlank => 1,
            CommonEvent::VBlank => 2,
            CommonEvent::TimerOverflow(n) => 3 + (n as usize & 3),
            CommonEvent::DmaTransfer(n) => 7 + (n as usize & 7),
            CommonEvent::AudioSample => 15,
            CommonEvent::AudioFifo(n) => 16 + (n as usize & 1),
            CommonEvent::NdsIpcSync => 18,
            CommonEvent::DiscRead => 19,
            CommonEvent::FrameEnd => 20,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pops_in_deadline_order() {
        let mut s = Scheduler::<CommonEvent>::new();
        s.schedule_at(300, CommonEvent::VBlank);
        s.schedule_at(100, CommonEvent::HBlank);
        s.schedule_at(200, CommonEvent::TimerOverflow(2));
        assert_eq!(s.next_deadline(), Some(100));
        assert_eq!(s.pop_due(), None);
        s.advance(1000);
        assert_eq!(s.pop_due(), Some((100, CommonEvent::HBlank)));
        assert_eq!(s.pop_due(), Some((200, CommonEvent::TimerOverflow(2))));
        assert_eq!(s.pop_due(), Some((300, CommonEvent::VBlank)));
        assert_eq!(s.pop_due(), None);
        assert_eq!(s.pending(), 0);
    }

    #[test]
    fn ties_are_fifo() {
        let mut s = Scheduler::<CommonEvent>::new();
        s.schedule_at(50, CommonEvent::DmaTransfer(1));
        s.schedule_at(50, CommonEvent::DmaTransfer(0));
        s.schedule_at(50, CommonEvent::AudioSample);
        s.advance(50);
        assert_eq!(s.pop_due().unwrap().1, CommonEvent::DmaTransfer(1));
        assert_eq!(s.pop_due().unwrap().1, CommonEvent::DmaTransfer(0));
        assert_eq!(s.pop_due().unwrap().1, CommonEvent::AudioSample);
    }

    #[test]
    fn reschedule_replaces_and_cancel_removes() {
        let mut s = Scheduler::<CommonEvent>::new();
        s.schedule_at(10, CommonEvent::TimerOverflow(0));
        s.schedule_at(500, CommonEvent::TimerOverflow(0));
        s.schedule_at(20, CommonEvent::TimerOverflow(1));
        s.cancel(CommonEvent::TimerOverflow(1));
        assert_eq!(s.pending(), 1);
        assert_eq!(s.deadline_of(CommonEvent::TimerOverflow(0)), Some(500));
        assert_eq!(s.deadline_of(CommonEvent::TimerOverflow(1)), None);
        s.advance(100);
        assert_eq!(s.pop_due(), None, "stale entries must not fire");
        s.advance(400);
        assert_eq!(s.pop_due(), Some((500, CommonEvent::TimerOverflow(0))));
    }

    #[test]
    fn cancelled_then_rescheduled_slot_fires_once() {
        let mut s = Scheduler::<CommonEvent>::new();
        s.schedule_at(10, CommonEvent::DiscRead);
        s.cancel(CommonEvent::DiscRead);
        s.schedule_at(10, CommonEvent::DiscRead);
        s.advance(10);
        assert_eq!(s.pop_due(), Some((10, CommonEvent::DiscRead)));
        assert_eq!(s.pop_due(), None);
    }

    #[test]
    fn rebase_preserves_relative_deadlines() {
        let mut s = Scheduler::<CommonEvent>::new();
        s.advance(1_000_000);
        s.schedule_in(25, CommonEvent::HBlank);
        s.schedule_in(80, CommonEvent::VBlank);
        let base = s.rebase();
        assert_eq!(base, 1_000_000);
        assert_eq!(s.now(), 0);
        assert_eq!(s.snapshot(), vec![(25, CommonEvent::HBlank), (80, CommonEvent::VBlank)]);
    }

    #[test]
    fn heavy_churn_stays_consistent() {
        let mut s = Scheduler::<CommonEvent>::new();
        let mut fired = 0u32;
        for i in 0..10_000u64 {
            s.schedule_in(5, CommonEvent::TimerOverflow((i % 4) as u8));
            s.advance(1);
            while s.pop_due().is_some() {
                fired += 1;
            }
        }
        // Each timer slot is rescheduled every 4 cycles before it can fire,
        // except the very last ones still pending.
        assert_eq!(fired, 0);
        assert_eq!(s.pending(), 4);
        s.advance(10);
        while s.pop_due().is_some() {
            fired += 1;
        }
        assert_eq!(fired, 4);
    }
}
