//! The DNS check: how fast this computer's name server answers next to the
//! big public ones, whether it answers at all, whether mistyped addresses
//! get sent to an ad page, and whether lookups are encrypted.
use crate::store::Time;
use std::net::{Ipv4Addr, UdpSocket};
#[cfg(target_os = "linux")]
use std::process::Command;
use std::time::{Duration, Instant};

/// Names most people look up, so the answers are usually ready.
const POPULAR: [&str; 8] = ["google.com", "youtube.com", "facebook.com", "amazon.com", "wikipedia.org", "netflix.com", "reddit.com", "microsoft.com"];
/// Made-up names under these make the server go and ask, as for a site
/// nobody near you has opened lately.
const FRESH: [&str; 8] = ["wikipedia.org", "github.com", "bbc.co.uk", "apple.com", "mozilla.org", "ubuntu.com", "cnn.com", "spotify.com"];
const TIMEOUT: Duration = Duration::from_secs(2);

/// The public ones compared, and their second addresses for switching.
pub const PUBLIC: [(&str, [u8; 4], [u8; 4]); 3] = [("Cloudflare", [1, 1, 1, 1], [1, 0, 0, 1]), ("Google", [8, 8, 8, 8], [8, 8, 4, 4]), ("Quad9", [9, 9, 9, 9], [149, 112, 112, 112])];

#[derive(Clone, Debug)]
pub struct Server {
    /// "Your router", "Cloudflare" and so on.
    pub name: String,
    pub ip: Ipv4Addr,
    /// This computer uses it now.
    pub yours: bool,
    /// Middle time for popular names, and for made-up ones.
    pub ready_ms: Option<f32>,
    pub fresh_ms: Option<f32>,
    pub asked: u32,
    pub failed: u32,
}

impl Server {
    pub fn reliable(&self) -> bool {
        self.asked > 0 && self.failed * 4 < self.asked
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub at: Time,
    /// Yours first, then the public ones.
    pub servers: Vec<Server>,
    /// Your server answers made-up names with an address (an ad page).
    pub redirects: Option<bool>,
    /// DNS over TLS, from systemd-resolved.
    pub encrypted: Option<bool>,
}

impl Report {
    pub fn yours(&self) -> Option<&Server> {
        self.servers.iter().find(|s| s.yours)
    }

    /// The fastest public one that answered everything.
    pub fn best_public(&self) -> Option<&Server> {
        self.servers
            .iter()
            .filter(|s| !s.yours && s.reliable() && s.ready_ms.is_some())
            .min_by(|a, b| score(a).total_cmp(&score(b)))
    }

    /// A public one worth switching to: clearly faster than yours, or
    /// yours doesn't answer.
    pub fn better(&self) -> Option<&Server> {
        let best = self.best_public()?;
        match self.yours() {
            Some(y) if y.reliable() => {
                let (mine, theirs) = (score(y), score(best));
                (mine - theirs > 15.0 && mine > theirs * 1.5).then_some(best)
            }
            _ => Some(best),
        }
    }

    /// The answer in plain words: a headline, what it means, and whether
    /// it's a problem.
    pub fn verdict(&self) -> (String, String, bool) {
        let Some(y) = self.yours() else {
            return ("Couldn't find which DNS server this computer uses".into(), "Names may still work through a VPN or another program.".into(), true);
        };
        if !y.reliable() {
            return (
                format!("Your DNS server ({}) doesn't answer reliably", y.ip),
                format!("{} of {} questions got no answer, so websites sometimes won't open. Switching to a public one fixes that.", y.failed, y.asked),
                true,
            );
        }
        let t = |v: Option<f32>| v.map_or("?".into(), crate::diag::ms);
        let mine = t(y.ready_ms);
        if let Some(b) = self.better() {
            return (
                format!("{} would look up sites faster than yours", b.name),
                format!(
                    "Yours takes {mine} for popular sites and {} for others; {} takes {} and {}. Every website waits for this \
                     before it opens, so switching makes them start a little sooner.",
                    t(y.fresh_ms),
                    b.name,
                    t(b.ready_ms),
                    t(b.fresh_ms)
                ),
                true,
            );
        }
        let redirect = if self.redirects == Some(true) { " But it sends mistyped addresses to an ad page; a public one wouldn't." } else { "" };
        (format!("Your DNS is quick: {mine}"), format!("None of the public ones is clearly faster, so there's no need to switch.{redirect}"), self.redirects == Some(true))
    }

