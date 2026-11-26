//! The measurements: a light "is the internet up?" check, and what a speed
//! test (run by Ookla's Speedtest, see `ookla`) reports.
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(3);
/// Well-known anycast hosts run by three different companies.
pub const HOSTS: [(&str, [u8; 4]); 3] = [("Cloudflare", [1, 1, 1, 1]), ("Google", [8, 8, 8, 8]), ("Quad9", [9, 9, 9, 9])];
/// A page that answers "204 No Content"; anything else means a login page
/// or proxy is in the way.
const PAGE_204: &str = "http://cp.cloudflare.com/generate_204";

/// One round of checks.
#[derive(Clone, Debug, Default)]
pub struct Check {
    /// Outside checks that worked, out of `total`.
    pub ok: u8,
    pub total: u8,
    /// Fastest TCP handshake to an outside host.
    pub latency: Option<Duration>,
    /// Whether the router answered; `None` when we can't tell on this system.
    pub router: Option<bool>,
    /// No network at all: Wi-Fi off or cable out.
    pub no_route: bool,
    /// What failed, for the outage log.
    pub failed: Vec<String>,
}

impl Check {
    pub fn up(&self) -> bool {
        self.ok > 0
    }
}

/// Default gateway from /proc/net/route. `Some(None)`: no route at all;
/// `None`: can't tell, including a default route with no router on it (a
/// VPN or a phone's tether), where knocking would only reach this computer.
#[cfg(target_os = "linux")]
pub fn gateway() -> Option<Option<[u8; 4]>> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    let default = table.lines().skip(1).map(|l| l.split_whitespace().collect::<Vec<_>>()).find(|f| f.get(1) == Some(&"00000000"));
    let Some(f) = default else { return Some(None) };
    let ip = u32::from_str_radix(f.get(2)?, 16).ok()?.to_le_bytes();
    (ip != [0; 4]).then_some(Some(ip))
}

/// The router on the default route with the lowest metric, as above.
#[cfg(windows)]
pub fn gateway() -> Option<Option<[u8; 4]>> {
    let Some(r) = crate::win::default_routes().into_iter().next() else { return Some(None) };
    r.router.map(|ip| Some(ip.octets()))
}

