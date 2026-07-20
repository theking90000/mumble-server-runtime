//! Binary capture format `.voxcap` and its non-blocking writer.
//!
//! This module owns the serialized truth of a recording session. The proxy
//! only moves bytes; everything durable about a run passes through here.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};

/// Magic header identifying a `.voxcap` file, format version 01.
pub const MAGIC: &[u8; 8] = b"VOXCAP01";

/// Hard upper bound on a single record's declared length. Real records are a
/// TCP chunk (16 KiB) or a UDP datagram (64 KiB); this leaves generous margin
/// while refusing to allocate gigabytes for a corrupt or hostile file.
pub const MAX_RECORD_LEN: u32 = 16 * 1024 * 1024;

/// Direction of a captured datagram or segment relative to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    ClientToServer,
    ServerToClient,
}

impl Dir {
    fn to_code(self) -> u8 {
        match self {
            Dir::ClientToServer => 0,
            Dir::ServerToClient => 1,
        }
    }

    fn from_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Dir::ClientToServer),
            1 => Ok(Dir::ServerToClient),
            other => Err(anyhow!("invalid direction code {other}")),
        }
    }
}

/// Transport a captured record travelled over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// TCP control plane, logged in clear text after TLS termination.
    Tcp,
    /// UDP voice plane, logged as raw OCB2-encrypted bytes (never decoded).
    Udp,
}

impl Transport {
    fn to_code(self) -> u8 {
        match self {
            Transport::Tcp => 0,
            Transport::Udp => 1,
        }
    }

    fn from_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Transport::Tcp),
            1 => Ok(Transport::Udp),
            other => Err(anyhow!("invalid transport code {other}")),
        }
    }
}

/// One captured record: a direction, a transport, a timestamp and the raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub dir: Dir,
    pub transport: Transport,
    pub ts_micros: i64,
    pub data: Vec<u8>,
}

/// Live counters shared between the async proxy and the writer thread.
#[derive(Debug, Default)]
pub struct LiveStats {
    pub enqueued: AtomicU64,
    pub written: AtomicU64,
    pub bytes_c2s_tcp: AtomicU64,
    pub bytes_s2c_tcp: AtomicU64,
    pub bytes_c2s_udp: AtomicU64,
    pub bytes_s2c_udp: AtomicU64,
    pub recs_tcp: AtomicU64,
    pub recs_udp: AtomicU64,
}

/// Current Unix time in microseconds. Never panics: a clock before the epoch
/// yields 0 rather than unwinding.
pub fn now_micros() -> i64 {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    i64::try_from(micros).unwrap_or(i64::MAX)
}

/// Owns the file and the background writer thread. Dropping it closes the
/// channel, which lets the writer thread flush and exit.
pub struct CaptureWriter {
    tx: Sender<Record>,
    stats: Arc<LiveStats>,
    handle: Option<JoinHandle<()>>,
}

impl CaptureWriter {
    /// Create the capture file, write the magic header and spawn the writer.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::create(path)
            .with_context(|| format!("creating capture file {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(MAGIC)
            .with_context(|| format!("writing magic to {}", path.display()))?;
        writer
            .flush()
            .with_context(|| format!("flushing magic to {}", path.display()))?;

        let (tx, rx) = channel::<Record>();
        let stats = Arc::new(LiveStats::default());
        let thread_stats = Arc::clone(&stats);
        let handle = std::thread::Builder::new()
            .name("voxcap-writer".to_string())
            .spawn(move || writer_loop(writer, rx, thread_stats))
            .context("spawning capture writer thread")?;

        Ok(Self {
            tx,
            stats,
            handle: Some(handle),
        })
    }

    /// A cloneable sink used by proxy tasks to enqueue records.
    pub fn sink(&self) -> RecordSink {
        RecordSink {
            tx: self.tx.clone(),
            stats: Arc::clone(&self.stats),
        }
    }

    /// Shared live counters.
    pub fn stats(&self) -> Arc<LiveStats> {
        Arc::clone(&self.stats)
    }
}

impl Drop for CaptureWriter {
    fn drop(&mut self) {
        // Each record is flushed as it is written, and shutdown drains until
        // `written >= enqueued`, so nothing is lost by not joining here. We
        // detach the writer thread rather than join: a lingering sink clone in a
        // still-running connection task would otherwise keep the channel open
        // and block this drop forever.
        let _ = self.handle.take();
    }
}

fn writer_loop(mut writer: BufWriter<File>, rx: Receiver<Record>, stats: Arc<LiveStats>) {
    while let Ok(record) = rx.recv() {
        match write_record(&mut writer, &record) {
            Ok(()) => {
                stats.written.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                eprintln!("capture writer error: {error:#}");
            }
        }
    }
}

fn write_record(writer: &mut BufWriter<File>, record: &Record) -> Result<()> {
    let len = u32::try_from(record.data.len()).context("record data length exceeds u32::MAX")?;
    writer.write_all(&[record.dir.to_code(), record.transport.to_code()])?;
    writer.write_all(&record.ts_micros.to_le_bytes())?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&record.data)?;
    // Flush after every record for durability: a crash keeps whatever landed.
    writer.flush()?;
    Ok(())
}

/// Cloneable handle used by proxy tasks to log captured bytes.
#[derive(Clone)]
pub struct RecordSink {
    tx: Sender<Record>,
    stats: Arc<LiveStats>,
}

