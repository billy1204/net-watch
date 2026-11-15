//! The firewall check, run when asked: whether this computer's firewall is
//! on, and which kinds of traffic get out to the internet. Something that
//! doesn't get out is stopped by this computer, the router or the provider.
use crate::store::Time;
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Got out and back, in this long.
    Open(Duration),
    /// Nothing came back: quietly dropped.
    NoAnswer,
    /// Something answered "no".
    TurnedAway,
    /// The server's name couldn't be looked up, so we can't tell.
    NoName,
    Failed(String),
}

#[derive(Clone, Copy)]
enum How {
    Tcp,
    Dns,
    Ntp,
    Stun,
}

/// One kind of traffic: what it's for, its technical name, where it's tried.
struct Kind {
    name: &'static str,
    what: &'static str,
    how: How,
    host: &'static str,
    port: u16,
    /// Why being blocked is nothing to worry about, for those often blocked
    /// on purpose.
    fine: Option<&'static str>,
}

const KINDS: [Kind; 12] = [
    Kind { name: "Websites", what: "HTTPS · TCP 443", how: How::Tcp, host: "1.1.1.1", port: 443, fine: None },
    Kind { name: "Plain websites", what: "HTTP · TCP 80", how: How::Tcp, host: "1.1.1.1", port: 80, fine: None },
    Kind { name: "Looking up names", what: "DNS · UDP 53", how: How::Dns, host: "1.1.1.1", port: 53, fine: None },
    Kind { name: "Looking up names (TCP)", what: "DNS · TCP 53", how: How::Tcp, host: "1.1.1.1", port: 53, fine: None },
    Kind { name: "Private name lookups", what: "DNS over TLS · TCP 853", how: How::Tcp, host: "1.1.1.1", port: 853, fine: None },
    Kind { name: "Setting the clock", what: "NTP · UDP 123", how: How::Ntp, host: "time.cloudflare.com", port: 123, fine: None },
    Kind { name: "Video calls and games", what: "STUN · UDP 19302", how: How::Stun, host: "stun.l.google.com", port: 19302, fine: None },
    Kind { name: "Reading email", what: "IMAP · TCP 993", how: How::Tcp, host: "imap.gmail.com", port: 993, fine: None },
    Kind { name: "Sending email", what: "SMTP · TCP 587", how: How::Tcp, host: "smtp.gmail.com", port: 587, fine: None },
    Kind { name: "Remote login", what: "SSH · TCP 22", how: How::Tcp, host: "github.com", port: 22, fine: None },
    Kind {
        name: "Mail servers",
        what: "SMTP · TCP 25",
        how: How::Tcp,
        host: "gmail-smtp-in.l.google.com",
        port: 25,
        fine: Some("Most home providers block this on purpose, against spam. Your email app uses the line above."),
    },
    Kind {
        name: "Newer addresses",
        what: "IPv6 · TCP 443",
        how: How::Tcp,
        host: "2606:4700:4700::1111",
        port: 443,
        fine: Some("Not every provider or router offers IPv6 yet; everything works without it."),
    },
];

#[derive(Clone, Debug)]
pub struct Line {
    pub name: &'static str,
    pub what: &'static str,
    pub host: &'static str,
    pub outcome: Outcome,
    pub fine: Option<&'static str>,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub at: Time,
    /// What we found about this computer's own firewall, in a sentence.
    pub computer: String,
    pub computer_on: Option<bool>,
    pub lines: Vec<Line>,
}

impl Report {
    /// Lines that didn't get out, leaving out the often-blocked ones.
    pub fn blocked(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter().filter(|l| l.fine.is_none() && !matches!(l.outcome, Outcome::Open(_) | Outcome::NoName))
    }
}

fn address(host: &str, port: u16) -> Result<SocketAddr, Outcome> {
    (host, port).to_socket_addrs().ok().and_then(|mut a| a.next()).ok_or(Outcome::NoName)
}

