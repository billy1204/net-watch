//! The device scan, run when asked: what's on the home network, with names,
//! makers and a guess at what each one is. Every address on the network is
//! sent a tiny "hello" (which also asks the device's name), then the
//! computer's own address table says which ones are there. Nothing leaves
//! the home network, and no admin rights are needed.
use crate::store::Time;
use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Ports that say what a device is, when it answers on them.
const PORTS: [(u16, &str); 19] = [
    (8060, "Roku"),
    (8009, "Chromecast or Google speaker"),
    (62078, "iPhone or iPad"),
    (7000, "AirPlay"),
    (1400, "Sonos speaker"),
    (9100, "Printer"),
    (631, "Printer"),
    (554, "Camera"),
    (8123, "Home Assistant"),
    (32400, "Plex server"),
    (1883, "Smart-home hub"),
    (445, "Shares files"),
    (548, "Mac file sharing"),
    (3389, "Windows remote desktop"),
    (22, "Remote login"),
    (53, "Name server"),
    (80, "Web page"),
    (443, "Web page"),
    (8080, "Web page"),
];

#[derive(Clone, Debug)]
pub struct Device {
    pub ip: Ipv4Addr,
    /// Hardware address, like "02:1a:2b:3c:4d:5e".
    pub mac: Option<String>,
    pub name: Option<String>,
    pub maker: Option<String>,
    /// A made-up hardware address, as phones and laptops use for privacy.
    pub private_mac: bool,
    pub ports: Vec<u16>,
    pub me: bool,
    pub router: bool,
    /// When a scan first saw it.
    pub first_seen: Time,
    /// First seen in the last day (and not by the very first scan).
    pub new: bool,
    /// Not seen by any earlier scan: it just joined.
    pub joined: bool,
}

impl Device {
    /// What it might be, from the ports it answers on: "Roku · Web page".
    pub fn guess(&self) -> String {
        let mut seen = Vec::new();
        for (port, what) in PORTS {
            if self.ports.contains(&port) && !seen.contains(&what) {
                seen.push(what);
            }
        }
        seen.join(" · ")
    }

    /// For a pop-up: "Pixel-9a (192.168.1.239)", or by maker or guess
    /// when it has no name.
    pub fn label(&self) -> String {
        let what = self.name.clone().or_else(|| self.maker.clone()).or_else(|| Some(self.guess()).filter(|g| !g.is_empty()));
        match what {
            Some(w) => format!("{w} ({})", self.ip),
            None => format!("Unnamed device ({})", self.ip),
        }
    }

    pub fn maker_text(&self) -> String {
        match (&self.maker, self.private_mac) {
            (Some(m), _) => m.clone(),
            (None, true) => "Private address".into(),
            (None, false) => String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Scan {
    pub at: Time,
    /// The range scanned, like "192.168.1.0/24".
    pub range: String,
    pub devices: Vec<Device>,
    /// No earlier scan to compare with.
    pub first: bool,
    /// The router's hardware address, which tells one network from another.
    pub network: Option<String>,
}

/// This computer's address, the network's range, the network adapter and
/// the router.
struct Net {
    me: Ipv4Addr,
    hosts: Vec<Ipv4Addr>,
    range: String,
    iface: Option<String>,
    router: Option<Ipv4Addr>,
}

/// The address this computer uses to reach `to` (nothing is sent).
fn my_address(to: Ipv4Addr) -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect((to, 9)).ok()?;
    match s.local_addr().ok()? {
        SocketAddr::V4(a) => Some(*a.ip()),
        _ => None,
    }
}

/// The addresses around `me` within `mask` bits, at most 1024 of them
/// (else just the 256 around it).
fn hosts(me: Ipv4Addr, bits: u32) -> (Vec<Ipv4Addr>, String) {
    let bits = if bits < 22 || bits > 30 { 24 } else { bits };
    let mask = u32::MAX << (32 - bits);
    let net = u32::from(me) & mask;
    let hosts = (net + 1..net + !mask).map(Ipv4Addr::from).collect();
    (hosts, format!("{}/{bits}", Ipv4Addr::from(net)))
}

fn network() -> Result<Net, String> {
    let table = std::fs::read_to_string("/proc/net/route").map_err(|e| e.to_string())?;
    let rows: Vec<Vec<&str>> = table.lines().skip(1).map(|l| l.split_whitespace().collect()).collect();
    let hex = |s: &str| u32::from_str_radix(s, 16).ok().map(|v| Ipv4Addr::from(v.to_le_bytes()));
    let default = rows.iter().find(|r| r.get(1) == Some(&"00000000")).ok_or("not connected to a network")?;
    let iface = default[0].to_string();
    // No router on the default route (a VPN or tether): no home network
    // to scan from here.
    let router = default.get(2).and_then(|g| hex(g)).filter(|r| !r.is_unspecified()).ok_or("no router on this connection (a VPN or tether?)")?;
    let me = my_address(router).ok_or("couldn't find this computer's address")?;
    // The route for the home network itself, on the same adapter.
    let bits = rows
        .iter()
        .filter(|r| r[0] == iface && r.get(2) == Some(&"00000000"))
        .filter_map(|r| hex(r.get(7)?))
        .map(|m| u32::from(m).count_ones())
        .find(|&b| b > 0)
        .unwrap_or(24);
    let (hosts, range) = hosts(me, bits);
    Ok(Net { me, hosts, range, iface: Some(iface), router: Some(router) })
}

/// The computer's address table: which addresses on the network answered,
/// with their hardware addresses.
fn neighbours(net: &Net) -> HashMap<Ipv4Addr, String> {
    let table = std::fs::read_to_string("/proc/net/arp").unwrap_or_default();
    table
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            // Flag 0x2: the device answered; 0x0 means it didn't.
            let ok = f.get(2).and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).is_some_and(|v| v & 2 != 0);
            (ok && f.get(5).copied() == net.iface.as_deref()).then(|| Some((f[0].parse().ok()?, f.get(3)?.to_lowercase())))?
        })
        .collect()
}

