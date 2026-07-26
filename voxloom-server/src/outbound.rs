//! One connection's output queue, and the admission policy that guards it.
//!
//! # Why the queue is bounded
//!
//! An unbounded queue turns a slow client into unbounded server memory: nothing
//! ever pushes back, so a peer that stops reading grows its backlog until the
//! process dies. The bound is the fix, and choosing what to do when the bound is
//! reached is the whole substance of this module.
//!
//! # Why there is exactly one queue, not one per traffic class
//!
//! Voice and control share a single ordered stream because the client requires
//! it: audio whose sender session it has never been told about is discarded
//! outright, so the `UserState` that introduces a speaker must reach the client
//! *before* that speaker's tunnelled audio. Two independent queues would let the
//! audio overtake the introduction and the speaker would be silently inaudible.
//!
//! REF: references/mumble/src/mumble/ServerHandler.cpp : `handleVoicePacket`
//!   looks up `ClientUser::get(audioData.senderSession)` and buffers the frame
//!   only when that lookup succeeds.
//!
//! # Two admission policies over that one stream
//!
//! The classes differ in what a refusal *means*, not in where they queue:
//!
//! - **Voice is droppable.** A late voice packet is worthless; delivering
//!   half a second of stale audio is worse than a gap. Voice is therefore
//!   admitted only while the queue is shallow, which bounds tunnel latency
//!   rather than tunnel volume.
//! - **Control is not droppable.** Silently skipping a `UserState` leaves the
//!   client holding a view that disagrees with the server's, permanently and
//!   invisibly. A control message that cannot be admitted therefore marks the
//!   connection for teardown: reconnecting rebuilds a correct view, which is the
//!   spirit of ADR-009 applied to the only failure P4 can produce today.
//!
//! # Admission never blocks
//!
//! Every push is non-blocking. Two reasons, both load-bearing. Connections push
//! presence updates onto *each other's* queues, so a blocking push would let one
//! slow client stall every task that tries to notify it, and two connections
//! pushing to each other while both queues are full would deadlock outright.
//! Non-blocking admission makes both impossible by construction rather than by
//! careful ordering.
//!
//! # What Phase 6 builds on this
//!
//! This queue is the intended commit point for per-connection view transactions:
//! accepted into the queue *is* committed, because acceptance is synchronous and
//! the stream underneath delivers in order or not at all. That design needs
//! all-or-nothing admission of a whole transaction, which
//! [`tokio::sync::mpsc::Sender::try_reserve`] composes into naturally: take one
//! permit per operation, and drop the collected permits to release them if any
//! one fails. Two consequences worth writing down before they are rediscovered
//! the hard way:
//!
//! - A transaction that cannot fit in [`CAPACITY`] *at all* must escalate to a
//!   forced reconnect. Retrying it forever is a livelock, not backpressure.
//! - A refused transaction must leave the committed view untouched, so the next
//!   attempt is planned against the newest desired state. Skipping the
//!   intermediate states is correct rather than merely cheap, because a diff is
//!   a function of two states and not a replay of a log.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::mpsc;
use voxloom_protocol::ControlMessage;

use crate::state::SessionId;

/// A message queued for a connection's TCP writer. Control frames and
/// TCP-tunnelled audio (`UDPTunnel`) both travel as [`ControlMessage`].
pub type Outbound = ControlMessage;

/// How many messages one connection's queue holds.
///
/// Sized for control, since voice is capped far lower by [`MAX_DEPTH_FOR_VOICE`].
/// It has to comfortably hold the largest burst of control traffic a connection
/// can legitimately receive at once, which today is one presence update per
/// already-connected user.
pub const CAPACITY: usize = 1024;

