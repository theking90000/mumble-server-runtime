use std::time::Duration;

#[derive(Debug, Default)]
pub struct ClientReport {
    pub tcp_connect: Option<Duration>,
    pub tls_handshake: Option<Duration>,
    pub protocol_handshake: Option<Duration>,
    pub tcp_frames_received: u64,
    pub tcp_pings_sent: u64,
    pub tcp_ping_rtts: Vec<Duration>,
    pub udp_packets_sent: u64,
    pub udp_packets_received: u64,
    pub udp_ping_rtts: Vec<Duration>,
    pub voice_packets_sent: u64,
    pub voice_packets_received: u64,
    pub interactions_sent: u64,
    pub denied_interactions: u64,
    pub completed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct Stats {
    reports: usize,
    completed: usize,
    tcp_connect: Vec<Duration>,
    tls_handshake: Vec<Duration>,
    protocol_handshake: Vec<Duration>,
    tcp_ping_rtts: Vec<Duration>,
    udp_ping_rtts: Vec<Duration>,
    tcp_frames_received: u64,
    tcp_pings_sent: u64,
    udp_packets_sent: u64,
    udp_packets_received: u64,
    voice_packets_sent: u64,
    voice_packets_received: u64,
    interactions_sent: u64,
    denied_interactions: u64,
    errors: Vec<String>,
}

impl Stats {
    pub fn record(&mut self, report: ClientReport) {
        self.reports += 1;
        self.completed += usize::from(report.completed);
        self.tcp_connect.extend(report.tcp_connect);
        self.tls_handshake.extend(report.tls_handshake);
        self.protocol_handshake.extend(report.protocol_handshake);
        self.tcp_ping_rtts.extend(report.tcp_ping_rtts);
        self.udp_ping_rtts.extend(report.udp_ping_rtts);
        self.tcp_frames_received = self
            .tcp_frames_received
            .saturating_add(report.tcp_frames_received);
        self.tcp_pings_sent = self.tcp_pings_sent.saturating_add(report.tcp_pings_sent);
        self.udp_packets_sent = self
            .udp_packets_sent
            .saturating_add(report.udp_packets_sent);
        self.udp_packets_received = self
            .udp_packets_received
            .saturating_add(report.udp_packets_received);
        self.voice_packets_sent = self
            .voice_packets_sent
            .saturating_add(report.voice_packets_sent);
        self.voice_packets_received = self
            .voice_packets_received
            .saturating_add(report.voice_packets_received);
        self.interactions_sent = self
            .interactions_sent
            .saturating_add(report.interactions_sent);
        self.denied_interactions = self
            .denied_interactions
            .saturating_add(report.denied_interactions);
        if let Some(error) = report.error
            && self.errors.len() < 5
        {
            self.errors.push(error);
        }
    }

    pub fn reports(&self) -> usize {
        self.reports
    }

    pub fn completed(&self) -> usize {
        self.completed
    }

    pub fn failure_rate(&self) -> f64 {
        if self.reports == 0 {
            return 1.0;
        }
        let failed = self.reports.saturating_sub(self.completed);
        failed as f64 / self.reports as f64
    }

    pub fn print(&mut self, elapsed: Duration) {
        println!();
        println!("clients");
        println!("  completed       {}/{}", self.completed, self.reports);
        println!("  failure rate    {:.2}%", self.failure_rate() * 100.0);
        print_distribution("TCP connect", &mut self.tcp_connect);
        print_distribution("TLS handshake", &mut self.tls_handshake);
        print_distribution("Mumble sync", &mut self.protocol_handshake);
        print_distribution("TCP ping RTT", &mut self.tcp_ping_rtts);
        print_distribution("UDP ping RTT", &mut self.udp_ping_rtts);
        println!("traffic");
        println!("  TCP frames rx   {}", self.tcp_frames_received);
        println!("  TCP pings tx    {}", self.tcp_pings_sent);
        println!("  UDP packets tx  {}", self.udp_packets_sent);
        println!("  UDP packets rx  {}", self.udp_packets_received);
        println!(
            "  voice tx/rx     {}/{}",
            self.voice_packets_sent, self.voice_packets_received
        );
        println!("  interactions    {}", self.interactions_sent);
        println!("  denied          {}", self.denied_interactions);
        let seconds = elapsed.as_secs_f64();
        if seconds > 0.0 {
            println!(
                "  aggregate UDP   {:.0} packets/s",
                (self.udp_packets_sent + self.udp_packets_received) as f64 / seconds
            );
        }
        if !self.errors.is_empty() {
            println!("first errors");
            for error in &self.errors {
                println!("  {error}");
            }
        }
    }
}

fn print_distribution(label: &str, values: &mut [Duration]) {
    if values.is_empty() {
        println!("  {label:<15} n/a");
        return;
    }
    values.sort_unstable();
    println!(
        "  {label:<15} p50 {:>8}  p95 {:>8}  p99 {:>8}  max {:>8}",
        display(percentile(values, 50)),
        display(percentile(values, 95)),
        display(percentile(values, 99)),
        display(values[values.len() - 1]),
    );
}

fn percentile(values: &[Duration], percentile: usize) -> Duration {
    let last = values.len().saturating_sub(1);
    let rank = values.len().saturating_mul(percentile).saturating_add(99) / 100;
    let index = rank.saturating_sub(1);
    values[index.min(last)]
}

fn display(duration: Duration) -> String {
    if duration >= Duration::from_secs(1) {
        format!("{:.2}s", duration.as_secs_f64())
    } else if duration >= Duration::from_millis(1) {
        format!("{:.2}ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{}us", duration.as_micros())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank_without_leaving_the_slice() {
        let values: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        assert_eq!(percentile(&values, 50), Duration::from_millis(50));
        assert_eq!(percentile(&values, 99), Duration::from_millis(99));
    }
}