/// A TCP handshake; a refusal still proves the host answered.
pub fn tcp(ip: [u8; 4], port: u16) -> Result<Duration, String> {
    let start = Instant::now();
    match TcpStream::connect_timeout(&SocketAddr::from((ip, port)), TIMEOUT) {
        Ok(_) => Ok(start.elapsed()),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => Ok(start.elapsed()),
        Err(e) if e.kind() == ErrorKind::TimedOut => Err("no answer".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// One DNS question (A record for example.com); checks the reply's ID.
pub fn dns(server: [u8; 4]) -> Result<Duration, String> {
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    let id = (std::process::id() as u16) ^ (Instant::now().elapsed().subsec_nanos() as u16);
    let mut q = vec![(id >> 8) as u8, id as u8, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    q.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
    let start = Instant::now();
    sock.send_to(&q, SocketAddr::from((server, 53))).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 512];
    loop {
        let n = match sock.recv(&mut buf) {
            Ok(n) => n,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Err("no answer".into()),
            Err(e) => return Err(e.to_string()),
        };
        if n >= 12 && buf[..2] == q[..2] {
            return Ok(start.elapsed());
        }
    }
}

pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(concat!("net-watch/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

pub fn page_204() -> Result<Duration, String> {
    let start = Instant::now();
    let resp = agent(TIMEOUT + Duration::from_secs(1)).get(PAGE_204).call().map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        204 => Ok(start.elapsed()),
        s => Err(format!("got {s} (a login page or proxy is in the way)")),
    }
}

/// Whether the router answers a knock on any common port.
pub fn router(ip: [u8; 4]) -> bool {
    std::thread::scope(|s| {
        let knocks: Vec<_> = [53, 80, 443].map(|port| s.spawn(move || tcp(ip, port).is_ok())).into();
        knocks.into_iter().any(|k| k.join().unwrap_or(false))
    })
}

/// Run every check at once; takes at most about 4 seconds.
pub fn check() -> Check {
    std::thread::scope(|s| {
        let gw = gateway();
        let router = s.spawn(move || match gw {
            Some(Some(ip)) => Some(router(ip)),
            Some(None) => Some(false),
            None => None,
        });
        let mut jobs: Vec<(String, std::thread::ScopedJoinHandle<Result<Duration, String>>)> = Vec::new();
        for (name, ip) in HOSTS {
            jobs.push((format!("{name} connect"), s.spawn(move || tcp(ip, 443))));
            jobs.push((format!("{name} DNS"), s.spawn(move || dns(ip))));
        }
        jobs.push(("Test page".into(), s.spawn(page_204)));
        let mut c = Check { total: jobs.len() as u8, ..Default::default() };
        for (name, job) in jobs {
            match job.join().unwrap_or_else(|_| Err("crashed".into())) {
                Ok(d) => {
                    c.ok += 1;
                    if name.ends_with("connect") {
                        c.latency = Some(c.latency.map_or(d, |l| l.min(d)));
                    }
                }
                Err(e) => c.failed.push(format!("{name}: {e}")),
            }
        }
        c.router = router.join().unwrap_or(None);
        c.no_route = gw == Some(None);
        c
    })
}

#[derive(Clone, Debug)]
pub struct Speed {
    pub down_mbps: f64,
    pub up_mbps: f64,
    pub ping_ms: f64,
    pub jitter_ms: f64,
    /// Data the test used, both ways.
    pub bytes: u64,
    /// Which server measured it.
    pub server: Option<String>,
    /// The result's page on the service's site, when it keeps one.
    pub link: Option<String>,
}

/// What a running speed test has found so far, for showing it live.
#[derive(Clone, Debug, Default)]
pub struct Live {
    /// "latency", "download" or "upload"; "" before it starts.
    pub step: &'static str,
    /// How long the whole test runs.
    pub secs: f32,
    pub ping_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    /// Speed about four times a second: (seconds into the test, Mbps).
    pub down: Vec<(f32, f32)>,
    pub up: Vec<(f32, f32)>,
    /// The result so far.
    pub down_mbps: Option<f64>,
    pub up_mbps: Option<f64>,
    pub bytes: u64,
    /// Seconds into the test when the upload started.
    pub upload_at: Option<f32>,
    /// The test server, like "AT&T (Atlanta, GA)".
    pub server: Option<String>,
}

/// `NET_WATCH_FAKE_SPEED=1`: a made-up test of `secs` with no network use,
/// for working on the live view.
pub fn fake_test(secs: u64, report: &dyn Fn(&Live)) -> Result<Speed, String> {
    let start = Instant::now();
    let mut live = Live {
        secs: secs as f32,
        step: "download",
        ping_ms: Some(16.0),
        jitter_ms: Some(1.2),
        upload_at: Some(secs as f32 / 2.0),
        server: Some("Pretend server (Nowhere, XX)".into()),
        ..Default::default()
    };
    let wave = |t: f32, base: f32| base * (0.85 + 0.15 * (t * 1.7).sin()) * if (t * 0.6).fract() < 0.6 { 1.0 } else { 0.3 };
    loop {
        let t = start.elapsed().as_secs_f32();
        if t >= live.secs {
            break;
        }
        live.step = if t < live.secs / 2.0 { "download" } else { "upload" };
        let (base, samples) = if live.step == "download" { (850.0, &mut live.down) } else { (300.0, &mut live.up) };
        samples.push((t, wave(t, base)));
        live.bytes += (wave(t, base) / 8.0 * 0.25 * 1e6) as u64;
        if live.step == "download" { live.down_mbps = Some(840.0) } else { live.up_mbps = Some(295.0) }
        report(&live);
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(Speed { down_mbps: 840.0, up_mbps: 295.0, ping_ms: 16.0, jitter_ms: 1.2, bytes: live.bytes, server: live.server, link: None })
}

/// `net-watch --probe`: run one check and one speed test and print them.
pub fn print_once() {
    println!("Checking the connection…");
    let c = check();
    let router = match c.router {
        Some(true) => "router answered",
        Some(false) => "router didn't answer",
        None => "router not checked",
    };
    println!("  {} of {} outside checks worked, {router}", c.ok, c.total);
    if let Some(l) = c.latency {
        println!("  latency {:.1} ms", l.as_secs_f64() * 1000.0);
    }
    for f in &c.failed {
        println!("  failed: {f}");
    }
    println!("  => {}", if c.up() { "UP" } else { "DOWN" });
    println!("Speed test (Ookla Speedtest)…");
    match crate::ookla::speed_test(&|_| {}) {
        Ok(s) => println!(
            "  download {:.1} Mbps, upload {:.1} Mbps, ping {:.1} ms, jitter {:.1} ms, used {:.1} MB",
            s.down_mbps,
            s.up_mbps,
            s.ping_ms,
            s.jitter_ms,
            s.bytes as f64 / 1e6
        ),
        Err(e) => println!("  failed: {e}"),
    }
}