/// Queue depth above which voice is dropped instead of enqueued.
///
/// This is a latency bound, not a volume one: at the usual 10 ms Opus framing,
/// 64 queued packets are already ~640 ms of backlog from a single speaker, and
/// audio that old is of no use to anyone. Control traffic counts towards the
/// depth on purpose, so a connection that is behind on its view stops accepting
/// voice before it stops accepting the view updates that explain it.
pub const MAX_DEPTH_FOR_VOICE: usize = 64;

/// What the queue did with a voice packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a dropped voice packet is an outcome the caller must acknowledge"]
pub enum VoiceAdmission {
    Accepted,
    /// Refused because the queue is too deep. Counted and logged here; the
    /// caller has nothing to repair, a gap in tunnelled audio is the correct
    /// behaviour under congestion.
    Dropped,
}

/// What the queue did with a control message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a refused control message means this connection must be torn down"]
pub enum ControlAdmission {
    Accepted,
    /// Refused: the queue is full, or the receiving task is already gone. The
    /// connection has been marked fatal ([`OutboundQueue::must_close`]) because
    /// dropping the message would leave the client's view silently diverged.
    Refused,
}

/// The sending half of one connection's output queue, plus its admission state.
///
/// Held inside the connection's `UserEntry`, so every task that can reach the
/// user can also push to it: the owning connection task, other connections
/// broadcasting presence, and the UDP voice plane.
pub struct OutboundQueue {
    sender: mpsc::Sender<Outbound>,
    /// Only used to make log lines actionable.
    session: SessionId,
    /// Set once a control message has been refused. The owning task polls it and
    /// ends the connection.
    must_close: AtomicBool,
    /// Voice packets dropped during the current congestion episode.
    dropped_voice: AtomicU64,
    /// Whether we are inside a congestion episode. Exists so the log gets two
    /// lines per episode (entry and recovery) instead of one per lost packet.
    congested: AtomicBool,
}

impl OutboundQueue {
    /// Create the queue for a connection, returning the receiving half for that
    /// connection's writer task.
    pub fn new(session: SessionId) -> (Self, mpsc::Receiver<Outbound>) {
        let (sender, receiver) = mpsc::channel(CAPACITY);
        (
            Self {
                sender,
                session,
                must_close: AtomicBool::new(false),
                dropped_voice: AtomicU64::new(0),
                congested: AtomicBool::new(false),
            },
            receiver,
        )
    }

