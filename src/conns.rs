//! What's using the network: which programs on this computer have
//! connections open and to whose servers, which ones wait for others to
//! connect, and how fast data is moving. Read from /proc without admin
//! rights, so other users' and system programs show by their user's name.
use crate::store::Time;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Proto {
    Tcp,
    Udp,
}

/// One socket from /proc/net/{tcp,udp}{,6}.
#[derive(Clone, Debug, PartialEq)]
struct Socket {
    proto: Proto,
    local: SocketAddr,
    /// `None` when not connected to anything.
    remote: Option<SocketAddr>,
    listening: bool,
    uid: u32,
    inode: u64,
}

/// A connection out to somewhere.
#[derive(Clone, Debug)]
pub struct Link {
    pub proto: Proto,
    pub remote: SocketAddr,
    /// Whose server, like "Google"; "Your network" for local addresses.
    pub owner: Option<String>,
}

/// A program waiting for others to connect.
#[derive(Clone, Debug)]
pub struct Listen {
    pub proto: Proto,
    pub port: u16,
}

#[derive(Clone, Debug)]
pub struct App {
    pub name: String,
    /// Run by someone else or the system, so we can't see which program.
    pub other_user: bool,
    pub links: Vec<Link>,
    pub listens: Vec<Listen>,
}

impl App {
    /// The owners it's talking to, most connections first.
    pub fn owners(&self) -> Vec<(String, usize)> {
        let mut count: HashMap<String, usize> = HashMap::new();
        for l in &self.links {
            *count.entry(l.owner.clone().unwrap_or_else(|| l.remote.ip().to_string())).or_default() += 1;
        }
        let mut v: Vec<(String, usize)> = count.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub at: Time,
    /// Programs with connections out, busiest first.
    pub apps: Vec<App>,
    /// Bytes per second in and out on all real adapters, since the last look.
    pub rate: Option<(f64, f64)>,
    /// When the totals below were read, for the next rate.
    totals: Option<(Instant, u64, u64)>,
}

impl Snapshot {
    pub fn talking(&self) -> impl Iterator<Item = &App> {
        self.apps.iter().filter(|a| !a.links.is_empty())
    }

    pub fn listening(&self) -> impl Iterator<Item = &App> {
        self.apps.iter().filter(|a| !a.listens.is_empty())
    }
}

// -- Reading /proc ------------------------------------------------------------

/// "0100007F:0035" (IPv4) or 32 hex digits and a port (IPv6), each 32-bit
/// word in the computer's own byte order.
fn parse_addr(s: &str) -> Option<SocketAddr> {
    let (ip, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(u32::from_str_radix(ip, 16).ok()?.to_le_bytes())),
        32 => {
            let mut b = [0u8; 16];
            for w in 0..4 {
                let word = u32::from_str_radix(&ip[w * 8..w * 8 + 8], 16).ok()?;
                b[w * 4..w * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
            let v6 = Ipv6Addr::from(b);
            v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4)
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

fn parse_table(text: &str, proto: Proto) -> Vec<Socket> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let local = parse_addr(f.get(1)?)?;
            let remote = parse_addr(f.get(2)?)?;
            let state = u8::from_str_radix(f.get(3)?, 16).ok()?;
            // TCP: 01 connected, 0A listening, the rest coming or going.
            // UDP: 07 for all; a remote port means it's connected.
            // Waiting on 127.0.0.1 only isn't using the network, and a UDP
            // port from the temporary range is a program's own outgoing one.
            let (listening, connected) = match proto {
                Proto::Tcp => (state == 0x0a, state == 0x01),
                Proto::Udp => (remote.port() == 0 && local.port() != 0 && !temporary(local.port()), remote.port() != 0),
            };
            let listening = listening && !local.ip().is_loopback();
            (listening || connected).then_some(())?;
            Some(Socket { proto, local, remote: connected.then_some(remote), listening, uid: f.get(7)?.parse().ok()?, inode: f.get(9)?.parse().ok()? })
        })
        .collect()
}

/// Ports handed out for programs' own outgoing use (32768 to 60999 unless
/// set otherwise).
fn temporary(port: u16) -> bool {
    static RANGE: std::sync::OnceLock<(u16, u16)> = std::sync::OnceLock::new();
    let (lo, hi) = *RANGE.get_or_init(|| {
        let text = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").unwrap_or_default();
        let v: Vec<u16> = text.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        (v.first().copied().unwrap_or(32768), v.get(1).copied().unwrap_or(60999))
    });
    (lo..=hi).contains(&port)
}

fn sockets() -> Vec<Socket> {
    [("tcp", Proto::Tcp), ("tcp6", Proto::Tcp), ("udp", Proto::Udp), ("udp6", Proto::Udp)]
        .iter()
        .flat_map(|(f, p)| parse_table(&std::fs::read_to_string(format!("/proc/net/{f}")).unwrap_or_default(), *p))
        .collect()
}

/// Which program holds each socket, for the programs we may look into
/// (this user's own).
fn holders() -> HashMap<u64, String> {
    let mut map = HashMap::new();
    let Ok(procs) = std::fs::read_dir("/proc") else { return map };
    for p in procs.flatten() {
        let Some(pid) = p.file_name().to_str().filter(|s| s.bytes().all(|b| b.is_ascii_digit())).map(str::to_string) else { continue };
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        let mut name = None;
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            let Some(inode) = target.to_str().and_then(|t| t.strip_prefix("socket:[")).and_then(|t| t.strip_suffix(']')).and_then(|t| t.parse().ok()) else { continue };
            let name = name.get_or_insert_with(|| program(&pid));
            map.insert(inode, name.clone());
        }
    }
    map
}