fn tcp(to: SocketAddr) -> Outcome {
    let start = Instant::now();
    match TcpStream::connect_timeout(&to, TIMEOUT) {
        Ok(_) => Outcome::Open(start.elapsed()),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => Outcome::TurnedAway,
        Err(e) if e.kind() == ErrorKind::TimedOut => Outcome::NoAnswer,
        Err(e) if e.kind() == ErrorKind::NetworkUnreachable || e.raw_os_error() == Some(101) => Outcome::Failed("no route there".into()),
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Send one UDP packet and wait for a reply that `ok` accepts.
fn udp(to: SocketAddr, packet: &[u8], ok: impl Fn(&[u8]) -> bool) -> Outcome {
    let run = || -> std::io::Result<Outcome> {
        let sock = UdpSocket::bind(if to.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
        sock.set_read_timeout(Some(TIMEOUT))?;
        sock.connect(to)?;
        let start = Instant::now();
        sock.send(packet)?;
        let mut buf = [0u8; 1024];
        loop {
            match sock.recv(&mut buf) {
                Ok(n) if ok(&buf[..n]) => return Ok(Outcome::Open(start.elapsed())),
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(Outcome::NoAnswer),
                Err(e) if e.kind() == ErrorKind::ConnectionRefused => return Ok(Outcome::TurnedAway),
                Err(e) => return Err(e),
            }
        }
    };
    run().unwrap_or_else(|e| Outcome::Failed(e.to_string()))
}

fn try_kind(k: &Kind) -> Outcome {
    let to = match address(k.host, k.port) {
        Ok(a) => a,
        Err(o) => return o,
    };
    match k.how {
        How::Tcp => tcp(to),
        How::Dns => {
            let mut q = vec![0x4e, 0x57, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
            q.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
            udp(to, &q, |r| r.len() >= 12 && r[..2] == [0x4e, 0x57])
        }
        // A time request: "version 3, from a client".
        How::Ntp => {
            let mut q = [0u8; 48];
            q[0] = 0x1b;
            udp(to, &q, |r| r.len() >= 48 && r[0] & 0x07 == 4)
        }
        // "What's my address?", which video calls ask to find a way through.
        How::Stun => {
            let mut q = vec![0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42];
            q.extend_from_slice(b"net-watch-id");
            udp(to, &q, |r| r.len() >= 20 && r[..2] == [0x01, 0x01] && r[8..20] == *b"net-watch-id")
        }
    }
}

/// This computer's own firewall, as far as we can tell without admin rights.
#[cfg(target_os = "linux")]
fn computer() -> (String, Option<bool>) {
    let active = |unit: &str| {
        std::process::Command::new("systemctl")
            .args(["is-active", unit])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
    };
    // ufw's service stays "active" even when ufw is off; its settings file
    // says whether it's on.
    if let Ok(conf) = std::fs::read_to_string("/etc/ufw/ufw.conf") {
        let on = conf.lines().any(|l| l.trim().eq_ignore_ascii_case("ENABLED=yes"));
        if on {
            return ("On: the ufw firewall is turned on. It normally lets everything out and stops others getting in.".into(), Some(true));
        }
        if !active("firewalld") {
            return ("Off: the ufw firewall is installed but turned off.".into(), Some(false));
        }
    }
    if active("firewalld") {
        return ("On: firewalld is running. It normally lets everything out and stops others getting in.".into(), Some(true));
    }
    if active("nftables") {
        return ("On: nftables firewall rules are loaded at startup.".into(), Some(true));
    }
    ("No firewall program found on this computer (other programs, like VPNs, can still set rules).".into(), Some(false))
}

#[cfg(windows)]
fn computer() -> (String, Option<bool>) {
    use std::os::windows::process::CommandExt;
    const NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("netsh").args(["advfirewall", "show", "allprofiles", "state"]).creation_flags(NO_WINDOW).output();
    let Ok(out) = out else { return ("Couldn't ask Windows about its firewall.".into(), None) };
    let text = String::from_utf8_lossy(&out.stdout);
    // "Domain Profile Settings:" … "State  ON", for each of the three.
    let mut profile = "";
    let mut states = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.trim().strip_suffix(" Profile Settings:") {
            profile = if name.contains("Domain") { "work" } else if name.contains("Private") { "home" } else { "public" };
        } else if let Some(state) = line.trim().strip_prefix("State") {
            states.push((profile, state.trim().eq_ignore_ascii_case("ON")));
        }
    }
    match (states.iter().all(|s| s.1), states.iter().any(|s| s.1)) {
        _ if states.is_empty() => ("Couldn't read Windows Firewall's state.".into(), None),
        (true, _) => ("On: Windows Firewall is on for every kind of network.".into(), Some(true)),
        (false, true) => {
            let off: Vec<&str> = states.iter().filter(|s| !s.1).map(|s| s.0).collect();
            (format!("Mostly on: Windows Firewall is off for {} networks.", off.join(" and ")), Some(true))
        }
        (false, false) => ("Off: Windows Firewall is turned off.".into(), Some(false)),
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn computer() -> (String, Option<bool>) {
    ("Can't tell on this system.".into(), None)
}

/// Run the whole check; takes about 5 seconds and almost no data.
pub fn check() -> Report {
    std::thread::scope(|s| {
        let me = s.spawn(computer);
        let jobs: Vec<_> = KINDS.iter().map(|k| (k, s.spawn(move || try_kind(k)))).collect();
        let lines = jobs
            .into_iter()
            .map(|(k, job)| Line {
                name: k.name,
                what: k.what,
                host: k.host,
                outcome: job.join().unwrap_or_else(|_| Outcome::Failed("crashed".into())),
                fine: k.fine,
            })
            .collect();
        let (computer, computer_on) = me.join().unwrap_or(("Couldn't tell.".into(), None));
        Report { at: chrono::Local::now(), computer, computer_on, lines }
    })
}

/// "Gets out" or why not, in a few words.
pub fn verdict(o: &Outcome) -> (&'static str, String) {
    match o {
        Outcome::Open(d) => ("Gets out", format!("answered in {} ms", d.as_millis())),
        Outcome::NoAnswer => ("Blocked", "no answer: quietly dropped".into()),
        Outcome::TurnedAway => ("Blocked", "turned away".into()),
        Outcome::NoName => ("Couldn't tell", "couldn't look up the server's name".into()),
        Outcome::Failed(e) => ("Blocked", e.clone()),
    }
}
