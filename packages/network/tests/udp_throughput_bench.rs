// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Ad-hoc throughput bench — NOT part of the regular test suite.
// Marked `#[ignore]` so `cargo test` skips it. Run explicitly:
//
//   cargo test -p streamlib-network --test udp_throughput_bench \
//     --release -- --ignored --nocapture --test-threads=1
//
// Two measurements:
//
//   1. Pure `recvmmsg(2)` drain rate — how fast a single tokio recv
//      loop can pull datagrams off a non-blocking UDP socket using
//      the same recvmmsg+async_io shape the real `UdpSourceProcessor`
//      uses. Upper bound on what streamlib can ever achieve on this
//      hardware.
//
//   2. Full UdpSource → iceoryx2 → UdpSink pipeline — the real
//      streamlib path. Delta vs (1) is the per-packet engine
//      overhead.
//
// The Runner-wrapped measurement (#2) installs the bench's tracing
// subscriber FIRST so streamlib's `init()` gracefully no-ops the
// dispatcher install and routes its events through our buffer.
// The bench then greps the buffer for the source's teardown line
// to read `packets_received`.

use std::io::Write;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;
use tokio::io::Interest;
use tokio::net::UdpSocket as TokioUdpSocket;
use tracing_subscriber::fmt::MakeWriter;

#[allow(unused_imports)]
use streamlib_network::{UdpSinkProcessor as _, UdpSourceProcessor as _};

// --------------------------------------------------------------------
// Captured-tracing harness for #2.
// --------------------------------------------------------------------

static GLOBAL_LOG_BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

#[derive(Clone)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLog {
    type Writer = CapturedLog;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn ensure_global_subscriber() -> Arc<Mutex<Vec<u8>>> {
    GLOBAL_LOG_BUF
        .get_or_init(|| {
            // Tell streamlib to skip its own logging init so this
            // bench can own the global subscriber and capture
            // streamlib events for counter-grepping. The constant
            // name lives in the engine crate; we set it by literal
            // string here to avoid a deeper internal import. See
            // `streamlib_engine::core::logging::init`'s
            // DEFER_LOGGING_TO_HOST_ENV constant for what this
            // env var disables.
            // SAFETY: this is set before any Runner::new call, so
            // no streamlib thread has read it yet. No concurrent
            // env mutation in this test binary.
            unsafe {
                std::env::set_var("STREAMLIB_DANGEROUSLY_DEFER_LOGGING_TO_HOST", "1");
            }
            let buf = Arc::new(Mutex::new(Vec::with_capacity(256 * 1024)));
            let writer = CapturedLog(Arc::clone(&buf));
            let subscriber = tracing_subscriber::fmt()
                .with_writer(writer)
                .with_target(false)
                .without_time()
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .finish();
            let _ = tracing::subscriber::set_global_default(subscriber);
            buf
        })
        .clone()
}

/// Pull the most recent `packets_received=N` value off any
/// `UdpSource: stopped` line emitted since `log_start_offset`.
fn parse_packets_received(log: &str) -> u64 {
    log.lines()
        .filter(|line| line.contains("UdpSource: stopped"))
        .filter_map(|line| {
            line.split_whitespace()
                .find_map(|tok| tok.strip_prefix("packets_received="))
                .and_then(|n| n.parse::<u64>().ok())
        })
        .next_back()
        .unwrap_or(0)
}

// --------------------------------------------------------------------
// Shared helpers
// --------------------------------------------------------------------

/// Tiny inline reimplementation of `RecvBatch` for the pure-syscall
/// bench — same shape as `udp_source.rs::recvmmsg_linux::RecvBatch`
/// but reachable from the test crate.
struct BenchRecvBatch {
    slots: Box<[BenchSlot]>,
    mmsghdrs: Box<[libc::mmsghdr]>,
}
struct BenchSlot {
    buffer: Box<[u8; 65_536]>,
    peer_storage: libc::sockaddr_storage,
    iovec: libc::iovec,
}
unsafe impl Send for BenchRecvBatch {}

impl BenchRecvBatch {
    fn new(batch_size: usize) -> Self {
        let mut slots: Vec<BenchSlot> = (0..batch_size)
            .map(|_| BenchSlot {
                buffer: Box::new([0u8; 65_536]),
                peer_storage: unsafe { std::mem::zeroed() },
                iovec: libc::iovec {
                    iov_base: std::ptr::null_mut(),
                    iov_len: 0,
                },
            })
            .collect();
        for slot in slots.iter_mut() {
            slot.iovec.iov_base = slot.buffer.as_mut_ptr() as *mut libc::c_void;
            slot.iovec.iov_len = 65_536;
        }
        let slots = slots.into_boxed_slice();

        let mut mmsghdrs: Vec<libc::mmsghdr> = Vec::with_capacity(batch_size);
        for slot in slots.iter() {
            let hdr = libc::msghdr {
                msg_name: &slot.peer_storage as *const _ as *mut libc::c_void,
                msg_namelen: std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t,
                msg_iov: &slot.iovec as *const _ as *mut libc::iovec,
                msg_iovlen: 1,
                msg_control: std::ptr::null_mut(),
                msg_controllen: 0,
                msg_flags: 0,
            };
            mmsghdrs.push(libc::mmsghdr {
                msg_hdr: hdr,
                msg_len: 0,
            });
        }
        Self {
            slots,
            mmsghdrs: mmsghdrs.into_boxed_slice(),
        }
    }