/// A program's name as people know it.
fn program(pid: &str) -> String {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default().trim().to_string();
    let known = [
        ("firefox", "Firefox"),
        ("firefox-bin", "Firefox"),
        ("chrome", "Chrome"),
        ("chromium", "Chromium"),
        ("brave", "Brave"),
        ("thunderbird", "Thunderbird"),
        ("code", "VS Code"),
        ("discord", "Discord"),
        ("Discord", "Discord"),
        ("slack", "Slack"),
        ("spotify", "Spotify"),
        ("steam", "Steam"),
        ("zoom", "Zoom"),
        ("teams", "Teams"),
        ("signal-desktop", "Signal"),
        ("telegram-deskto", "Telegram"),
        ("dropbox", "Dropbox"),
        ("claude", "Claude"),
        ("net-watch", "Net Watch"),
        ("gnome-shell", "GNOME"),
        ("cosmic-comp", "COSMIC"),
        ("kdeconnectd", "KDE Connect"),
        ("gsd-sharing", "GNOME sharing"),
    ];
    // Some programs name their main thread "MainThread" and the like; the
    // program file says more.
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok().and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()));
    let generic = ["MainThread", "GeckoMain", "Web Content", "Isolated Web Co", "Socket Process", "ThreadPoolForeg"];
    let comm = match &exe {
        Some(e) if generic.contains(&comm.as_str()) => e.clone(),
        _ => comm,
    };
    for n in [Some(comm.as_str()), exe.as_deref()].into_iter().flatten() {
        if let Some((_, nice)) = known.iter().find(|k| k.0.eq_ignore_ascii_case(n)) {
            return nice.to_string();
        }
    }
    let mut c = comm.chars();
    c.next().map_or_else(|| format!("Program {pid}"), |f| f.to_uppercase().chain(c).collect())
}

/// User names by number, from /etc/passwd.
fn users() -> HashMap<u32, String> {
    std::fs::read_to_string("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            Some((f.get(2)?.parse().ok()?, f.first()?.to_string()))
        })
        .collect()
}

/// A system user's programs as people know them.
fn system_name(user: &str) -> String {
    match user {
        "root" => "System (admin)".into(),
        "systemd-resolve" => "DNS (systemd-resolved)".into(),
        "systemd-timesync" => "Clock sync".into(),
        "avahi" => "Device discovery (Avahi)".into(),
        "cups" | "lp" => "Printing".into(),
        "colord" => "Color profiles".into(),
        "geoclue" => "Location".into(),
        "_apt" => "Software updates".into(),
        "nobody" => "System (nobody)".into(),
        u => format!("System ({u})"),
    }
}

