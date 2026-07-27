//! What a flavor writes back while it observes an event.
//!
//! [`crate::build::ShardBuilder`] is the writing surface of the render, where a
//! flavor states what **is**. This is the writing surface of an event, where it
//! states what it **says**. That split is the whole rule, and it is what keeps
//! [`crate::shard::ShardLogic`] at three methods rather than growing one per
//! feature: a state belongs to the render, which restates it every turn until it
//! stops being true; a word is said once, at a date, and no later render can
//! restate it.
//!
//! This accumulates rather than sends. The shard drains it once `observe` has
//! returned, which keeps a flavor free of any borrow on the shard and lets a
//! test drive `observe` with a scratch `Reply` and read back what came out.
//!
//! Two of the three verbs need nothing but the outbound queues the shard already
//! owns. The third, [`Reply::switch`], is an orchestration between two shards
//! that only a runtime can carry out, so it is recorded as an [`Effect`] and
//! handed to whoever wired the shard up. A shard that belongs to no runtime says
//! so out loud rather than pretending the move happened.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::ids::{ConnectionId, ShardId};

/// One thing a flavor said to one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Word {
    /// Plain speech, shown in the client's message log.
    Say(String),
    /// A refusal, shown wherever the client reports denials.
    Refuse(String),
}

/// Something a flavor asked for that its shard cannot carry out alone.
///
/// Deliberately a value rather than a call: the flavor states what it wants, the
/// runtime decides how, and a shard running outside one can report the gap
/// instead of silently doing nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Effect {
    /// Hand a connection to another shard.
    Move {
        connection: ConnectionId,
        to: ShardId,
    },
}

/// Where a shard sends what it cannot do itself.
///
/// Called on the shard's task, so an implementation must not block and must not
/// await: the gateway's own is a non-blocking send into the runtime's mailbox.
pub type Effects = Arc<dyn Fn(Effect) + Send + Sync>;

/// What a flavor said during one [`crate::shard::ShardLogic::observe`].
#[derive(Debug, Default)]
pub struct Reply {
    words: BTreeMap<ConnectionId, Vec<Word>>,
    effects: Vec<Effect>,
}

impl Reply {
    /// Tell a connection something, in the server's own name.
    pub fn say(&mut self, to: ConnectionId, text: &str) {
        self.words
            .entry(to)
            .or_default()
            .push(Word::Say(text.to_owned()));
    }

    /// Refuse, visibly.
    ///
    /// The point is that the client learns something happened. A flavor that
    /// stays silent leaves the user pressing a button that does nothing, which
    /// is the state this type exists to end.
    pub fn refuse(&mut self, to: ConnectionId, reason: &str) {
        self.words
            .entry(to)
            .or_default()
            .push(Word::Refuse(reason.to_owned()));
    }

    /// Hand a connection to another shard.
    ///
    /// Not a disconnect followed by a connect: the source hands over the view the
    /// client still holds and the destination plans one transition from it. What
    /// the flavor said in the same breath is delivered **first**, so a farewell
    /// reaches the socket before the move is asked for.
    pub fn switch(&mut self, connection: ConnectionId, to: ShardId) {
        self.effects.push(Effect::Move { connection, to });
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.effects.is_empty()
    }

    /// What was said, per connection, in the order it was said. Empties the
    /// reply.
    #[must_use]
    pub fn drain(&mut self) -> BTreeMap<ConnectionId, Vec<Word>> {
        std::mem::take(&mut self.words)
    }

    /// What the flavor asked the runtime for, in the order it asked. Empties the
    /// reply.
    #[must_use]
    pub fn drain_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_keep_their_order_within_a_connection() {
        let mut reply = Reply::default();
        reply.say(ConnectionId(1), "first");
        reply.refuse(ConnectionId(2), "not here");
        reply.say(ConnectionId(1), "second");

        let drained = reply.drain();
        assert_eq!(
            drained.get(&ConnectionId(1)),
            Some(&vec![
                Word::Say("first".to_owned()),
                Word::Say("second".to_owned())
            ])
        );
        assert_eq!(
            drained.get(&ConnectionId(2)),
            Some(&vec![Word::Refuse("not here".to_owned())])
        );
    }

    #[test]
    fn draining_empties_the_reply() {
        let mut reply = Reply::default();
        assert!(reply.is_empty());
        reply.say(ConnectionId(1), "something");
        reply.switch(ConnectionId(1), ShardId(2));
        assert!(!reply.is_empty());

        let _drained = reply.drain();
        assert!(!reply.is_empty(), "the effects are still pending");
        assert_eq!(
            reply.drain_effects(),
            vec![Effect::Move {
                connection: ConnectionId(1),
                to: ShardId(2)
            }]
        );
        assert!(
            reply.is_empty(),
            "a drained reply must not say the same thing twice"
        );
    }
}
