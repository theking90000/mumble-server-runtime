//! One connection's bounded output queue.
//!
//! The queue is the **commit point**: accepted into the queue *is* committed,
//! because acceptance is synchronous and the stream underneath delivers in order
//! or not at all. That only works if a whole transition is admitted all at once,
//! which is what [`OutboundQueue::try_send_all`] guarantees by reserving every
//! slot before writing any of them. A half-delivered transition would leave the
//! client in a state nobody can describe.
//!
//! Admission never blocks. Connections push view updates onto each other's
//! queues, so a blocking push would let one slow client stall every task trying
//! to notify it, and two connections pushing to each other with both queues full
//! would deadlock outright. Non-blocking makes both impossible by construction.

use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use tokio::sync::mpsc;
use mumble_server_runtime_protocol::ControlMessage;

/// How many messages one connection's queue holds.
pub const CAPACITY: usize = 1024;

/// The depth past which tunnelled voice is refused.
///
/// Voice is bounded in **latency**, not in volume: at a speaker's usual 10 ms of
/// framing, 64 queued messages is roughly 640 ms of backlog, and a packet that
/// late is worth nothing to anyone. Control messages have no such bound because
/// a skipped `UserState` leaves the client on a view that silently diverges.
///
/// One queue rather than two, because the client discards audio whose sender
/// session it does not know: the `UserState` introducing a speaker has to reach
/// the client before that speaker's tunnelled audio, and two independent queues
/// cannot promise that.
///
/// REF: references/mumble/src/mumble/ServerHandler.cpp : `handleVoicePacket`
///   looks the sender up with `ClientUser::get(senderSession)` and drops the
///   packet when it is absent.
pub const MAX_DEPTH_FOR_VOICE: usize = 64;

/// What became of a tunnelled voice packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceAdmission {
    Accepted,
    /// The queue was too deep for the packet to still be worth hearing. Counted
    /// rather than silent (R6), and never a reason to end the connection.
    Dropped,
}

/// Why a transition could not be admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum Refused {
    /// Not enough free slots right now. Nothing was queued, the committed state
    /// stays where it is, and the next attempt is planned against a fresher
    /// desired state. Ordinary backpressure, not a fault.
    #[error("output queue has {free} free slots, {needed} were needed")]
    Congested { needed: usize, free: usize },

    /// Larger than the queue can *ever* hold. Retrying would never succeed, so
    /// this is a livelock rather than backpressure and the connection is torn
    /// down: reconnecting rebuilds the view from scratch.
    #[error("transition of {needed} messages exceeds the queue capacity of {capacity}")]
    TooLarge { needed: usize, capacity: usize },

    /// The connection's writer has ended.
    #[error("the connection's writer has ended")]
    Closed,
}

/// The sending half of one connection's output queue.
#[derive(Debug)]
pub struct OutboundQueue {
    sender: mpsc::Sender<ControlMessage>,
    /// Set once a transition could never fit, or the writer is gone. The owning
    /// task polls it and ends the connection.
    must_close: AtomicBool,
}

impl OutboundQueue {
    /// Create a queue, returning the receiving half for the connection's writer.
    #[must_use]
    pub fn new() -> (OutboundQueue, mpsc::Receiver<ControlMessage>) {
        Self::with_capacity(CAPACITY)
    }