/// A DNS question for the name of `ip` ("4.3.2.1.in-addr.arpa").
pub fn name_question(id: u16, ip: Ipv4Addr) -> Vec<u8> {
    let mut q = vec![(id >> 8) as u8, id as u8, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    let o = ip.octets();
    for label in [o[3].to_string(), o[2].to_string(), o[1].to_string(), o[0].to_string(), "in-addr".into(), "arpa".into()] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 12, 0, 1]);
    q
}

/// A Windows-style "what are your names?" question.
fn netbios_question() -> Vec<u8> {
    let mut q = vec![0x4e, 0x57, 0x00, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, 0x20, b'C', b'K'];
    q.extend_from_slice(&[b'A'; 30]);
    q.extend_from_slice(&[0, 0, 0x21, 0, 1]);
    q
}

/// A name in a DNS message at `i`, following pointers; and where it ends.
pub fn read_name(b: &[u8], mut i: usize) -> Option<(String, usize)> {
    let (mut labels, mut end, mut hops) = (Vec::new(), None, 0);
    loop {
        let len = *b.get(i)? as usize;
        if len == 0 {
            end.get_or_insert(i + 1);
            break;
        }
        if len & 0xc0 == 0xc0 {
            end.get_or_insert(i + 2);
            i = ((len & 0x3f) << 8) | *b.get(i + 1)? as usize;
            hops += 1;
            if hops > 20 {
                return None;
            }
            continue;
        }
        labels.push(String::from_utf8_lossy(b.get(i + 1..i + 1 + len)?).into_owned());
        i += 1 + len;
    }
    Some((labels.join("."), end?))
}

/// The name in a DNS answer to `name_question`.
pub fn name_answer(b: &[u8]) -> Option<String> {
    if b.len() < 12 || b[2] & 0x80 == 0 {
        return None;
    }
    let count = |at: usize| u16::from_be_bytes([b[at], b[at + 1]]) as usize;
    let mut i = 12;
    for _ in 0..count(4) {
        i = read_name(b, i)?.1 + 4;
    }
    for _ in 0..count(6) + count(8) + count(10) {
        let (_, j) = read_name(b, i)?;
        let kind = u16::from_be_bytes([*b.get(j)?, *b.get(j + 1)?]);
        let len = u16::from_be_bytes([*b.get(j + 8)?, *b.get(j + 9)?]) as usize;
        if kind == 12 {
            return read_name(b, j + 10).map(|n| n.0);
        }
        i = j + 10 + len;
    }
    None
}

/// The device's own name in a reply to `netbios_question`.
fn netbios_answer(b: &[u8]) -> Option<String> {
    let n = *b.get(56)? as usize;
    (0..n).find_map(|k| {
        let e = b.get(57 + k * 18..75 + k * 18)?;
        let group = e[16] & 0x80 != 0;
        let name = String::from_utf8_lossy(&e[..15]).trim().to_string();
        (e[15] == 0 && !group && !name.is_empty()).then_some(name)
    })
}