    fn drain(&mut self, fd: std::os::fd::RawFd) -> std::io::Result<usize> {
        for i in 0..self.slots.len() {
            self.mmsghdrs[i].msg_hdr.msg_namelen =
                std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            self.mmsghdrs[i].msg_len = 0;
        }
        let n = unsafe {
            libc::recvmmsg(
                fd,
                self.mmsghdrs.as_mut_ptr(),
                self.slots.len() as libc::c_uint,
                libc::MSG_DONTWAIT,
                std::ptr::null_mut(),
            )
        };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

fn pick_free_udp_port() -> SocketAddr {
    let probe = UdpSocket::bind("127.0.0.1:0").expect("probe bind");
    let addr = probe.local_addr().expect("probe local_addr");
    drop(probe);
    addr
}

fn build_recv_socket(
    addr: SocketAddr,
    recv_buffer_bytes: u32,
    handle: &tokio::runtime::Handle,
) -> TokioUdpSocket {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .expect("socket()");
    sock.set_reuse_address(true).unwrap();
    sock.set_nonblocking(true).unwrap();
    sock.set_recv_buffer_size(recv_buffer_bytes as usize).unwrap();
    sock.bind(&addr.into()).expect("bind");
    let std_sock: std::net::UdpSocket = sock.into();
    let _g = handle.enter();
    TokioUdpSocket::from_std(std_sock).expect("from_std")
}

/// Spawn `sender_count` sender threads that flood `payload_size`-byte
/// datagrams as fast as the kernel accepts for `duration`. Returns
/// the total sent count and elapsed wall time.
fn run_senders(
    target: SocketAddr,
    payload_size: usize,
    sender_count: usize,
    duration: Duration,
) -> (u64, Duration) {
    let total_sent = Arc::new(AtomicUsize::new(0));
    let burst_start = Instant::now();
    let stop_at = burst_start + duration;

    let mut handles = Vec::with_capacity(sender_count);
    for _ in 0..sender_count {
        let sent_counter = Arc::clone(&total_sent);
        let h = std::thread::spawn(move || {
            let sock = UdpSocket::bind("127.0.0.1:0").expect("sender bind");
            let payload = vec![0u8; payload_size];
            let mut local = 0u64;
            while Instant::now() < stop_at {
                if sock.send_to(&payload, target).is_ok() {
                    local += 1;
                }
            }
            sent_counter.fetch_add(local as usize, Ordering::Relaxed);
        });
        handles.push(h);
    }
    for h in handles {
        h.join().unwrap();
    }
    let elapsed = burst_start.elapsed();
    (total_sent.load(Ordering::Relaxed) as u64, elapsed)
}

// --------------------------------------------------------------------
// (1) Pure recvmmsg drain (no Runner, no iceoryx2, no NetworkPacket
//     allocation per datagram).
// --------------------------------------------------------------------

fn recvmmsg_scenario(
    payload_size: usize,
    sender_count: usize,
    duration: Duration,
    batch_size: usize,
) -> (u64, u64, Duration) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt");
    let handle = rt.handle().clone();

    let bind_addr = pick_free_udp_port();
    let recv_sock = build_recv_socket(bind_addr, 64 * 1024 * 1024, &handle);
    let fd = recv_sock.as_raw_fd();

    let received = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let received_clone = Arc::clone(&received);
    let stop_clone = Arc::clone(&stop);
    let recv_task = std::thread::spawn(move || {
        rt.block_on(async move {
            let mut batch = BenchRecvBatch::new(batch_size);
            while !stop_clone.load(Ordering::Relaxed) {
                let drained = recv_sock
                    .async_io(Interest::READABLE, || batch.drain(fd))
                    .await;
                if let Ok(n) = drained {
                    received_clone.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        });
    });

    std::thread::sleep(Duration::from_millis(50));

    let (sent, elapsed) = run_senders(bind_addr, payload_size, sender_count, duration);

    std::thread::sleep(Duration::from_millis(200));
    stop.store(true, Ordering::Relaxed);
    // Wake the recv_task by poking the socket so its async_io returns.
    let waker = UdpSocket::bind("127.0.0.1:0").unwrap();
    waker.send_to(&[0u8; 1], bind_addr).ok();
    recv_task.join().unwrap();

    (sent, received.load(Ordering::Relaxed), elapsed)
}

#[test]
#[serial]
#[ignore]
fn pure_recvmmsg_throughput_sweep() {
    println!();
    println!("===========================================================================");
    println!("(1) Pure recvmmsg drain — no streamlib runtime, no iceoryx2, no allocation");
    println!("    Kernel: stock Ubuntu 24.04, net.core.rmem_max=212992");
    println!("===========================================================================");
    println!(
        "{:>10} {:>4}  {:>14} {:>14} {:>7}  {:>10}  {:>10}",
        "payload(B)", "S", "sent", "received", "rcv%", "Mpps", "Gbps"
    );

    let duration = Duration::from_secs(3);
    let batch_size = 256;

    for &payload in &[64usize, 256, 1472, 8192] {
        for &senders in &[1usize, 2, 4, 8] {
            let (sent, rcv, elapsed) =
                recvmmsg_scenario(payload, senders, duration, batch_size);
            let secs = elapsed.as_secs_f64();
            let mpps = rcv as f64 / secs / 1_000_000.0;
            let gbps = rcv as f64 * payload as f64 * 8.0 / secs / 1_000_000_000.0;
            let pct = if sent > 0 {
                100.0 * rcv as f64 / sent as f64
            } else {
                0.0
            };
            println!(
                "{:>10} {:>4}  {:>14} {:>14} {:>6.1}%  {:>10.3}  {:>10.3}",
                payload, senders, sent, rcv, pct, mpps, gbps,
            );
        }
    }
    println!("===========================================================================");
    println!();
}

// --------------------------------------------------------------------
// (2) Full streamlib pipeline — Runner + UdpSource + iceoryx2 +
//     UdpSink (sink swallows; we read source's packets_received from
//     its teardown log line).
// --------------------------------------------------------------------

fn pipeline_scenario(
    payload_size: usize,
    sender_count: usize,
    duration: Duration,
    batch_size_cfg: u32,
) -> (u64, u64, Duration) {
    let log_buf = ensure_global_subscriber();
    let log_start = log_buf.lock().unwrap().len();

    let source_bind = pick_free_udp_port();
    let runtime = Runner::new().expect("Runner::new");

    let source_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "network", "UdpSource", "1.0.0"),
            serde_json::json!({
                "bind_addr": source_bind.to_string(),
                "batch_size": batch_size_cfg,
                "recv_buffer_bytes": 64u32 * 1024u32 * 1024u32,
            }),
        ))
        .expect("add UdpSource");

