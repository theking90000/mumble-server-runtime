//! The shard's delta journal.
//!
//! A shard renders once and journals one delta per version. Connections replay
//! the slice they have not seen yet, which is what makes a delta cost
//! `O(N·|D|)` to distribute rather than `O(N·W)`.
//!
//! # There is no snapshot mechanism to write
//!
//! A newcomer receives `plan(empty view, current view)` - the ordinary planner.
//! A departure is the inverse. A migration is both. Nothing here needs to know
//! about any of that, which is why this module has no view vocabulary at all.
//!
//! # Falling off the tail closes the connection
//!
//! A connection whose cursor has dropped below [`Journal::tail`] cannot be
//! repaired from deltas, and it is dying anyway: its output queue holds a
//! thousand messages. It is closed and reconnects from scratch.
//!
//! REF: docs/design/guide-implementation.md 8.1

use std::collections::VecDeque;

use thiserror::Error;

use crate::plan::PlannedOp;

/// How many versions of history the journal keeps.
///
/// A starting point, not a truth (guide 17.3). It bounds memory, and it is the
/// distance beyond which a connection is declared unrecoverable.
pub const DEPTH: usize = 256;

/// A connection asked for history the journal no longer holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "cursor {cursor} is below the journal tail {tail}: this connection has missed too much to be \
     repaired from deltas"
)]
pub struct TooFarBehind {
    pub cursor: u64,
    pub tail: u64,
}

/// Deltas for versions `tail + 1 ..= head`.
#[derive(Debug, Default)]
pub struct Journal {
    tail: u64,
    head: u64,
    entries: VecDeque<Vec<PlannedOp>>,
}

impl Journal {
    /// An empty journal at version zero.
    #[must_use]
    pub fn new() -> Journal {
        Journal {
            tail: 0,
            head: 0,
            entries: VecDeque::new(),
        }
    }

    /// The newest version.
    #[must_use]
    pub fn head(&self) -> u64 {
        self.head
    }

    /// The oldest version still replayable *from*.
    #[must_use]
    pub fn tail(&self) -> u64 {
        self.tail
    }

    /// Append a delta, returning the version it became.
    ///
    /// Evicts from the front past [`DEPTH`], which is the only thing that moves
    /// the tail.
    pub fn push(&mut self, ops: Vec<PlannedOp>) -> u64 {
        self.head = self.head.saturating_add(1);
        self.entries.push_back(ops);
        while self.entries.len() > DEPTH {
            let _evicted = self.entries.pop_front();
            self.tail = self.tail.saturating_add(1);
        }
        self.head
    }

    /// Every operation in versions `from + 1 ..= to`, in order.
    ///
    /// # Errors
    ///
    /// [`TooFarBehind`] when `from` is below the tail. A `to` above the head, or
    /// a `to` below `from`, yields nothing: both mean the caller is already up to
    /// date or ahead, which is not an error a connection can act on.
    pub fn replay(&self, from: u64, to: u64) -> Result<Vec<&PlannedOp>, TooFarBehind> {
        if from < self.tail {
            return Err(TooFarBehind {
                cursor: from,
                tail: self.tail,
            });
        }
        let to = to.min(self.head);
        if to <= from {
            return Ok(Vec::new());
        }

        // Version v lives at index `v - tail - 1`; both subtractions are safe
        // because `from >= tail` and `v > from`.
        let start = usize::try_from(from - self.tail).unwrap_or(usize::MAX);
        let end = usize::try_from(to - self.tail).unwrap_or(usize::MAX);
        Ok(self
            .entries
            .iter()
            .take(end)
            .skip(start)
            .flatten()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::ids::{ChannelId, SessionId};
    use crate::plan::PlanOp;
    use crate::scope::Scope;

    fn delta(marker: u32) -> Vec<PlannedOp> {
        vec![PlannedOp {
            op: PlanOp::RemoveUser(SessionId(marker)),
            scope: Scope::ROOT,
        }]
    }

    fn markers(ops: &[&PlannedOp]) -> Vec<u32> {
        ops.iter()
            .filter_map(|planned| match planned.op {
                PlanOp::RemoveUser(session) => Some(session.0),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn replaying_in_pieces_equals_replaying_at_once() {
        let mut journal = Journal::new();
        for marker in 1..=10 {
            journal.push(delta(marker));
        }

        let whole = journal.replay(0, 10).expect("within the journal");
        let mut pieced: Vec<u32> = Vec::new();
        for (from, to) in [(0, 3), (3, 3), (3, 7), (7, 10)] {
            pieced.extend(markers(
                &journal.replay(from, to).expect("within the journal"),
            ));
        }

        assert_eq!(markers(&whole), pieced);
        assert_eq!(markers(&whole), (1..=10).collect::<Vec<u32>>());
    }

    #[test]
    fn a_cursor_at_the_head_replays_nothing() {
        let mut journal = Journal::new();
        journal.push(delta(1));
        assert!(
            journal
                .replay(journal.head(), journal.head())
                .expect("within the journal")
                .is_empty()
        );
    }

    #[test]
    fn a_cursor_below_the_tail_is_detected_rather_than_silently_truncated() {
        let mut journal = Journal::new();
        for marker in 0..u32::try_from(DEPTH).unwrap_or(256) + 5 {
            journal.push(delta(marker));
        }

        assert_eq!(journal.tail(), 5, "five versions were evicted");
        assert_eq!(
            journal.replay(4, journal.head()),
            Err(TooFarBehind { cursor: 4, tail: 5 })
        );
        assert!(
            journal.replay(5, journal.head()).is_ok(),
            "the tail itself is still a valid cursor"
        );
    }

    #[test]
    fn the_journal_never_grows_past_its_depth() {
        let mut journal = Journal::new();
        for marker in 0..u32::try_from(DEPTH).unwrap_or(256) * 3 {
            journal.push(delta(marker));
        }
        assert_eq!(journal.entries.len(), DEPTH);
        assert_eq!(journal.head() - journal.tail(), DEPTH as u64);
    }

    #[test]
    fn asking_beyond_the_head_stops_at_the_head() {
        let mut journal = Journal::new();
        journal.push(delta(1));
        journal.push(delta(2));

        let ops = journal.replay(0, 999).expect("within the journal");
        assert_eq!(markers(&ops), vec![1, 2]);
    }

    #[test]
    fn an_empty_delta_still_advances_the_version() {
        let mut journal = Journal::new();
        let version = journal.push(Vec::new());
        assert_eq!(version, 1);
        assert_eq!(journal.head(), 1);
        assert_eq!(
            journal.replay(0, 1).expect("within the journal").len(),
            0,
            "a version with no operations replays as nothing"
        );
        let _ = ChannelId::ROOT;
    }
}