    pub fn text(&self) -> String {
        let (head, detail, _) = self.verdict();
        let mut out = format!("Net Watch DNS check, {}\n{head}\n{detail}\n\n", self.at.format("%Y-%m-%d %H:%M"));
        for s in &self.servers {
            let t = |v: Option<f32>| v.map_or("-".into(), crate::diag::ms);
            out += &format!(
                "{:<14} {:<15} popular {:<8} fresh {:<8} {} of {} unanswered{}\n",
                s.name,
                s.ip,
                t(s.ready_ms),
                t(s.fresh_ms),
                s.failed,
                s.asked,
                if s.yours { "  (yours)" } else { "" }
            );
        }
        let yn = |v: Option<bool>, y: &str, n: &str| v.map_or("unknown".to_string(), |b| if b { y.into() } else { n.into() });
        out += &format!("\nMistyped addresses: {}\n", yn(self.redirects, "redirected to an ad page", "answered honestly"));
        out += &format!("Encrypted DNS: {}\n", yn(self.encrypted, "on", "off"));
        out
    }
}

/// What a server is judged on: mostly popular names, partly fresh ones.
fn score(s: &Server) -> f32 {
    let ready = s.ready_ms.unwrap_or(2000.0);
    ready * 0.7 + s.fresh_ms.unwrap_or(ready) * 0.3
}

/// A DNS question: `name`, record type `kind` (1 address, 16 text).
pub fn question(id: u16, name: &str, kind: u8) -> Vec<u8> {
    let mut q = vec![(id >> 8) as u8, id as u8, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, kind, 0, 1]);
    q
}

/// An answer: (result code, how many records). 0 is fine, 3 "no such name".
#[derive(Debug, PartialEq)]
struct Answer {
    code: u8,
    records: u16,
}

fn read_answer(b: &[u8], id: u16) -> Option<Answer> {
    (b.len() >= 12 && b[..2] == id.to_be_bytes() && b[2] & 0x80 != 0).then(|| Answer { code: b[3] & 0x0f, records: u16::from_be_bytes([b[6], b[7]]) })
}

/// Ask `server` for `name`'s address and time it.
fn lookup(sock: &UdpSocket, server: Ipv4Addr, id: u16, name: &str) -> Option<(Duration, Answer)> {
    let start = Instant::now();
    sock.send_to(&question(id, name, 1), (server, 53)).ok()?;
    let mut buf = [0u8; 1500];
    while start.elapsed() < TIMEOUT {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { return None };
        if from.ip() == server
            && let Some(a) = read_answer(&buf[..n], id)
        {
            return Some((start.elapsed(), a));
        }
    }
    None
}

/// A name that surely doesn't exist: "nw-1a2b3c4d5e6f.<under>".
fn made_up(under: &str, n: u32) -> String {
    let seed = Instant::now().elapsed().as_nanos() as u64 ^ (std::process::id() as u64) << 20 ^ n as u64 * 0x9e37_79b9;
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    format!("nw-{:012x}.{under}", (seed ^ t).wrapping_mul(0x2545_f491_4f6c_dd1d) & 0xffff_ffff_ffff)
}

fn middle(mut v: Vec<f32>) -> Option<f32> {
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied()
}

/// Time one server on every name; gives up after three silences in a row.
fn measure(name: String, ip: Ipv4Addr, yours: bool) -> Server {
    let mut s = Server { name, ip, yours, ready_ms: None, fresh_ms: None, asked: 0, failed: 0 };
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return s };
    let _ = sock.set_read_timeout(Some(TIMEOUT));
    let (mut ready, mut fresh, mut quiet) = (Vec::new(), Vec::new(), 0);
    let names = POPULAR.iter().map(|n| (n.to_string(), false)).chain(FRESH.iter().enumerate().map(|(i, n)| (made_up(n, i as u32), true)));
    for (i, (n, is_fresh)) in names.enumerate() {
        if quiet >= 3 {
            break;
        }
        s.asked += 1;
        match lookup(&sock, ip, 0x4e00 + i as u16, &n) {
            Some((d, a)) if a.code == 0 || a.code == 3 => {
                quiet = 0;
                let v = d.as_secs_f32() * 1000.0;
                if is_fresh { fresh.push(v) } else { ready.push(v) }
            }
            _ => {
                quiet += 1;
                s.failed += 1;
            }
        }
    }
    (s.ready_ms, s.fresh_ms) = (middle(ready), middle(fresh));
    s
}

/// Whether `server` answers a made-up .com name with an address.
fn redirects(server: Ipv4Addr) -> Option<bool> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    let _ = sock.set_read_timeout(Some(TIMEOUT));
    let (_, a) = lookup(&sock, server, 0x4eff, &made_up("com", 99))?;
    Some(a.code == 0 && a.records > 0)
}

/// The servers this computer really asks, and whether it encrypts: those
/// of the adapter the internet goes through, and whether Windows has DNS
/// over HTTPS set up for them there (or required for all).
#[cfg(windows)]
fn yours() -> (Vec<Ipv4Addr>, Option<bool>) {
    use crate::win;
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    let all = win::adapters();
    let main = win::default_routes().into_iter().next().and_then(|r| all.iter().find(|a| a.luid == r.luid).cloned());
    let Some(main) = main else { return (Vec::new(), None) };
    let mut servers: Vec<Ipv4Addr> = main.dns.iter().filter_map(|ip| if let std::net::IpAddr::V4(v) = ip { Some(*v) } else { None }).collect();
    servers.dedup();
    let key = format!(r"SYSTEM\CurrentControlSet\Services\Dnscache\InterfaceSpecificParameters\{}\DohInterfaceSettings\Doh", main.guid);
    let set_up = win::reg_subkeys(HKEY_LOCAL_MACHINE, &key);
    let per_server = servers.iter().any(|s| set_up.iter().any(|k| k == &s.to_string()));
    let required = win::reg_dword(HKEY_LOCAL_MACHINE, r"SOFTWARE\Policies\Microsoft\Windows NT\DNSClient", "DoHPolicy").is_some_and(|p| p == 3);
    (servers, Some(per_server || required))
}