/// Bytes in and out on every real adapter, from /proc/net/dev.
fn totals() -> Option<(Instant, u64, u64)> {
    let text = std::fs::read_to_string("/proc/net/dev").ok()?;
    let (mut rx, mut tx) = (0, 0);
    for line in text.lines().skip(2) {
        let (dev, rest) = line.split_once(':')?;
        if !std::path::Path::new("/sys/class/net").join(dev.trim()).join("device").exists() {
            continue;
        }
        let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        rx += f.first().copied().unwrap_or(0);
        tx += f.get(8).copied().unwrap_or(0);
    }
    Some((Instant::now(), rx, tx))
}

fn local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified(),
        IpAddr::V6(v) => v.is_loopback() || v.is_unspecified() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// What's kept between looks so each one is cheap: whose each address is
/// (only new ones are asked about), and which program holds each socket
/// (every program's files are only read again when a socket is new).
#[derive(Default)]
pub struct Known {
    pub owners: HashMap<IpAddr, Option<String>>,
    held: HashMap<u64, String>,
}

/// Look now. `last` gives the speed since then.
pub fn look(last: Option<&Snapshot>, known: &mut Known) -> Snapshot {
    let socks = sockets();
    let users = users();
    let me = users_me();
    if socks.iter().any(|s| Some(s.uid) == me && s.inode != 0 && !known.held.contains_key(&s.inode)) {
        known.held = holders();
    }
    let held = &known.held;
    let owners = &mut known.owners;
    // Whose servers, for addresses not seen before (at most 200 at a time).
    let new: Vec<IpAddr> = socks.iter().filter_map(|s| s.remote.map(|r| r.ip())).filter(|ip| !local(*ip) && !owners.contains_key(ip)).collect::<std::collections::BTreeSet<_>>().into_iter().take(200).collect();
    if !new.is_empty() {
        for (ip, o) in new.iter().zip(crate::route::owners(&new)) {
            owners.insert(*ip, o);
        }
    }
    let mut apps: Vec<App> = Vec::new();
    for s in &socks {
        // Programs talking only to this computer itself aren't using the network.
        if s.remote.is_some_and(|r| r.ip().is_loopback()) {
            continue;
        }
        let (name, other_user) = match held.get(&s.inode) {
            Some(n) => (n.clone(), false),
            None if Some(s.uid) == me => ("This user (program not found)".into(), false),
            None => (system_name(users.get(&s.uid).map_or("unknown", String::as_str)), true),
        };
        let i = match apps.iter().position(|a| a.name == name) {
            Some(i) => i,
            None => {
                apps.push(App { name, other_user, links: Vec::new(), listens: Vec::new() });
                apps.len() - 1
            }
        };
        let app = &mut apps[i];
        if let Some(r) = s.remote {
            let owner = if local(r.ip()) { Some("Your network".into()) } else { owners.get(&r.ip()).cloned().flatten() };
            app.links.push(Link { proto: s.proto, remote: r, owner });
        } else if s.listening && !app.listens.iter().any(|l| l.port == s.local.port() && l.proto == s.proto) {
            app.listens.push(Listen { proto: s.proto, port: s.local.port() });
        }
    }
    apps.sort_by(|a, b| b.links.len().cmp(&a.links.len()).then(a.other_user.cmp(&b.other_user)).then(a.name.cmp(&b.name)));
    apps.retain(|a| !a.links.is_empty() || !a.listens.is_empty());
    for a in &mut apps {
        a.listens.sort_by_key(|l| l.port);
    }
    let now = totals();
    let rate = match (last.and_then(|l| l.totals), now) {
        (Some((t0, r0, x0)), Some((t1, r1, x1))) => {
            let secs = (t1 - t0).as_secs_f64().max(0.1);
            Some((r1.saturating_sub(r0) as f64 / secs, x1.saturating_sub(x0) as f64 / secs))
        }
        _ => None,
    };
    Snapshot { at: chrono::Local::now(), apps, rate, totals: now }
}

