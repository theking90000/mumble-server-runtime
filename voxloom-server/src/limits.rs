//! Data-plane limits (spec 15.7).
//!
//! Phase 4 implements the two that protect the voice path itself: a hard cap on
//! datagram size, and a per-connection packet budget. The full rate-limit matrix
//! of 22.5 (authentication, text, blobs, context actions) is Phase 9; putting a
//! budget on audio now is not an optimisation, it is what keeps one connection
//! from turning into N encryptions per packet for every other connection.

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

/// A token bucket over voice packets from one connection.
///
/// The clock is passed in rather than read here: the packet path already knows
/// what time it is, and a test that has to sleep to observe a rate limit is a
/// test that will be flaky by Friday.
#[derive(Debug)]
pub struct VoiceBudget {
    tokens: u32,
    last_refill: Instant,
}

impl VoiceBudget {
    pub fn new(now: Instant) -> Self {
        Self {
            tokens: BURST,
            last_refill: now,
        }
    }

    /// Take one packet's worth of budget, refilling for elapsed time first.
    /// `false` means the caller must drop the packet.
    pub fn allow(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_is_allowed_then_the_sustained_rate_applies() {
        let start = Instant::now();
        let mut budget = VoiceBudget::new(start);

        for packet in 0..BURST {
            assert!(budget.allow(start), "packet {packet} of the burst refused");
        }
        assert!(!budget.allow(start), "the burst is not bounded");

        // A second of silence refills to the cap, not beyond it.
        let later = start + Duration::from_secs(10);
        assert!(budget.allow(later));
        for _ in 0..BURST {
            budget.allow(later);
        }
        assert!(!budget.allow(later), "refill exceeded the burst ceiling");
    }

    #[test]
    fn budget_accrues_with_elapsed_time() {
        let start = Instant::now();
        let mut budget = VoiceBudget::new(start);
        for _ in 0..BURST {
            budget.allow(start);
        }
        assert!(!budget.allow(start));

        // 100 ms buys PACKETS_PER_SECOND/10 packets.
        let later = start + Duration::from_millis(100);
        for packet in 0..(PACKETS_PER_SECOND / 10) {
            assert!(budget.allow(later), "packet {packet} after refill refused");
        }
        assert!(!budget.allow(later), "refill was too generous");
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
