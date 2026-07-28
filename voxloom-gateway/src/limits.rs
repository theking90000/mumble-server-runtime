//! Data-plane limits (spec 15.7).
//!
//! Two protect the voice path itself: a hard cap on datagram size, and a
//! per-connection packet budget. That budget is not an optimisation, it is what
//! keeps one connection from turning into N encryptions per packet for every
//! other connection.
//!
//! One protects the control path: a leaky bucket in front of `TextMessage`,
//! which is the only thing a client can type as fast as it likes. The rest of
//! the rate-limit matrix of 22.5 - authentication, blobs, context actions - is
//! still Phase 9.

use std::time::{Duration, Instant};

/// Largest voice datagram the server accepts, in bytes.
///
/// REF: references/mumble/src/MumbleProtocol.h : `MAX_UDP_PACKET_SIZE = 1024`.
///   The real server drops tunnelled audio below 2 bytes or above this, so
///   matching it keeps both ingress paths on the same rule.
pub const MAX_UDP_PACKET_SIZE: usize = 1024;

/// Smallest datagram that can carry anything meaningful (a type byte plus at
/// least one payload byte).
/// REF: references/mumble/src/murmur/Server.cpp : the `UDPTunnel` branch drops
///   `len < 2`.
pub const MIN_UDP_PACKET_SIZE: usize = 2;

/// Sustained voice packets per second allowed from one connection.
///
/// A client at the usual 10 ms framing emits 100 packets per second, and 20 ms
/// framing halves that. 200 leaves generous headroom for a fast codec setting
/// while still bounding a flood to twice the legitimate worst case.
const PACKETS_PER_SECOND: u32 = 200;

/// How many packets may arrive back to back before the sustained rate applies.
/// A burst absorbs normal jitter and scheduler hiccups without ever letting the
/// long-run average exceed [`PACKETS_PER_SECOND`].
const BURST: u32 = 400;

/// What the reference server adds to a voice payload to account for what the
/// network actually carried.
///
/// REF: references/mumble/src/murmur/Server.cpp : `processMsg` bills
///   `20 + 8 + 4 + payload` - IP, UDP, crypt, data - against the bandwidth
///   record.
pub const PACKET_OVERHEAD: usize = 20 + 8 + 4;

/// How the throughput window is bucketed, and over what span.
///
/// Ten buckets of 100 ms. The reference server keeps 360 individually timed
/// frames per connection, which is roughly 4 KB of ring per user to answer one
/// line in a dialog; bucketing gives the same figure to a tenth of a second for
/// fifty bytes. The deviation is deliberate and this is the only place it shows.
const BANDWIDTH_BUCKETS: usize = 10;
const BUCKET: Duration = Duration::from_millis(100);
const WINDOW: Duration = Duration::from_millis(1000);

/// One connection's voice traffic account.
///
/// Three things that share a clock and a lock: the token bucket that decides
/// admission, the one-second window that measures throughput, and the last sign
/// of life that is not a keepalive. Keeping them together is what makes the hot
/// path take one lock rather than three.
///
/// The clock is passed in rather than read here: the packet path already knows
/// what time it is, and a test that has to sleep to observe a rate limit is a
/// test that will be flaky by Friday.
#[derive(Debug)]
pub struct VoiceBudget {
    tokens: u32,
    last_refill: Instant,
    /// Bytes billed to each bucket, the current one at `bucket`.
    billed: [u32; BANDWIDTH_BUCKETS],
    bucket: usize,
    bucket_started: Instant,
    /// The last thing this connection did that counts as being there.
    last_active: Instant,
}

impl VoiceBudget {
    pub fn new(now: Instant) -> Self {
        Self {
            tokens: BURST,
            last_refill: now,
            billed: [0; BANDWIDTH_BUCKETS],
            bucket: 0,
            bucket_started: now,
            last_active: now,
        }
    }

    /// Take one packet's worth of budget, refilling for elapsed time first, and
    /// bill what it carried.
    ///
    /// A refused packet is billed nothing, like the reference server's own
    /// record: `addFrame` is both the limiter and the meter, and a frame it
    /// rejects never enters the window.
    ///
    /// `bytes` is the decoded packet, to which the network overhead is added.
    /// `false` means the caller must drop the packet.
    pub fn allow(&mut self, now: Instant, bytes: usize) -> bool {
        self.refill(now);
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;

        self.roll(now);
        let billed = u32::try_from(bytes.saturating_add(PACKET_OVERHEAD)).unwrap_or(u32::MAX);
        if let Some(bucket) = self.billed.get_mut(self.bucket) {
            *bucket = bucket.saturating_add(billed);
        }
        self.last_active = now;
        true
    }