    /// Create a queue of a given depth. Exists so tests can produce congestion
    /// without queueing a thousand messages first.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> (OutboundQueue, mpsc::Receiver<ControlMessage>) {
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        (
            OutboundQueue {
                sender,
                must_close: AtomicBool::new(false),
            },
            receiver,
        )
    }

    /// Admit a whole transition, all of it or none of it.
    ///
    /// Atomicity comes from reserving every slot before sending any of them: a
    /// permit holds capacity, and dropping the collected permits releases them
    /// without having written anything. Failing halfway is therefore impossible
    /// rather than merely unlikely.
    ///
    /// # Errors
    ///
    /// [`Refused`], with nothing queued in every case.
    pub fn try_send_all(&self, messages: Vec<ControlMessage>) -> Result<(), Refused> {
        let needed = messages.len();
        if needed == 0 {
            return Ok(());
        }

        let capacity = self.sender.max_capacity();
        if needed > capacity {
            self.must_close.store(true, Ordering::Relaxed);
            return Err(Refused::TooLarge { needed, capacity });
        }

        let mut permits = Vec::with_capacity(needed);
        for _ in 0..needed {
            match self.sender.try_reserve() {
                Ok(permit) => permits.push(permit),
                Err(mpsc::error::TrySendError::Full(())) => {
                    // Every permit taken so far is released by this return, so
                    // the queue is left exactly as it was found.
                    return Err(Refused::Congested {
                        needed,
                        free: permits.len(),
                    });
                }
                Err(mpsc::error::TrySendError::Closed(())) => {
                    self.must_close.store(true, Ordering::Relaxed);
                    return Err(Refused::Closed);
                }
            }
        }

        for (permit, message) in permits.into_iter().zip(messages) {
            permit.send(message);
        }
        Ok(())
    }

    /// Offer one tunnelled voice packet, dropping it if the queue is already
    /// too deep for it to arrive in time.
    ///
    /// Deliberately not `try_send_all`: a refused transition is retried, while a
    /// refused voice packet is gone, and conflating the two would either close
    /// connections over lost audio or replay stale speech.
    pub fn push_voice(&self, message: ControlMessage) -> VoiceAdmission {
        if self.depth() >= MAX_DEPTH_FOR_VOICE {
            return VoiceAdmission::Dropped;
        }
        match self.sender.try_send(message) {
            Ok(()) => VoiceAdmission::Accepted,
            Err(mpsc::error::TrySendError::Full(_)) => VoiceAdmission::Dropped,
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.must_close.store(true, Ordering::Relaxed);
                VoiceAdmission::Dropped
            }
        }
    }

    /// Whether the owning task must end this connection.
    #[must_use]
    pub fn must_close(&self) -> bool {
        self.must_close.load(Ordering::Relaxed)
    }

    /// Mark this connection for teardown.
    pub fn mark_fatal(&self) {
        self.must_close.store(true, Ordering::Relaxed);
    }

    /// Messages currently queued.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.sender
            .max_capacity()
            .saturating_sub(self.sender.capacity())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use mumble_server_runtime_protocol::messages::tcp;

    fn message(session: u32) -> ControlMessage {
        ControlMessage::UserRemove(tcp::UserRemove {
            session,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn an_admitted_transition_arrives_whole_and_in_order() {
        let (queue, mut receiver) = OutboundQueue::new();
        queue
            .try_send_all(vec![message(1), message(2), message(3)])
            .expect("an empty queue has room");

        let mut received = Vec::new();
        for _ in 0..3 {
            received.push(receiver.recv().await.expect("queued message"));
        }
        assert_eq!(
            received,
            vec![message(1), message(2), message(3)],
            "plan order is what carries the ordering rules"
        );
    }

    #[tokio::test]
    async fn a_transition_that_does_not_fit_queues_nothing_at_all() {
        let (queue, mut receiver) = OutboundQueue::with_capacity(4);
        queue
            .try_send_all(vec![message(1), message(2)])
            .expect("room for two");
        let depth_before = queue.depth();

        let refused = queue
            .try_send_all(vec![message(3); 3])
            .expect_err("three messages cannot fit in two slots");
        assert_eq!(refused, Refused::Congested { needed: 3, free: 2 });
        assert_eq!(
            queue.depth(),
            depth_before,
            "a refused transition must leave the queue exactly as it was"
        );
        assert!(
            !queue.must_close(),
            "ordinary backpressure is not a reason to drop the connection"
        );

        receiver.recv().await.expect("queued message");
        queue
            .try_send_all(vec![message(3); 3])
            .expect("one drained slot is all that was missing");
    }

    #[test]
    fn a_transition_larger_than_the_queue_ends_the_connection() {
        let (queue, _receiver) = OutboundQueue::with_capacity(4);
        let refused = queue
            .try_send_all(vec![message(1); 5])
            .expect_err("it cannot fit, now or ever");

        assert_eq!(
            refused,
            Refused::TooLarge {
                needed: 5,
                capacity: 4
            }
        );
        assert!(
            queue.must_close(),
            "retrying something that can never fit is a livelock, not backpressure"
        );
    }

    #[test]
    fn a_closed_writer_ends_the_connection() {
        let (queue, receiver) = OutboundQueue::with_capacity(4);
        drop(receiver);

        assert_eq!(queue.try_send_all(vec![message(1)]), Err(Refused::Closed));
        assert!(queue.must_close());
    }

    #[test]
    fn voice_is_refused_long_before_the_queue_is_full() {
        let (queue, _receiver) = OutboundQueue::new();
        for _ in 0..MAX_DEPTH_FOR_VOICE {
            assert_eq!(queue.push_voice(message(1)), VoiceAdmission::Accepted);
        }

        assert_eq!(queue.push_voice(message(1)), VoiceAdmission::Dropped);
        assert!(
            !queue.must_close(),
            "dropping late audio is the correct outcome, not a fault"
        );
        assert!(
            queue.depth() < CAPACITY,
            "the point of the voice bound is that control still has room"
        );
    }

    #[test]
    fn control_still_fits_when_voice_has_been_refused() {
        let (queue, _receiver) = OutboundQueue::with_capacity(MAX_DEPTH_FOR_VOICE + 8);
        for _ in 0..MAX_DEPTH_FOR_VOICE {
            assert_eq!(queue.push_voice(message(1)), VoiceAdmission::Accepted);
        }
        assert_eq!(queue.push_voice(message(1)), VoiceAdmission::Dropped);

        assert_eq!(queue.try_send_all(vec![message(2); 8]), Ok(()));
    }

    #[test]
    fn an_empty_transition_is_free() {
        let (queue, _receiver) = OutboundQueue::with_capacity(1);
        assert_eq!(queue.try_send_all(Vec::new()), Ok(()));
        assert_eq!(queue.depth(), 0);
    }
}
