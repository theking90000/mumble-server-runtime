use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScenarioKind {
    /// Complete the protocol and hold the connection open.
    Connect,
    /// Choose an Arena team, migrate, switch roles, and return to the lobby.
    Arena,
}

/// Stateful headless Mumble load generator for test servers.
#[derive(Debug, Clone, Parser)]
#[command(
    version,
    about,
    after_help = "TLS certificates are deliberately not verified. Use only against a server you are authorized to load-test."
)]
pub struct Config {
    /// TCP and UDP server address.
    #[arg(long, default_value = "127.0.0.1:64738")]
    pub server: SocketAddr,

    /// Number of concurrent clients to create.
    #[arg(short = 'n', long, default_value = "1")]
    pub clients: NonZeroUsize,

    /// Workload behavior after the Mumble handshake.
    #[arg(long, value_enum, default_value = "connect")]
    pub scenario: ScenarioKind,

    /// Time over which client starts are spread.
    #[arg(long, default_value = "0s", value_parser = parse_duration)]
    pub ramp: Duration,

    /// Time to keep all launched clients active after the ramp.
    #[arg(long, default_value = "30s", value_parser = parse_nonzero_duration)]
    pub duration: Duration,

    /// Timeout for establishing the TCP socket.
    #[arg(long, default_value = "5s", value_parser = parse_nonzero_duration)]
    pub connect_timeout: Duration,

    /// Timeout for TLS plus Mumble protocol synchronization.
    #[arg(long, default_value = "10s", value_parser = parse_nonzero_duration)]
    pub handshake_timeout: Duration,

    /// TCP and UDP keepalive interval.
    #[arg(long, default_value = "5s", value_parser = parse_nonzero_duration)]
    pub ping_interval: Duration,

    /// Raw concatenation of fixed-size Opus packets. Its presence enables voice.
    #[arg(long)]
    pub voice_file: Option<PathBuf>,

    /// Average percentage of time each client talks (0 to 100).
    #[arg(long, default_value = "5", value_parser = parse_percent)]
    pub talk_percent: u8,

    /// Duration of each continuous talking burst.
    #[arg(long, default_value = "2s", value_parser = parse_nonzero_duration)]
    pub talk_spurt: Duration,

    /// Bytes in each Opus packet from --voice-file.
    #[arg(long, default_value = "30", value_parser = parse_voice_frame_bytes)]
    pub voice_frame_bytes: NonZeroUsize,

    /// How often an interactive scenario may make its next stateful move.
    #[arg(long, default_value = "1s", value_parser = parse_nonzero_duration)]
    pub interaction_interval: Duration,

    /// Prefix used to create unique Mumble usernames.
    #[arg(long, default_value = "stress")]
    pub username_prefix: String,

    /// Optional opaque Authenticate.password value.
    #[arg(long)]
    pub password: Option<String>,

    /// Accept this fraction of failed clients before returning a failing exit code.
    #[arg(long, default_value = "0", value_parser = parse_failure_threshold)]
    pub failure_threshold: f64,

    /// Optional path for a secret-free JSON summary.
    #[arg(long)]
    pub json_output: Option<PathBuf>,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.username_prefix.trim().is_empty() {
            bail!("--username-prefix cannot be empty");
        }
        Ok(())
    }
}

fn parse_nonzero_duration(value: &str) -> Result<Duration, String> {
    let duration = parse_duration(value)?;
    if duration.is_zero() {
        return Err("duration must be greater than zero".to_owned());
    }
    Ok(duration)
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else {
        return Err("use a duration suffix: ms, s, or m".to_owned());
    };
    let amount = number
        .parse::<u64>()
        .with_context(|| format!("{number:?} is not an unsigned integer"))
        .map_err(|error| error.to_string())?;
    let milliseconds = amount
        .checked_mul(multiplier)
        .ok_or_else(|| "duration is too large".to_owned())?;
    Ok(Duration::from_millis(milliseconds))
}

fn parse_failure_threshold(value: &str) -> Result<f64, String> {
    let threshold = value
        .parse::<f64>()
        .map_err(|error| format!("invalid failure threshold: {error}"))?;
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err("failure threshold must be between 0 and 1".to_owned());
    }
    Ok(threshold)
}

fn parse_voice_frame_bytes(value: &str) -> Result<NonZeroUsize, String> {
    let bytes = value
        .parse::<usize>()
        .map_err(|error| format!("invalid Opus packet size: {error}"))?;
    if !(1..=1200).contains(&bytes) {
        return Err("Opus packet size must be between 1 and 1200 bytes".to_owned());
    }
    NonZeroUsize::new(bytes).ok_or_else(|| "Opus packet size cannot be zero".to_owned())
}

fn parse_percent(value: &str) -> Result<u8, String> {
    let percent = value
        .parse::<u8>()
        .map_err(|error| format!("invalid talk percentage: {error}"))?;
    if percent > 100 {
        return Err("talk percentage must be between 0 and 100".to_owned());
    }
    Ok(percent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_parser_accepts_supported_units() {
        assert_eq!(parse_duration("25ms"), Ok(Duration::from_millis(25)));
        assert_eq!(parse_duration("2s"), Ok(Duration::from_secs(2)));
        assert_eq!(parse_duration("3m"), Ok(Duration::from_secs(180)));
    }

    #[test]
    fn duration_parser_rejects_missing_units_and_overflow() {
        assert!(parse_duration("12").is_err());
        assert!(parse_duration("18446744073709551615m").is_err());
    }

    #[test]
    fn failure_threshold_is_a_fraction() {
        assert_eq!(parse_failure_threshold("0.25"), Ok(0.25));
        assert!(parse_failure_threshold("-0.1").is_err());
        assert!(parse_failure_threshold("1.1").is_err());
    }

    #[test]
    fn talk_percentage_is_inclusive() {
        assert_eq!(parse_percent("0"), Ok(0));
        assert_eq!(parse_percent("5"), Ok(5));
        assert_eq!(parse_percent("100"), Ok(100));
        assert!(parse_percent("101").is_err());
    }

    #[test]
    fn opus_packet_size_is_bounded() {
        assert_eq!(parse_voice_frame_bytes("30").map(NonZeroUsize::get), Ok(30));
        assert!(parse_voice_frame_bytes("0").is_err());
        assert!(parse_voice_frame_bytes("1201").is_err());
    }
}