/// "Living-Room-TV.local." or "Billys-iPhone.attlocal.net" → its first part.
fn short(name: &str) -> Option<String> {
    let first = name.split('.').next()?.trim();
    let meaningless = ["wlan0", "eth0", "localhost", "unknown", "android"].iter().any(|m| first.eq_ignore_ascii_case(m));
    (!first.is_empty() && !meaningless && first.parse::<u8>().is_err()).then(|| first.to_string())
}

/// Say hello to every address, then ask the ones that are there their
/// names; collect the names that come back.
fn say_hello(net: &Net) -> (HashMap<Ipv4Addr, String>, HashSet<Ipv4Addr>) {
    let mut names: HashMap<Ipv4Addr, (u8, String)> = HashMap::new();
    let mut answered = HashSet::new();
    let (Ok(hello), Ok(sock)) = (UdpSocket::bind("0.0.0.0:0"), UdpSocket::bind("0.0.0.0:0")) else { return Default::default() };
    // Port 9 throws everything away; the point is the computer finding out
    // whether the address is there. Hellos to empty addresses wait in the
    // computer for a few seconds, so a busy socket skips rather than waits.
    let _ = hello.set_nonblocking(true);
    for ip in &net.hosts {
        if *ip != net.me {
            let _ = hello.send_to(&[0], (*ip, 9));
        }
    }
    std::thread::sleep(Duration::from_millis(1200));
    // "What's your name?" to those that are there: the Apple/Linux way, the
    // Windows way, and to the router.
    let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
    let netbios = netbios_question();
    for ip in neighbours(net).keys().filter(|ip| net.hosts.contains(ip)) {
        let n = net.hosts.iter().position(|h| h == ip).unwrap_or(0) as u16;
        let _ = sock.send_to(&name_question(n, *ip), (*ip, 5353));
        let _ = sock.send_to(&netbios, (*ip, 137));
        if let Some(r) = net.router {
            let _ = sock.send_to(&name_question(n, *ip), (r, 53));
        }
    }
    let until = Instant::now() + Duration::from_millis(1500);
    let mut buf = [0u8; 1500];
    while Instant::now() < until {
        let Ok((len, SocketAddr::V4(from))) = sock.recv_from(&mut buf) else { continue };
        let b = &buf[..len];
        // Better names first: the router's (often the one people gave the
        // device), then the device's own, then the Windows one.
        let found = match from.port() {
            53 if Some(*from.ip()) == net.router => {
                let id = u16::from_be_bytes([b.first().copied().unwrap_or(0), b.get(1).copied().unwrap_or(0)]) as usize;
                net.hosts.get(id).and_then(|ip| Some((*ip, 0, name_answer(b)?)))
            }
            5353 => name_answer(b).map(|n| (*from.ip(), 1, n)),
            137 => netbios_answer(b).map(|n| (*from.ip(), 2, n)),
            _ => None,
        };
        if from.port() != 53 {
            answered.insert(*from.ip());
        }
        if let Some((ip, rank, name)) = found
            && let Some(name) = short(&name)
            && names.get(&ip).is_none_or(|(r, _)| rank < *r)
        {
            names.insert(ip, (rank, name));
        }
    }
    (names.into_iter().map(|(ip, (_, n))| (ip, n)).collect(), answered)
}

/// Which of `PORTS` each device answers on, many at once.
fn knock(ips: &[Ipv4Addr]) -> Vec<Vec<u16>> {
    let jobs: Vec<(usize, u16)> = (0..ips.len()).flat_map(|i| PORTS.iter().map(move |p| (i, p.0))).collect();
    let next = AtomicUsize::new(0);
    let open = Mutex::new(vec![Vec::new(); ips.len()]);
    std::thread::scope(|s| {
        for _ in 0..jobs.len().min(96) {
            s.spawn(|| {
                while let Some(&(i, port)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if TcpStream::connect_timeout(&SocketAddr::from((ips[i], port)), Duration::from_millis(600)).is_ok() {
                        open.lock().unwrap()[i].push(port);
                    }
                }
            });
        }
    });
    open.into_inner().unwrap()
}

/// Makers for hardware addresses, from the IEEE list when the system has a
/// copy (Debian and Ubuntu's ieee-data).
fn makers(macs: &HashSet<String>) -> HashMap<String, String> {
    let want: HashSet<String> = macs.iter().filter_map(|m| prefix(m)).collect();
    let list = ["/usr/share/ieee-data/oui.csv", "/var/lib/ieee-data/oui.csv"].iter().find_map(|p| std::fs::read_to_string(p).ok());
    let mut found = HashMap::new();
    for line in list.as_deref().unwrap_or("").lines() {
        let mut parts = line.splitn(3, ',');
        let (Some(_), Some(prefix), Some(rest)) = (parts.next(), parts.next(), parts.next()) else { continue };
        if !want.contains(prefix) {
            continue;
        }
        let name = match rest.strip_prefix('"') {
            Some(q) => q.split('"').next().unwrap_or(""),
            None => rest.split(',').next().unwrap_or(""),
        };
        found.insert(prefix.to_string(), tidy_maker(name));
    }
    found
}