    /// Note that the connection did something that is not a keepalive.
    pub fn touch(&mut self, now: Instant) {
        self.last_active = now;
    }

    /// Voice throughput over the last second, in **bytes per second**.
    ///
    /// The unit is the client's: it divides by 125 to print kbit/s.
    ///
    /// REF: references/mumble/src/murmur/ServerUser.cpp : `bandwidth()` sums the
    ///   frames of the last second and divides by the elapsed time.
    /// REF: references/mumble/src/mumble/UserInformation.cpp : the dialog prints
    ///   `msg.bandwidth() / 125.0` as kbit/s.
    pub fn bandwidth(&mut self, now: Instant) -> u32 {
        self.roll(now);
        let total: u32 = self.billed.iter().copied().fold(0, u32::saturating_add);
        // The window is a whole second by construction, so the sum already is a
        // per-second figure.
        total
    }

    /// How long this connection has been doing nothing.
    ///
    /// REF: references/mumble/src/murmur/ServerUser.cpp : `idleSeconds()` takes
    ///   the shorter of "since the last voice frame" and "since the last control
    ///   message that unidles".
    pub fn idle(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_active)
    }

    /// Advance the window to `now`, clearing whatever it moved past.
    fn roll(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.bucket_started);
        let steps = usize::try_from(elapsed.as_millis() / BUCKET.as_millis()).unwrap_or(usize::MAX);
        if steps == 0 {
            return;
        }

        if steps >= BANDWIDTH_BUCKETS {
            // Nothing in the window is still within the last second.
            self.billed = [0; BANDWIDTH_BUCKETS];
            self.bucket = 0;
        } else {
            for step in 1..=steps {
                let index = (self.bucket + step) % BANDWIDTH_BUCKETS;
                if let Some(bucket) = self.billed.get_mut(index) {
                    *bucket = 0;
                }
            }
            self.bucket = (self.bucket + steps) % BANDWIDTH_BUCKETS;
        }
        // Anchored on whole buckets so a burst of packets cannot keep dragging
        // the boundary forward and stretch the window past a second.
        self.bucket_started += BUCKET.saturating_mul(u32::try_from(steps).unwrap_or(u32::MAX));
        if now.saturating_duration_since(self.bucket_started) > WINDOW {
            self.bucket_started = now;
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill);
        if elapsed < Duration::from_millis(1) {
            return;
        }

        // Whole tokens only; the remainder stays on the clock so a stream of
        // sub-millisecond gaps still accrues budget instead of losing it.
        let earned = elapsed
            .as_millis()
            .saturating_mul(u128::from(PACKETS_PER_SECOND))
            / 1000;
        let earned = u32::try_from(earned).unwrap_or(u32::MAX);
        if earned == 0 {
            return;
        }

        self.tokens = self.tokens.saturating_add(earned).min(BURST);
        self.last_refill = now;
    }
}

/// Whether a datagram is within the accepted size band (spec 15.7).
pub fn is_acceptable_size(len: usize) -> bool {
    (MIN_UDP_PACKET_SIZE..=MAX_UDP_PACKET_SIZE).contains(&len)
}

/// Sustained text messages per second allowed from one connection, and how many
/// may arrive back to back.
///
/// REF: references/mumble/src/murmur/Meta.cpp : `iMessageLimit = 1`,
///   `iMessageBurst = 5`.
const MESSAGES_PER_SECOND: u32 = 1;
const MESSAGE_BURST: u32 = 5;

/// One connection's allowance for the things it types.
///
/// The same leaky bucket the reference server puts in front of `TextMessage`,
/// and it is worth having here rather than in the shard: a flood refused at the
/// socket never crosses a mailbox nor wakes a shard task.
///
/// The clock is passed in for the same reason [`VoiceBudget`] does it: a test
/// that has to sleep to observe a rate limit is a test that will be flaky.
///
/// REF: references/mumble/src/murmur/Messages.cpp : the `RATELIMIT` macro
///   returns from `msgTextMessage` **without** answering the client.
#[derive(Debug)]
pub struct TextBudget {
    tokens: u32,
    last_refill: Instant,
}

impl TextBudget {
    #[must_use]
    pub fn new(now: Instant) -> TextBudget {
        TextBudget {
            tokens: MESSAGE_BURST,
            last_refill: now,
        }
    }