/// The servers this computer really asks, and whether it encrypts:
/// systemd-resolved's for the link the internet goes through, else
/// /etc/resolv.conf's.
#[cfg(target_os = "linux")]
fn yours() -> (Vec<Ipv4Addr>, Option<bool>) {
    let mut servers = Vec::new();
    let mut encrypted = None;
    if let Ok(out) = Command::new("resolvectl").env("LC_ALL", "C").arg("status").output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        // One block per link; the one with +DefaultRoute is the way out.
        for block in text.split("\n\n").filter(|b| b.contains("+DefaultRoute")) {
            encrypted = Some(block.contains("+DNSOverTLS") || encrypted == Some(true));
            for line in block.lines() {
                if let Some((_, list)) = line.split_once("DNS Servers:").or_else(|| line.split_once("Current DNS Server:")) {
                    servers.extend(list.split_whitespace().filter_map(|s| s.split('#').next()?.parse::<Ipv4Addr>().ok()));
                }
            }
        }
    }
    if servers.is_empty() {
        let conf = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
        servers = conf.lines().filter_map(|l| l.trim().strip_prefix("nameserver")?.trim().parse::<Ipv4Addr>().ok()).filter(|ip| !ip.is_loopback()).collect();
    }
    let mut seen = Vec::new();
    servers.retain(|s| !seen.contains(s) && {
        seen.push(*s);
        true
    });
    (servers, encrypted)
}

/// What to call a server.
fn label(ip: Ipv4Addr, router: Option<Ipv4Addr>) -> String {
    if let Some((n, ..)) = PUBLIC.iter().find(|p| Ipv4Addr::from(p.1) == ip || Ipv4Addr::from(p.2) == ip) {
        n.to_string()
    } else if Some(ip) == router {
        "Your router".into()
    } else if ip.is_private() {
        "On your network".into()
    } else {
        "Your provider".into()
    }
}

/// Run the check; takes 1 to 5 seconds.
pub fn check() -> Report {
    let (mine, encrypted) = yours();
    let router = crate::probe::gateway().flatten().map(Ipv4Addr::from);
    // Yours (the first, which gets asked first) and the public ones.
    let mut list: Vec<(String, Ipv4Addr, bool)> = mine.first().map(|&ip| (label(ip, router), ip, true)).into_iter().collect();
    for (n, ip, _) in PUBLIC {
        let ip = Ipv4Addr::from(ip);
        if !list.iter().any(|l| l.1 == ip) {
            list.push((n.into(), ip, false));
        }
    }
    let (servers, redirect) = std::thread::scope(|s| {
        let jobs: Vec<_> = list.iter().map(|(n, ip, y)| s.spawn(move || measure(n.clone(), *ip, *y))).collect();
        let redirect = mine.first().and_then(|&ip| redirects(ip));
        (jobs.into_iter().filter_map(|j| j.join().ok()).collect(), redirect)
    });
    Report { at: chrono::Local::now(), servers, redirects: redirect, encrypted }
}

/// `net-watch --dns`: run the check and print it.
pub fn print_once() {
    println!("{}", check().text());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, yours: bool, ready: f32, failed: u32) -> Server {
        Server { name: name.into(), ip: Ipv4Addr::new(1, 1, 1, 1), yours, ready_ms: Some(ready), fresh_ms: Some(ready), asked: 12, failed }
    }

    #[test]
    fn reads_answers() {
        let mut b = question(0x1234, "example.com", 1);
        assert_eq!(read_answer(&b, 0x1234), None, "a question isn't an answer");
        b[2] |= 0x80;
        b[3] = 3;
        assert_eq!(read_answer(&b, 0x1234), Some(Answer { code: 3, records: 0 }));
        assert_eq!(read_answer(&b, 0x9999), None);
        assert!(made_up("com", 1).starts_with("nw-") && made_up("com", 1).ends_with(".com"));
        assert_eq!(middle(vec![30.0, 10.0, 20.0]), Some(20.0));
    }

    #[test]
    fn says_when_to_switch() {
        let mut r = Report { at: chrono::Local::now(), servers: vec![server("Your router", true, 20.0, 0), server("Cloudflare", false, 12.0, 0)], redirects: Some(false), encrypted: Some(false) };
        assert!(r.better().is_none() && !r.verdict().2);
        r.servers[0].ready_ms = Some(60.0);
        r.servers[0].fresh_ms = Some(60.0);
        assert_eq!(r.better().map(|s| s.name.as_str()), Some("Cloudflare"));
        r.servers[0].failed = 6;
        assert!(r.verdict().0.contains("doesn't answer reliably"));
    }
}