/// The maker's part of a hardware address: "48:41:7b:…" → "48417B".
fn prefix(mac: &str) -> Option<String> {
    Some(mac.replace(['-', ':'], "").get(..6)?.to_uppercase())
}

/// "Roku, Inc" → "Roku"; "SAMSUNG ELECTRO-MECHANICS(THAILAND)" → "Samsung Electro-Mechanics".
fn tidy_maker(name: &str) -> String {
    let mut n = name.split([',', '(']).next().unwrap_or(name).trim().to_string();
    for end in [" Inc.", " Inc", " Corporation", " Corp.", " Corp", " Co.", " Ltd.", " Ltd", " LLC", " Limited", " GmbH", " Technologies", " Technology"] {
        if n.len() > end.len() && n.to_lowercase().ends_with(&end.to_lowercase()) {
            n.truncate(n.len() - end.len());
        }
    }
    if n.chars().any(|c| c.is_lowercase()) {
        return n;
    }
    // ALL CAPITALS → Title Case.
    let mut start = true;
    n.chars()
        .map(|c| {
            let out = if start { c } else { c.to_ascii_lowercase() };
            start = !c.is_alphanumeric();
            out
        })
        .collect()
}

fn my_mac(net: &Net) -> Option<String> {
    let iface = net.iface.as_ref()?;
    let mac = std::fs::read_to_string(format!("/sys/class/net/{iface}/address")).ok()?;
    Some(mac.trim().to_lowercase())
}

/// A device an earlier scan saw, from `devices.csv`.
struct Seen {
    mac: String,
    first: Time,
    name: String,
}

const STAMP: &str = "%Y-%m-%d %H:%M:%S";

/// Each network keeps its own list, named after its router, so a scan at
/// work or a café doesn't fill the home list with "new" devices.
fn history_file(network: Option<&str>) -> Option<std::path::PathBuf> {
    let dir = crate::store::data_dir()?;
    let Some(router) = network else { return Some(dir.join("devices-unknown.csv")) };
    let path = dir.join(format!("devices-{}.csv", router.replace(':', "")));
    // The list from before there was one per network was made at home.
    let old = dir.join("devices.csv");
    if !path.exists() && old.exists() {
        let _ = std::fs::rename(old, &path);
    }
    Some(path)
}

/// The router's hardware address on the network the computer is on now,
/// without scanning; `None` when there's no router to tell by.
pub fn current_network() -> Option<String> {
    let net = network().ok()?;
    neighbours(&net).get(&net.router?).cloned()
}

/// The devices earlier scans saw; `None` before the first scan.
fn history(network: Option<&str>) -> Option<Vec<Seen>> {
    let text = std::fs::read_to_string(history_file(network)?).ok()?;
    let rows = text.lines().skip(1).filter_map(|l| {
        let f: Vec<&str> = l.split(',').collect();
        let first = chrono::NaiveDateTime::parse_from_str(f.get(1)?, STAMP).ok()?.and_local_timezone(chrono::Local).earliest()?;
        Some(Seen { mac: f[0].to_string(), first, name: f.get(4)?.to_string() })
    });
    Some(rows.collect())
}