    // UdpSink with no default destination — packets arriving with
    // empty peer_addr would be dropped, but inbound NetworkPackets
    // from UdpSource carry the sender's peer_addr, so the sink
    // tries to echo them back. We don't care about the echo; the
    // source's count is what we measure. Sink is the simplest way
    // to give the iceoryx2 link a registered consumer.
    let sink_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "network", "UdpSink", "1.0.0"),
            serde_json::json!({}),
        ))
        .expect("add UdpSink");

    runtime
        .connect(
            OutputLinkPortRef::new(source_id.as_str(), "packets"),
            InputLinkPortRef::new(sink_id.as_str(), "packets"),
        )
        .expect("connect");

    runtime.start().expect("start");
    std::thread::sleep(Duration::from_millis(250));

    let (sent, elapsed) = run_senders(source_bind, payload_size, sender_count, duration);

    // Drain tail — let the source pull anything still in the kernel
    // recv buffer through to its publish counter.
    std::thread::sleep(Duration::from_millis(300));

    runtime.stop().expect("stop");

    let log = log_buf.lock().unwrap();
    let log_str = String::from_utf8_lossy(&log[log_start..]).to_string();
    let received = parse_packets_received(&log_str);
    drop(log);

    (sent, received, elapsed)
}

#[test]
#[serial]
#[ignore]
fn full_pipeline_throughput_sweep() {
    println!();
    println!("===========================================================================");
    println!("(2) Full streamlib pipeline — Runner + UdpSource + iceoryx2 + UdpSink");
    println!("    Each datagram: kernel recv → recvmmsg drain → NetworkPacket alloc →");
    println!("    rmp_serde encode → iceoryx2 publish → sink consume → send_to back");
    println!("===========================================================================");
    println!(
        "{:>10} {:>4}  {:>14} {:>14} {:>7}  {:>10}  {:>10}",
        "payload(B)", "S", "sent", "received", "rcv%", "Mpps", "Gbps"
    );

    let duration = Duration::from_secs(3);
    let batch_size_cfg = 256u32;

    for &payload in &[64usize, 256, 1472, 8192] {
        for &senders in &[1usize, 2, 4, 8] {
            let (sent, rcv, elapsed) =
                pipeline_scenario(payload, senders, duration, batch_size_cfg);
            let secs = elapsed.as_secs_f64();
            let mpps = rcv as f64 / secs / 1_000_000.0;
            let gbps = rcv as f64 * payload as f64 * 8.0 / secs / 1_000_000_000.0;
            let pct = if sent > 0 {
                100.0 * rcv as f64 / sent as f64
            } else {
                0.0
            };
            println!(
                "{:>10} {:>4}  {:>14} {:>14} {:>6.1}%  {:>10.3}  {:>10.3}",
                payload, senders, sent, rcv, pct, mpps, gbps,
            );
        }
    }
    println!("===========================================================================");
    println!();
}
