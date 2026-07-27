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
//! Nothing here reaches another shard: what the shard owns is the outbound queue
//! of every connection attached to it, and that is exactly what these verbs use.
//! Moving a connection is an orchestration between two shards, so it stays with
//! the runtime handle that knows them both.

use std::collections::BTreeMap;

use crate::ids::ConnectionId;

/// One thing a flavor said to one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Word {
    /// Plain speech, shown in the client's message log.
    Say(String),
    /// A refusal, shown wherever the client reports denials.
    Refuse(String),
}

/// What a flavor said during one [`crate::shard::ShardLogic::observe`].
#[derive(Debug, Default)]
pub struct Reply {
    words: BTreeMap<ConnectionId, Vec<Word>>,
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

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// What was said, per connection, in the order it was said. Empties the
    /// reply.
    #[must_use]
    pub fn drain(&mut self) -> BTreeMap<ConnectionId, Vec<Word>> {
        std::mem::take(&mut self.words)
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
        assert!(!reply.is_empty());

        let _drained = reply.drain();
        assert!(
            reply.is_empty(),
            "a drained reply must not say the same thing twice"
        );
    }
}
