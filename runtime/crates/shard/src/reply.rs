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
//! Most of the verbs need nothing but the outbound queues the shard already
//! owns. [`Reply::relay`] and [`Reply::announce`] need one thing more - the view,
//! to turn an [`Audience`] into the connections that make it up - which is why
//! they record what to deliver rather than to whom, and the shard expands them
//! when it drains. [`Reply::switch`] is an orchestration between two shards that
//! only a runtime can carry out, so it is recorded as an [`Effect`] and handed to
//! whoever wired the shard up. A shard that belongs to no runtime says so out
//! loud rather than pretending the move happened.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::ids::{ChannelKey, ConnectionId, Occupant, ShardId};

/// One thing a flavor said to one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Word {
    /// Plain speech, shown in the client's message log.
    Say(String),
    /// A refusal, shown wherever the client reports denials.
    Refuse(String),
}

/// Who a message is for, in the flavor's own vocabulary.
///
/// Keys and occupants rather than wire identifiers, exactly like
/// [`crate::shard::ActionTarget`]: the flavor names what it rendered, and the
/// shard is what turns that into the sessions and channel ids a client holds.
///
/// Expanding an audience needs the view, which a [`Reply`] deliberately does not
/// have, so the expansion happens when the shard drains what was said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Everyone shown in that channel.
    Channel(ChannelKey),
    /// Everyone shown in that channel or in any channel below it.
    Tree(ChannelKey),
    /// One occupant, privately.
    User(Occupant),
}

/// One message a flavor asked to have delivered to an audience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spoken {
    /// Who it is attributed to. `None` means the server itself, which is what
    /// makes the client label it as coming from the server rather than a user.
    pub from: Option<ConnectionId>,
    pub to: Audience,
    pub text: String,
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
    spoken: Vec<Spoken>,
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

    /// Deliver `text` to `to`, attributed to `from`.
    ///
    /// The answer to a [`crate::shard::VoiceEvent::Said`] a flavor is willing to
    /// carry out, and the one place a client's own words reach other clients.
    /// The flavor stays in charge of the audience: relaying somewhere other than
    /// where the message was aimed is a rewrite, not a workaround.
    ///
    /// Two rules the shard applies when it expands this, both borrowed from
    /// elsewhere in the model rather than invented here:
    ///
    /// - The sender never receives its own message.
    /// - A recipient that cannot see `from` is skipped. That is the audio
    ///   coupling rule - a receiver must see the sender - applied to text, and
    ///   naming a session the recipient does not hold would break the view
    ///   invariants anyway. Use [`Reply::announce`] for something everyone
    ///   should read whoever said it.
    pub fn relay(&mut self, from: ConnectionId, to: Audience, text: &str) {
        self.spoken.push(Spoken {
            from: Some(from),
            to,
            text: text.to_owned(),
        });
    }

    /// Deliver `text` to `to`, in the server's own name.
    ///
    /// [`Reply::say`] addressed to a group: no actor, so no recipient is skipped
    /// for not seeing one.
    pub fn announce(&mut self, to: Audience, text: &str) {
        self.spoken.push(Spoken {
            from: None,
            to,
            text: text.to_owned(),
        });
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
        self.words.is_empty() && self.spoken.is_empty() && self.effects.is_empty()
    }

    /// What was said, per connection, in the order it was said. Empties the
    /// reply.
    #[must_use]
    pub fn drain(&mut self) -> BTreeMap<ConnectionId, Vec<Word>> {
        std::mem::take(&mut self.words)
    }

    /// What was addressed to an audience, in the order it was said. Empties the
    /// reply.
    #[must_use]
    pub fn drain_spoken(&mut self) -> Vec<Spoken> {
        std::mem::take(&mut self.spoken)
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
    use crate::ids::ChannelKey;

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
    fn an_audience_keeps_what_it_was_told_and_who_said_it() {
        let mut reply = Reply::default();
        reply.relay(
            ConnectionId(1),
            Audience::Channel(ChannelKey(7)),
            "hello team",
        );
        reply.announce(Audience::Tree(ChannelKey(0)), "the round is over");

        assert_eq!(
            reply.drain_spoken(),
            vec![
                Spoken {
                    from: Some(ConnectionId(1)),
                    to: Audience::Channel(ChannelKey(7)),
                    text: "hello team".to_owned(),
                },
                Spoken {
                    from: None,
                    to: Audience::Tree(ChannelKey(0)),
                    text: "the round is over".to_owned(),
                },
            ]
        );
        assert!(reply.is_empty(), "draining must not leave a copy behind");
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