/// This user's number, from who owns our own /proc entry.
fn users_me() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// What a port is usually for.
pub fn port_word(proto: Proto, port: u16) -> &'static str {
    match (proto, port) {
        (Proto::Tcp, 443) | (Proto::Udp, 443) => "secure web",
        (Proto::Tcp, 80) => "web",
        (_, 53) => "DNS",
        (Proto::Tcp, 853) => "encrypted DNS",
        (Proto::Tcp, 22) => "SSH",
        (Proto::Tcp, 993 | 143) => "email (reading)",
        (Proto::Tcp, 587 | 465 | 25) => "email (sending)",
        (Proto::Tcp, 5228..=5230) => "Google notifications",
        (Proto::Tcp, 5223) => "Apple notifications",
        (Proto::Udp, 3478 | 19302..=19309) => "calls (STUN)",
        (Proto::Udp, 123) => "clock",
        (Proto::Udp, 5353) => "finding devices (mDNS)",
        (Proto::Udp, 1900) => "finding devices (UPnP)",
        (Proto::Udp, 67 | 68) => "getting an address (DHCP)",
        (Proto::Tcp, 631) | (Proto::Udp, 631) => "printing",
        (Proto::Tcp, 445 | 139) => "file sharing",
        (Proto::Tcp, 3389) => "remote desktop",
        (Proto::Tcp, 5900..=5909) => "screen sharing (VNC)",
        (Proto::Tcp, 1714..=1764) | (Proto::Udp, 1714..=1764) => "KDE Connect",
        (Proto::Tcp, 8080 | 8000 | 3000 | 5173) => "a web server",
        _ => "",
    }
}

/// `net-watch --conns`: look once and print it.
pub fn print_once() {
    let s = look(None, &mut Known::default());
    for a in &s.apps {
        let owners: Vec<String> = a.owners().into_iter().map(|(o, n)| format!("{o} ×{n}")).collect();
        let listens: Vec<String> = a.listens.iter().map(|l| format!("{:?} {}", l.proto, l.port)).collect();
        println!("{:<32} {:>3} out  {}  {}", a.name, a.links.len(), owners.join(", "), if listens.is_empty() { String::new() } else { format!("waits on {}", listens.join(", ")) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_tables() {
        assert_eq!(parse_addr("0100007F:0035"), Some("127.0.0.1:53".parse().unwrap()));
        assert_eq!(parse_addr("00470626000010000000000096231468:01BB"), Some("[2606:4700:10::6814:2396]:443".parse().unwrap()));
        // An IPv4 address carried over IPv6.
        assert_eq!(parse_addr("0000000000000000FFFF0000F101A8C0:0050"), Some("192.168.1.241:80".parse().unwrap()));
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
                   0: 0100007F:0277 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 15293489 1 0\n\
                   1: F101A8C0:D5F8 01010101:01BB 01 00000000:00000000 00:00000000 00000000  1000        0 777 1 0\n\
                   2: F101A8C0:D5F9 01010101:01BB 06 00000000:00000000 00:00000000 00000000  1000        0 0 1 0\n";
        let s = parse_table(tcp, Proto::Tcp);
        assert_eq!(s.len(), 1, "closing ones, and ones waiting on 127.0.0.1 only, are left out");
        assert_eq!((s[0].remote, s[0].uid, s[0].inode), (Some("1.1.1.1:443".parse().unwrap()), 1000, 777));
    }

    #[test]
    fn knows_local_addresses() {
        assert!(local("192.168.1.254".parse().unwrap()) && local("fe80::1".parse().unwrap()) && local("fd00::1".parse().unwrap()));
        assert!(!local("1.1.1.1".parse().unwrap()) && !local("2606:4700::1111".parse().unwrap()));
        assert_eq!(port_word(Proto::Tcp, 443), "secure web");
    }
}