/// Add this scan to `devices.csv`: each device once, with when it was
/// first and last seen.
fn remember(scan: &Scan) {
    let Some(path) = history_file(scan.network.as_deref()) else { return };
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut rows: Vec<Vec<String>> = old.lines().skip(1).map(|l| l.split(',').map(str::to_string).collect()).filter(|r: &Vec<String>| r.len() >= 6).collect();
    let at = scan.at.format(STAMP).to_string();
    let clean = |s: &str| s.replace([',', '\n'], " ");
    for d in &scan.devices {
        let Some(mac) = &d.mac else { continue };
        let name = clean(d.name.as_deref().unwrap_or(""));
        match rows.iter_mut().find(|r| r[0] == *mac) {
            Some(r) => {
                r[2] = at.clone();
                r[3] = d.ip.to_string();
                if !name.is_empty() {
                    r[4] = name;
                }
                r[5] = clean(&d.maker_text());
            }
            None => rows.push(vec![mac.clone(), d.first_seen.format(STAMP).to_string(), at.clone(), d.ip.to_string(), name, clean(&d.maker_text())]),
        }
    }
    let mut out = String::from("hardware_address,first_seen,last_seen,address,name,maker\n");
    for r in rows {
        out += &r.join(",");
        out.push('\n');
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("csv.tmp");
    if std::fs::write(&tmp, out).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

/// Run the whole scan; takes about 5 seconds.
pub fn scan() -> Result<Scan, String> {
    let net = network()?;
    let t = Instant::now();
    let (names, answered) = say_hello(&net);
    debug(&format!("hello {:?}", t.elapsed()));
    let table = neighbours(&net);
    let mut ips: Vec<Ipv4Addr> = table.keys().chain(answered.iter()).copied().filter(|ip| net.hosts.contains(ip)).collect();
    ips.push(net.me);
    ips.sort();
    ips.dedup();
    let t = Instant::now();
    let ports = knock(&ips);
    debug(&format!("knock {} {:?}", ips.len(), t.elapsed()));
    let me_mac = my_mac(&net);
    let macs: HashSet<String> = table.values().cloned().chain(me_mac.clone()).collect();
    let makers = makers(&macs);
    let network = net.router.and_then(|r| table.get(&r).cloned());
    let before = history(network.as_deref());
    let now = chrono::Local::now();
    // The first scan's devices were already there.
    let start = before.as_ref().and_then(|b| b.iter().map(|s| s.first).min());
    let devices = ips
        .iter()
        .zip(ports)
        .map(|(ip, ports)| {
            let me = *ip == net.me;
            let mac = if me { me_mac.clone() } else { table.get(ip).cloned() };
            let first = mac.as_ref().and_then(|m| u8::from_str_radix(m.get(..2)?, 16).ok()).unwrap_or(0);
            let maker = mac.as_deref().and_then(prefix).and_then(|p| makers.get(&p).cloned());
            let name = names.get(ip).cloned().or_else(|| me.then(computer_name).flatten());
            let private_mac = first & 0x02 != 0;
            // Phones and laptops change their made-up hardware address now
            // and then; one with a name seen before isn't new.
            let seen = before.iter().flatten().find(|s| {
                mac.as_ref() == Some(&s.mac) || (private_mac && name.as_ref().is_some_and(|n| !n.is_empty() && *n == s.name))
            });
            let first_seen = seen.map_or(now, |s| s.first);
            let joined = before.is_some() && seen.is_none() && mac.is_some();
            Device {
                ip: *ip,
                name,
                first_seen,
                new: joined || (start.is_some_and(|s| first_seen > s) && now - first_seen < chrono::Duration::hours(24)),
                joined,
                maker,
                private_mac,
                ports,
                me,
                router: Some(*ip) == net.router,
                mac,
            }
        })
        .collect();
    let scan = Scan { at: now, range: net.range, devices, first: before.is_none(), network };
    remember(&scan);
    Ok(scan)
}

fn debug(s: &str) {
    if std::env::var_os("NET_WATCH_DEBUG").is_some() {
        eprintln!("{s}");
    }
}

/// `net-watch --scan`: scan once and print what's found.
pub fn print_once() {
    let start = Instant::now();
    match scan() {
        Ok(s) => {
            println!("{} devices on {} in {:.1} s", s.devices.len(), s.range, start.elapsed().as_secs_f32());
            for d in &s.devices {
                let name = d.name.as_deref().unwrap_or(if d.me { "(this computer)" } else { "" });
                let new = if d.joined { "JUST JOINED " } else if d.new { "new " } else { "" };
                println!("{:<16}{:<19}{:<24}{:<26}{new}{}", d.ip, d.mac.as_deref().unwrap_or(""), name, d.maker_text(), d.guess());
            }
        }
        Err(e) => println!("Couldn't scan: {e}"),
    }
}

fn computer_name() -> Option<String> {
    let name = std::fs::read_to_string("/etc/hostname").ok().or_else(|| std::env::var("COMPUTERNAME").ok())?;
    short(name.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makers_tidy() {
        assert_eq!(tidy_maker("Roku, Inc"), "Roku");
        assert_eq!(tidy_maker("Apple, Inc."), "Apple");
        assert_eq!(tidy_maker("SAMSUNG ELECTRO-MECHANICS(THAILAND)"), "Samsung Electro-Mechanics");
        assert_eq!(tidy_maker("Google Inc."), "Google");
    }

    #[test]
    fn names() {
        let q = name_question(7, Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(read_name(&q, 12).unwrap().0, "20.1.168.192.in-addr.arpa");
        assert_eq!(short("Billys-iPhone.attlocal.net").as_deref(), Some("Billys-iPhone"));
        assert_eq!(short("20.1.168.192.in-addr.arpa"), None);
    }
}