    /// Offer a control message. Never dropped silently: a refusal marks the
    /// connection for teardown.
    pub fn push_control(&self, message: Outbound) -> ControlAdmission {
        match self.sender.try_send(message) {
            Ok(()) => ControlAdmission::Accepted,
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Report once. Later refusals on the same connection are the
                // same episode and add nothing.
                if !self.must_close.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "voxloom-server: session {}: control queue full ({CAPACITY} messages); \
                         closing the connection rather than letting its view diverge",
                        self.session
                    );
                }
                ControlAdmission::Refused
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // The connection task has already gone and its deregistration is
                // in flight. Not an anomaly, and nothing to log.
                self.must_close.store(true, Ordering::Relaxed);
                ControlAdmission::Refused
            }
        }
    }

    /// Offer a voice packet. Dropped rather than queued when the connection is
    /// already behind, because stale audio has no value.
    pub fn push_voice(&self, message: Outbound) -> VoiceAdmission {
        if self.depth() >= MAX_DEPTH_FOR_VOICE {
            return self.record_voice_drop();
        }
        match self.sender.try_send(message) {
            Ok(()) => {
                self.record_voice_accept();
                VoiceAdmission::Accepted
            }
            // Lost the race against another sender, or the receiver is gone.
            // Either way the packet is dropped, which is what congestion means.
            Err(_) => self.record_voice_drop(),
        }
    }

    /// Whether a control message has been refused, meaning the owning task must
    /// end this connection.
    pub fn must_close(&self) -> bool {
        self.must_close.load(Ordering::Relaxed)
    }

    /// Messages currently queued.
    pub fn depth(&self) -> usize {
        self.sender
            .max_capacity()
            .saturating_sub(self.sender.capacity())
    }

    /// Voice packets dropped in the current congestion episode (0 when clear).
    pub fn dropped_voice(&self) -> u64 {
        self.dropped_voice.load(Ordering::Relaxed)
    }

    fn record_voice_drop(&self) -> VoiceAdmission {
        self.dropped_voice.fetch_add(1, Ordering::Relaxed);
        if !self.congested.swap(true, Ordering::Relaxed) {
            eprintln!(
                "voxloom-server: session {}: output queue congested at depth {}; dropping voice",
                self.session,
                self.depth()
            );
        }
        VoiceAdmission::Dropped
    }

    fn record_voice_accept(&self) {
        // Relaxed ordering throughout: these counters drive log lines, never a
        // decision. A racing pair of threads can at worst log one episode twice.
        if self.congested.swap(false, Ordering::Relaxed) {
            let dropped = self.dropped_voice.swap(0, Ordering::Relaxed);
            eprintln!(
                "voxloom-server: session {}: output queue drained, {dropped} voice packets lost",
                self.session
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn message(byte: u8) -> Outbound {
        ControlMessage::UdpTunnel(vec![byte, byte])
    }

    #[test]
    fn voice_stops_being_admitted_once_the_queue_is_deep() {
        let (queue, _receiver) = OutboundQueue::new(1);

        for packet in 0..MAX_DEPTH_FOR_VOICE {
            assert_eq!(
                queue.push_voice(message(0)),
                VoiceAdmission::Accepted,
                "packet {packet} refused below the depth bound"
            );
        }
        assert_eq!(queue.push_voice(message(0)), VoiceAdmission::Dropped);
        assert_eq!(queue.dropped_voice(), 1);
    }

    #[test]
    fn control_still_fits_when_voice_is_already_refused() {
        let (queue, _receiver) = OutboundQueue::new(1);
        for _ in 0..MAX_DEPTH_FOR_VOICE {
            let _admitted = queue.push_voice(message(0));
        }
        assert_eq!(queue.push_voice(message(0)), VoiceAdmission::Dropped);

        // The reserve exists precisely so this keeps working: the view must be
        // repairable on a connection that is already too slow for audio.
        assert_eq!(
            queue.push_control(message(1)),
            ControlAdmission::Accepted,
            "voice congestion must not starve control"
        );
        assert!(!queue.must_close());
    }

    #[test]
    fn a_full_queue_makes_control_fatal_rather_than_lossy() {
        let (queue, _receiver) = OutboundQueue::new(1);
        for _ in 0..CAPACITY {
            assert_eq!(queue.push_control(message(2)), ControlAdmission::Accepted);
        }

        assert_eq!(queue.push_control(message(2)), ControlAdmission::Refused);
        assert!(
            queue.must_close(),
            "a refused control message must end the connection, not be skipped"
        );
    }

    #[test]
    fn a_closed_receiver_refuses_control_without_being_an_anomaly() {
        let (queue, receiver) = OutboundQueue::new(1);
        drop(receiver);

        assert_eq!(queue.push_control(message(3)), ControlAdmission::Refused);
        assert!(queue.must_close());
    }

    #[tokio::test]
    async fn draining_the_queue_lets_voice_flow_again() {
        let (queue, mut receiver) = OutboundQueue::new(1);
        for _ in 0..MAX_DEPTH_FOR_VOICE {
            let _admitted = queue.push_voice(message(0));
        }
        assert_eq!(queue.push_voice(message(0)), VoiceAdmission::Dropped);

        for _ in 0..MAX_DEPTH_FOR_VOICE {
            receiver.recv().await.expect("queued message");
        }

        assert_eq!(queue.push_voice(message(0)), VoiceAdmission::Accepted);
        assert_eq!(
            queue.dropped_voice(),
            0,
            "recovery must clear the episode counter"
        );
    }
}