    /// Take one message's worth of budget. `false` means drop it.
    pub fn allow(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last_refill);
        // Whole tokens only; the remainder stays on the clock so a stream of
        // short gaps still accrues budget instead of losing it.
        let earned = elapsed
            .as_millis()
            .saturating_mul(u128::from(MESSAGES_PER_SECOND))
            / 1000;
        if let Ok(earned) = u32::try_from(earned)
            && earned > 0
        {
            self.tokens = self.tokens.saturating_add(earned).min(MESSAGE_BURST);
            self.last_refill = now;
        }

        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typist_gets_a_burst_then_one_message_a_second() {
        let start = Instant::now();
        let mut budget = TextBudget::new(start);

        for message in 0..MESSAGE_BURST {
            assert!(budget.allow(start), "message {message} is within the burst");
        }
        assert!(!budget.allow(start), "the burst is spent");

        assert!(
            budget.allow(start + Duration::from_millis(1000)),
            "a second of silence buys one message"
        );
        assert!(
            !budget.allow(start + Duration::from_millis(1000)),
            "and only one"
        );
    }

    #[test]
    fn a_burst_is_allowed_then_the_sustained_rate_applies() {
        let start = Instant::now();
        let mut budget = VoiceBudget::new(start);

        for packet in 0..BURST {
            assert!(
                budget.allow(start, 0),
                "packet {packet} of the burst refused"
            );
        }
        assert!(!budget.allow(start, 0), "the burst is not bounded");

        // A second of silence refills to the cap, not beyond it.
        let later = start + Duration::from_secs(10);
        assert!(budget.allow(later, 0));
        for _ in 0..BURST {
            budget.allow(later, 0);
        }
        assert!(!budget.allow(later, 0), "refill exceeded the burst ceiling");
    }

    #[test]
    fn budget_accrues_with_elapsed_time() {
        let start = Instant::now();
        let mut budget = VoiceBudget::new(start);
        for _ in 0..BURST {
            budget.allow(start, 0);
        }
        assert!(!budget.allow(start, 0));

        // 100 ms buys PACKETS_PER_SECOND/10 packets.
        let later = start + Duration::from_millis(100);
        for packet in 0..(PACKETS_PER_SECOND / 10) {
            assert!(
                budget.allow(later, 0),
                "packet {packet} after refill refused"
            );
        }
        assert!(!budget.allow(later, 0), "refill was too generous");
    }

    #[test]
    fn throughput_is_measured_over_the_last_second_and_then_forgotten() {
        let start = Instant::now();
        let mut budget = VoiceBudget::new(start);

        // Ten packets of 100 payload bytes, spread across the whole window.
        for step in 0..10 {
            let now = start + Duration::from_millis(step * 100);
            assert!(budget.allow(now, 100));
        }

        let billed = 10 * (100 + PACKET_OVERHEAD);
        let measured = budget.bandwidth(start + Duration::from_millis(999));
        assert_eq!(
            u32::try_from(billed).unwrap_or(u32::MAX),
            measured,
            "the window must bill the payload plus the network overhead"
        );

        // A second of silence, and the whole window has moved past them.
        assert_eq!(
            budget.bandwidth(start + Duration::from_secs(3)),
            0,
            "throughput is a rate, not a total"
        );
    }

    #[test]
    fn a_refused_packet_is_billed_nothing() {
        let start = Instant::now();
        let mut budget = VoiceBudget::new(start);
        for _ in 0..BURST {
            budget.allow(start, 10);
        }
        let admitted = budget.bandwidth(start);

        assert!(!budget.allow(start, 10_000), "the burst is not bounded");
        assert_eq!(
            budget.bandwidth(start),
            admitted,
            "a packet the server dropped never crossed the network for this user"
        );
    }

    #[test]
    fn idling_is_the_time_since_the_last_thing_that_counted() {
        let start = Instant::now();
        let mut budget = VoiceBudget::new(start);
        assert_eq!(
            budget.idle(start + Duration::from_secs(5)),
            Duration::from_secs(5)
        );

        budget.allow(start + Duration::from_secs(5), 100);
        assert_eq!(
            budget.idle(start + Duration::from_secs(6)),
            Duration::from_secs(1)
        );

        // A control message that unidles resets it just the same.
        budget.touch(start + Duration::from_secs(6));
        assert_eq!(budget.idle(start + Duration::from_secs(6)), Duration::ZERO);
    }

    #[test]
    fn the_accepted_size_band_matches_the_reference() {
        assert!(!is_acceptable_size(0));
        assert!(!is_acceptable_size(1));
        assert!(is_acceptable_size(2));
        assert!(is_acceptable_size(MAX_UDP_PACKET_SIZE));
        assert!(!is_acceptable_size(MAX_UDP_PACKET_SIZE + 1));
    }
}