impl RecordSink {
    /// Account for and enqueue a captured segment. Sending is best-effort: if
    /// the writer thread is gone the record is dropped silently.
    pub fn log(&self, dir: Dir, transport: Transport, data: &[u8]) {
        let bytes = data.len() as u64;
        match (dir, transport) {
            (Dir::ClientToServer, Transport::Tcp) => {
                self.stats.bytes_c2s_tcp.fetch_add(bytes, Ordering::Relaxed);
            }
            (Dir::ServerToClient, Transport::Tcp) => {
                self.stats.bytes_s2c_tcp.fetch_add(bytes, Ordering::Relaxed);
            }
            (Dir::ClientToServer, Transport::Udp) => {
                self.stats.bytes_c2s_udp.fetch_add(bytes, Ordering::Relaxed);
            }
            (Dir::ServerToClient, Transport::Udp) => {
                self.stats.bytes_s2c_udp.fetch_add(bytes, Ordering::Relaxed);
            }
        }
        match transport {
            Transport::Tcp => {
                self.stats.recs_tcp.fetch_add(1, Ordering::Relaxed);
            }
            Transport::Udp => {
                self.stats.recs_udp.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);

        let record = Record {
            dir,
            transport,
            ts_micros: now_micros(),
            data: data.to_vec(),
        };
        let _ = self.tx.send(record);
    }
}

/// Read every record from a `.voxcap` file. A clean EOF at a record boundary is
/// the normal end of file; a partial record is an error.
pub fn read_records(path: impl AsRef<Path>) -> Result<Vec<Record>> {
    let path = path.as_ref();
    let mut file =
        File::open(path).with_context(|| format!("opening capture file {}", path.display()))?;

    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)
        .with_context(|| format!("reading magic from {}", path.display()))?;
    if &magic != MAGIC {
        return Err(anyhow!(
            "bad magic in {}: expected {:?}, found {:?}",
            path.display(),
            MAGIC,
            magic
        ));
    }

    let mut records = Vec::new();
    loop {
        let mut header = [0u8; 14];
        match read_full(&mut file, &mut header)? {
            ReadOutcome::Eof => break,
            ReadOutcome::Partial(got) => {
                return Err(anyhow!(
                    "truncated record header in {}: {} of 14 bytes",
                    path.display(),
                    got
                ));
            }
            ReadOutcome::Full => {}
        }

        let dir = Dir::from_code(header[0])?;
        let transport = Transport::from_code(header[1])?;
        let mut ts_bytes = [0u8; 8];
        ts_bytes.copy_from_slice(&header[2..10]);
        let ts_micros = i64::from_le_bytes(ts_bytes);
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&header[10..14]);
        let len = u32::from_le_bytes(len_bytes);
        if len > MAX_RECORD_LEN {
            return Err(anyhow!(
                "record length {len} in {} exceeds maximum {MAX_RECORD_LEN}",
                path.display()
            ));
        }
        let len = usize::try_from(len).context("record length exceeds usize")?;

        let mut data = vec![0u8; len];
        file.read_exact(&mut data)
            .with_context(|| format!("reading {len} record bytes from {}", path.display()))?;

        records.push(Record {
            dir,
            transport,
            ts_micros,
            data,
        });
    }

    Ok(records)
}

enum ReadOutcome {
    Full,
    Eof,
    Partial(usize),
}

/// Fill `buf` fully; distinguish a clean EOF (zero bytes read) from a partial
/// read (some bytes then EOF).
fn read_full(file: &mut File, buf: &mut [u8]) -> Result<ReadOutcome> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = file
            .read(&mut buf[filled..])
            .context("reading record header")?;
        if n == 0 {
            if filled == 0 {
                return Ok(ReadOutcome::Eof);
            }
            return Ok(ReadOutcome::Partial(filled));
        }
        filled += n;
    }
    Ok(ReadOutcome::Full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_records() {
        let dir = std::env::temp_dir().join(format!("voxcap-test-{}", now_micros()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("roundtrip.voxcap");

        let originals = vec![
            Record {
                dir: Dir::ClientToServer,
                transport: Transport::Tcp,
                ts_micros: 1_234_567,
                data: vec![1, 2, 3, 4],
            },
            Record {
                dir: Dir::ServerToClient,
                transport: Transport::Udp,
                ts_micros: 7_654_321,
                data: vec![],
            },
            Record {
                dir: Dir::ServerToClient,
                transport: Transport::Tcp,
                ts_micros: -42,
                data: vec![0xff; 300],
            },
        ];

        {
            let writer = CaptureWriter::create(&path).expect("create writer");
            let stats = writer.stats();
            let sink = writer.sink();
            for record in &originals {
                // log() stamps its own timestamp, so we assert structural
                // roundtrip (dir/transport/data) rather than exact ts below.
                sink.log(record.dir, record.transport, &record.data);
            }
            let expected = originals.len() as u64;
            for _ in 0..1000 {
                if stats.written.load(Ordering::Relaxed) >= expected {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(stats.written.load(Ordering::Relaxed), expected);
            drop(sink);
            drop(writer);
        }

        let read_back = read_records(&path).expect("read records");
        assert_eq!(read_back.len(), originals.len());
        for (got, expected) in read_back.iter().zip(originals.iter()) {
            assert_eq!(got.dir, expected.dir);
            assert_eq!(got.transport, expected.transport);
            assert_eq!(got.data, expected.data);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_bad_magic() {
        let dir = std::env::temp_dir().join(format!("voxcap-badmagic-{}", now_micros()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("bad.voxcap");
        std::fs::write(&path, b"NOTVOXCAP").expect("write bad file");
        assert!(read_records(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
