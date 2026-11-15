//! The "What's wrong?" check: the connection tested in order, from this
//! computer outwards, stopping at the first step that fails, with what it
//! means and what to try. Reads Linux's own network files where it can and
//! runs `ping` for the rest; no admin rights needed.
use crate::probe;
use crate::store::Time;
use chrono::Local;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Pass,
    /// Works, but not well.
    Warn,
    Fail,
    /// Not tried: an earlier step failed, or it doesn't apply here.
    Skipped,
}

/// A button that helps with a fix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Help {
    /// The Network tool, for Wi-Fi and the connection's settings.
    Network,
    /// A web browser, for a sign-in page.
    SignIn,
}

#[derive(Clone, Debug)]
pub struct Step {
    pub name: &'static str,
    pub mark: Mark,
    /// What was found, in a sentence.
    pub says: String,
    /// What to try, when something's wrong.
    pub fix: Option<String>,
    pub help: Option<Help>,
    /// The numbers behind it, for "Show details".
    pub details: Vec<(&'static str, String)>,
}

impl Step {
    fn new(name: &'static str, mark: Mark, says: impl Into<String>) -> Self {
        Step { name, mark, says: says.into(), fix: None, help: None, details: Vec::new() }
    }

    fn fix(mut self, text: &str, help: Option<Help>) -> Self {
        self.fix = Some(text.into());
        self.help = help;
        self
    }

    fn detail(&mut self, label: &'static str, value: impl Into<String>) {
        self.details.push((label, value.into()));
    }
}

/// Every step, in order.
pub const STEPS: [&str; 7] = [
    "Connected to a network",
    "Address from the router",
    "Router answers",
    "Reaches the internet",
    "Names work (DNS)",
    "Websites load",
    "Connection quality",
];

#[derive(Clone, Debug)]
pub struct Report {
    pub at: Time,
    pub steps: Vec<Step>,
}

impl Report {
    pub fn done(&self) -> bool {
        self.steps.len() == STEPS.len()
    }

    /// The step that failed, else the first that works but not well.
    pub fn problem(&self) -> Option<&Step> {
        let first = |m| self.steps.iter().find(|s| s.mark == m);
        first(Mark::Fail).or_else(|| first(Mark::Warn))
    }

    /// The whole report as text, to paste into a message to a provider.
    pub fn text(&self) -> String {
        let mut out = format!("Net Watch connection check, {}\n", self.at.format("%Y-%m-%d %H:%M"));
        for s in &self.steps {
            let mark = match s.mark {
                Mark::Pass => "OK",
                Mark::Warn => "WARN",
                Mark::Fail => "FAIL",
                Mark::Skipped => "--",
            };
            out += &format!("\n[{mark}] {}: {}\n", s.name, s.says);
            if let Some(fix) = &s.fix {
                out += &format!("    Try: {fix}\n");
            }
            for (k, v) in &s.details {
                out += &format!("    {k}: {v}\n");
            }
        }
        out
    }
}

/// What the earlier steps found, for the later ones.
#[derive(Default)]
struct Found {
    iface: Option<String>,
    wifi: bool,
    router: Option<Ipv4Addr>,
    ipv6: bool,
    /// Percent of pings to the router that got lost.
    router_loss: Option<u32>,
    internet_ping: bool,
}

/// Run the check; `report` hears each step as it's done. Takes about 5 to
/// 10 seconds.
pub fn run(report: &dyn Fn(Step)) {
    let mut f = Found::default();
    let steps: [fn(&mut Found) -> Step; 7] = [link, address, router, internet, names, websites, quality];
    let mut stopped = false;
    for (i, step) in steps.iter().enumerate() {
        if stopped {
            report(Step::new(STEPS[i], Mark::Skipped, "Not checked: fix the step above first."));
            continue;
        }
        let s = step(&mut f);
        stopped = s.mark == Mark::Fail;
        report(s);
    }
}

// -- Reading Linux's files ---------------------------------------------------

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// A real adapter (not a VPN, bridge or container's).
fn physical(dev: &str) -> bool {
    Path::new("/sys/class/net").join(dev).join("device").exists()
}

fn is_wifi(dev: &str) -> bool {
    Path::new("/sys/class/net").join(dev).join("wireless").exists()
}

/// The adapters, by name.
fn devices() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("/sys/class/net")
        .map(|d| d.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// The default route: (adapter, router), the lowest metric winning; the
/// router is `None` when the route has none (a VPN or tether).
fn default_route() -> Option<(String, Option<Ipv4Addr>)> {
    let table = read("/proc/net/route")?;
    let hex = |s: &str| u32::from_str_radix(s, 16).ok().map(|v| Ipv4Addr::from(v.to_le_bytes()));
    table
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|f| f.len() > 7 && f[1] == "00000000" && f[7] == "00000000")
        .min_by_key(|f| f[6].parse::<u32>().unwrap_or(u32::MAX))
        .map(|f| (f[0].to_string(), hex(f[2]).filter(|r| !r.is_unspecified())))
}

/// Wi-Fi signal in dBm, from /proc/net/wireless.
fn signal(dev: &str) -> Option<i32> {
    let table = read("/proc/net/wireless")?;
    let line = table.lines().find(|l| l.trim_start().starts_with(&format!("{dev}:")))?;
    let level = line.split_whitespace().nth(3)?.trim_end_matches('.');
    level.parse::<f32>().ok().map(|v| v as i32).filter(|&v| v < 0)
}

/// Wi-Fi switched off by a key or airplane mode.
fn wifi_blocked() -> bool {
    std::fs::read_dir("/sys/class/rfkill").is_ok_and(|d| {
        d.filter_map(|e| e.ok()).any(|e| {
            let p = e.path();
            read(p.join("type")).as_deref() == Some("wlan") && (read(p.join("soft")).as_deref() == Some("1") || read(p.join("hard")).as_deref() == Some("1"))
        })
    })
}

/// The network's name, from NetworkManager.
fn wifi_name(dev: &str) -> Option<String> {
    let out = Command::new("nmcli").env("LC_ALL", "C").args(["-t", "-g", "GENERAL.CONNECTION", "device", "show", dev]).output().ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !name.is_empty()).then_some(name)
}

/// The router's hardware address, if it answered at the lowest level.
fn router_mac(router: Ipv4Addr) -> Option<String> {
    let table = read("/proc/net/arp")?;
    table.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        let answered = u32::from_str_radix(f.get(2)?.trim_start_matches("0x"), 16).ok()? & 2 != 0;
        (answered && f[0].parse::<Ipv4Addr>().ok()? == router).then(|| f[3].to_string())
    })
}

/// This computer's address on the way to `to`.
fn my_address(to: Ipv4Addr) -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect((to, 9)).ok()?;
    match s.local_addr().ok()? {
        SocketAddr::V4(a) => Some(*a.ip()),
        _ => None,
    }
}

/// A public IPv6 address on `dev`.
fn ipv6_address(dev: &str) -> Option<Ipv6Addr> {
    let table = read("/proc/net/if_inet6")?;
    table.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        // Scope 00 is global; flag 0x20 is "still being checked".
        (f.len() == 6 && f[5] == dev && f[3] == "00").then(|| u128::from_str_radix(f[0], 16).ok().map(Ipv6Addr::from))?
    })
}

/// The name servers this computer asks.
fn name_servers() -> Vec<String> {
    let mut v: Vec<String> = read("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver").map(|s| s.trim().to_string()))
        .collect();
    // 127.0.0.53 is systemd-resolved passing questions on; ask it where to.
    if v.iter().any(|s| s == "127.0.0.53")
        && let Ok(out) = Command::new("resolvectl").env("LC_ALL", "C").arg("dns").output()
    {
        let up: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once(':').map(|(_, s)| s.trim()))
            .flat_map(|s| s.split_whitespace().map(str::to_string))
            .collect();
        if !up.is_empty() {
            v = up;
        }
    }
    v.dedup();
    v
}

// -- Ping --------------------------------------------------------------------

struct Ping {
    sent: u32,
    got: u32,
    avg_ms: Option<f32>,
    max_ms: Option<f32>,
}

impl Ping {
    fn loss(&self) -> u32 {
        if self.sent == 0 { 100 } else { (self.sent - self.got) * 100 / self.sent }
    }
}

/// `count` pings to `ip`, 0.2 s apart (the quickest allowed without admin
/// rights); `None` when `ping` couldn't run.
fn ping(ip: IpAddr, count: u32, extra: &[&str]) -> Option<Ping> {
    let out = Command::new("ping")
        .env("LC_ALL", "C")
        .args(["-n", "-q", "-i", "0.2", "-W", "1", "-w", "4", "-c"])
        .arg(count.to_string())
        .args(extra)
        .arg(ip.to_string())
        .output()
        .ok()?;
    parse_ping(&String::from_utf8_lossy(&out.stdout))
}

fn parse_ping(text: &str) -> Option<Ping> {
    // "5 packets transmitted, 5 received, 0% packet loss, time 803ms"
    let counts = text.lines().find(|l| l.contains("packets transmitted"))?;
    let mut n = counts.split(',').filter_map(|p| p.split_whitespace().next()?.parse::<u32>().ok());
    let (sent, got) = (n.next()?, n.next()?);
    // "rtt min/avg/max/mdev = 16.7/17.4/18.2/0.7 ms"
    let rtt: Vec<f32> = text
        .lines()
        .find_map(|l| l.split_once(" = ").filter(|(k, _)| k.contains("min/avg/max")).map(|(_, v)| v))
        .map(|v| v.trim_end_matches(" ms").split('/').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_default();
    Some(Ping { sent, got, avg_ms: rtt.get(1).copied(), max_ms: rtt.get(2).copied() })
}

fn ms(v: f32) -> String {
    if v < 10.0 { format!("{v:.1} ms") } else { format!("{v:.0} ms") }
}

// -- The steps ---------------------------------------------------------------

fn link(f: &mut Found) -> Step {
    let name = STEPS[0];
    let route = default_route();
    let devs: Vec<String> = devices().into_iter().filter(|d| physical(d)).collect();
    let up = |d: &String| read(format!("/sys/class/net/{d}/operstate")).as_deref() == Some("up");
    // The adapter the internet goes through, else any that's connected.
    let dev = route.as_ref().map(|r| r.0.clone()).filter(|d| physical(d) && up(d)).or_else(|| devs.iter().find(|d| up(d)).cloned());
    let Some(dev) = dev else {
        let mut s = if devs.is_empty() {
            Step::new(name, Mark::Fail, "This computer has no network adapter that Linux can use.")
                .fix("Check the adapter is plugged in (for a USB one) and that its driver is installed.", None)
        } else if wifi_blocked() && devs.iter().any(|d| is_wifi(d)) {
            Step::new(name, Mark::Fail, "Wi-Fi is switched off.")
                .fix("Turn Wi-Fi on: check airplane mode, or a Wi-Fi key or switch on the laptop.", Some(Help::Network))
        } else {
            let wifi = devs.iter().any(|d| is_wifi(d));
            let says = if wifi { "Not on Wi-Fi, and no cable is plugged in." } else { "No network cable is plugged in." };
            Step::new(name, Mark::Fail, says).fix(
                if wifi { "Join a Wi-Fi network, or plug in a network cable." } else { "Plug the cable in firmly at both ends, or try another cable or router port." },
                Some(Help::Network),
            )
        };
        s.detail("Adapters", if devs.is_empty() { "none".into() } else { devs.join(", ") });
        return s;
    };
    f.wifi = is_wifi(&dev);
    let mut s = if f.wifi {
        let net = wifi_name(&dev).map_or("Wi-Fi".into(), |n| format!("Wi-Fi \u{201c}{n}\u{201d}"));
        match signal(&dev) {
            Some(dbm) if dbm < -75 => Step::new(name, Mark::Warn, format!("On {net}, but the signal is weak."))
                .fix("Move closer to the router or away from walls, or try a cable. A weak signal makes the internet slow and drop out.", None),
            Some(dbm) if dbm < -67 => Step::new(name, Mark::Pass, format!("On {net}; the signal is fair.")),
            _ => Step::new(name, Mark::Pass, format!("On {net} with a good signal.")),
        }
    } else {
        let speed = read(format!("/sys/class/net/{dev}/speed")).and_then(|s| s.parse::<i32>().ok()).filter(|&s| s > 0);
        match speed {
            Some(mbps) if mbps < 1000 => Step::new(name, Mark::Warn, format!("On a cable, but it only connects at {mbps} Mbps."))
                .fix("A cable or port that's worn or the wrong kind holds it back: try another cable (Cat 5e or better) or router port.", None),
            _ => Step::new(name, Mark::Pass, "On a network cable."),
        }
    };
    s.detail("Adapter", dev.clone());
    if let Some(dbm) = f.wifi.then(|| signal(&dev)).flatten() {
        s.detail("Signal", format!("{dbm} dBm (above -67 is good, below -75 is weak)"));
    }
    if let Some(mbps) = read(format!("/sys/class/net/{dev}/speed")).filter(|_| !f.wifi).filter(|s| !s.starts_with('-')) {
        s.detail("Link speed", format!("{mbps} Mbps"));
    }
    if let Some(mtu) = read(format!("/sys/class/net/{dev}/mtu")) {
        s.detail("Packet size (MTU)", mtu);
    }
    if let Some(mac) = read(format!("/sys/class/net/{dev}/address")) {
        s.detail("Hardware address", mac);
    }
    f.iface = Some(dev);
    s
}

fn address(f: &mut Found) -> Step {
    let name = STEPS[1];
    let fix = "Turn Wi-Fi off and on again, or unplug the cable for 10 seconds. If it keeps happening, restart the router.";
    let Some((dev, router)) = default_route() else {
        return Step::new(name, Mark::Fail, "The network didn't give this computer a way out to the internet.").fix(fix, Some(Help::Network));
    };
    let me = my_address(router.unwrap_or(Ipv4Addr::new(1, 1, 1, 1)));
    let mut s = match me {
        Some(ip) if ip.is_link_local() => Step::new(name, Mark::Fail, "The router didn't hand this computer an address, so it made one up.")
            .fix(fix, Some(Help::Network)),
        None => Step::new(name, Mark::Fail, "This computer has no address on the network.").fix(fix, Some(Help::Network)),
        Some(ip) => match router {
            Some(r) => Step::new(name, Mark::Pass, format!("Got the address {ip} from the router at {r}.")),
            None => Step::new(name, Mark::Pass, format!("Has the address {ip}; the internet goes through {dev}, likely a VPN or a phone.")),
        },
    };
    if let Some(ip) = me {
        s.detail("This computer", ip.to_string());
    }
    s.detail("Router", router.map_or("none on this route".into(), |r| r.to_string()));
    s.detail("Internet goes through", if physical(&dev) { dev.clone() } else { format!("{dev} (a VPN or tunnel)") });
    match f.iface.as_deref().and_then(ipv6_address).or_else(|| ipv6_address(&dev)) {
        Some(v6) => {
            f.ipv6 = true;
            s.detail("IPv6 address", v6.to_string());
        }
        None => s.detail("IPv6 address", "none (that's fine)"),
    }
    f.router = router;
    s
}

fn router(f: &mut Found) -> Step {
    let name = STEPS[2];
    let Some(r) = f.router else {
        return Step::new(name, Mark::Skipped, "No router on this connection to check (a VPN or a phone's hotspot).");
    };
    let p = ping(IpAddr::V4(r), 10, &[]);
    let knocked = p.as_ref().is_none_or(|p| p.got == 0) && probe::router(r.octets());
    let mut s = match &p {
        Some(p) if p.got > 0 && p.loss() >= 10 => Step::new(name, Mark::Warn, format!("The router answers, but {}% of messages to it got lost.", p.loss())).fix(
            if f.wifi {
                "Messages are lost between this computer and the router, which usually means weak or crowded Wi-Fi: move closer, or try a cable or the 5 GHz network."
            } else {
                "Messages are lost on the cable to the router: try another cable or router port, or restart the router."
            },
            None,
        ),
        Some(p) if p.avg_ms.is_some_and(|a| a > 60.0) => Step::new(name, Mark::Warn, "The router answers, but slowly.").fix(
            "A busy or weak Wi-Fi, or a router that's overloaded: move closer, or restart the router.",
            None,
        ),
        Some(p) if p.got > 0 => Step::new(name, Mark::Pass, format!("The router answers in {}.", p.avg_ms.map_or("good time".into(), ms))),
        _ if knocked => Step::new(name, Mark::Pass, "The router answers."),
        _ => Step::new(name, Mark::Fail, "The router isn't answering.").fix(
            "Restart the router: unplug it for 30 seconds, then wait 2 minutes for it to start. On Wi-Fi, check you're on your own network.",
            Some(Help::Network),
        ),
    };
    s.detail("Router", r.to_string());
    if let Some(mac) = router_mac(r) {
        s.detail("Router's hardware address", mac);
    }
    match &p {
        Some(p) => {
            s.detail("Lost", format!("{} of {}", p.sent - p.got, p.sent));
            if let (Some(a), Some(m)) = (p.avg_ms, p.max_ms) {
                s.detail("Time", format!("{} on average, {} at worst", ms(a), ms(m)));
            }
            f.router_loss = Some(p.loss());
        }
        None => s.detail("Ping", "couldn't run `ping`; knocked on ports 53, 80 and 443 instead"),
    }
    s
}

fn internet(f: &mut Found) -> Step {
    let name = STEPS[3];
    let (p, connects) = std::thread::scope(|sc| {
        let p = sc.spawn(|| ping(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 10, &[]));
        let c: Vec<_> = probe::HOSTS.map(|(n, ip)| (n, sc.spawn(move || probe::tcp(ip, 443)))).into();
        let c: Vec<(&str, Result<Duration, String>)> = c.into_iter().map(|(n, j)| (n, j.join().unwrap_or_else(|_| Err("crashed".into())))).collect();
        (p.join().ok().flatten(), c)
    });
    let reached = connects.iter().filter(|c| c.1.is_ok()).count();
    let fastest = connects.iter().filter_map(|c| c.1.as_ref().ok()).min().copied();
    f.internet_ping = p.as_ref().is_some_and(|p| p.got > 0);
    let past_router = f.router.is_some() && f.router_loss.is_some_and(|l| l < 10);
    let mut s = match &p {
        _ if reached == 0 && !f.internet_ping => Step::new(
            name,
            Mark::Fail,
            if f.router.is_some() { "Your router is fine, but nothing gets past it to the internet." } else { "Nothing gets out to the internet." },
        )
        .fix(
            "The problem is between the router and your provider. Restart the modem and router (unplug for 30 seconds). \
             If it's still down after 5 minutes, your provider may have an outage: call them.",
            None,
        ),
        Some(p) if p.got > 0 && p.loss() >= 5 => {
            let fix = if past_router {
                "The loss starts past your router, on your provider's line. If it keeps up, tell your provider; \"Copy report\" gives them the numbers."
            } else {
                "Some of it is lost before the router already; fix the step above first."
            };
            Step::new(name, Mark::Warn, format!("Reaches the internet, but {}% of messages got lost on the way.", p.loss())).fix(fix, None)
        }
        Some(p) if p.avg_ms.is_some_and(|a| a > 150.0) => Step::new(name, Mark::Warn, "Reaches the internet, but slowly.")
            .fix("Something on your network may be using it all (a download or backup), or your provider is busy. Try again later, or run a speed test.", None),
        _ => Step::new(
            name,
            Mark::Pass,
            format!("Reaches the internet in {}.", p.as_ref().and_then(|p| p.avg_ms).or(fastest.map(|d| d.as_secs_f32() * 1000.0)).map_or("good time".into(), ms)),
        ),
    };
    match &p {
        Some(p) => {
            s.detail("Lost (to 1.1.1.1)", format!("{} of {}", p.sent - p.got, p.sent));
            if let (Some(a), Some(m)) = (p.avg_ms, p.max_ms) {
                s.detail("Time (to 1.1.1.1)", format!("{} on average, {} at worst", ms(a), ms(m)));
            }
        }
        None => s.detail("Ping", "couldn't run `ping`"),
    }
    for (n, r) in &connects {
        s.detail("Connects to", format!("{n}: {}", r.as_ref().map_or_else(|e| e.clone(), |d| ms(d.as_secs_f32() * 1000.0))));
    }
    s
}

/// The system's own lookup, which can hang; given up on after 5 seconds.
fn system_lookup(host: &'static str) -> Result<Duration, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let start = Instant::now();
        let r = (host, 443).to_socket_addrs().map(|mut a| a.next().is_some());
        let _ = tx.send(match r {
            Ok(true) => Ok(start.elapsed()),
            Ok(false) => Err("no addresses".into()),
            Err(e) => Err(e.to_string()),
        });
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap_or(Err("no answer".into()))
}

fn names(_: &mut Found) -> Step {
    let name = STEPS[4];
    let (own, public) = std::thread::scope(|sc| {
        let public = sc.spawn(|| probe::dns([1, 1, 1, 1]));
        (system_lookup("example.com"), public.join().unwrap_or(Err("crashed".into())))
    });
    let mut s = match (&own, &public) {
        (Ok(d), _) if d.as_millis() > 500 => Step::new(name, Mark::Warn, format!("Names work, but slowly ({}).", ms(d.as_secs_f32() * 1000.0)))
            .fix("Every website waits for this. Switching this connection's DNS to 1.1.1.1 and 8.8.8.8 in Network may help.", Some(Help::Network)),
        (Ok(d), _) => Step::new(name, Mark::Pass, format!("Names work ({}).", ms(d.as_secs_f32() * 1000.0))),
        (Err(_), Ok(_)) => Step::new(name, Mark::Fail, "Your DNS server isn't answering, though others do, so websites won't open even though the internet is up.")
            .fix("Switch this connection's DNS to 1.1.1.1 and 8.8.8.8 in Network, or restart the router.", Some(Help::Network)),
        (Err(_), Err(_)) => Step::new(name, Mark::Fail, "Questions about names aren't getting answers.").fix(
            "Something blocks them: a VPN, a firewall, or the network itself (some work and school networks). Try the Firewall check, or another network.",
            None,
        ),
    };
    let servers = name_servers();
    s.detail("This computer asks", if servers.is_empty() { "nobody (none set)".into() } else { servers.join(", ") });
    s.detail("Looking up example.com", own.map_or_else(|e| e, |d| ms(d.as_secs_f32() * 1000.0)));
    s.detail("Asking 1.1.1.1 directly", public.map_or_else(|e| e, |d| ms(d.as_secs_f32() * 1000.0)));
    s
}

fn websites(_: &mut Found) -> Step {
    let name = STEPS[5];
    let (plain, secure) = std::thread::scope(|sc| {
        let secure = sc.spawn(|| {
            let start = Instant::now();
            match probe::agent(Duration::from_secs(5)).get("https://www.google.com/generate_204").call() {
                Ok(r) if r.status().as_u16() == 204 => Ok(start.elapsed()),
                Ok(r) => Err(format!("got {}", r.status().as_u16())),
                Err(e) => Err(e.to_string()),
            }
        });
        (probe::page_204(), secure.join().unwrap_or(Err("crashed".into())))
    });
    let mut s = match (&plain, &secure) {
        (Ok(_), Ok(d)) => Step::new(name, Mark::Pass, format!("Websites load, secure ones too ({}).", ms(d.as_secs_f32() * 1000.0))),
        (Err(e), _) if e.starts_with("got ") => Step::new(name, Mark::Fail, "A sign-in page is in the way, like at a hotel, café or airport.")
            .fix("Open a web browser and sign in or accept the terms, then check again.", Some(Help::SignIn)),
        (_, Err(e)) if e.contains("ertificate") || e.contains("expired") || e.contains("NotValidYet") => {
            Step::new(name, Mark::Fail, "Secure websites are refused: their certificates don't check out.").fix(
                "Check this computer's date and time are right (a wrong clock does this). Otherwise something on the network is listening in.",
                None,
            )
        }
        (Ok(_), Err(_)) => Step::new(name, Mark::Fail, "Plain websites load, but secure ones don't.")
            .fix("Something blocks secure connections: a firewall, VPN, or parental controls on the router.", None),
        _ => Step::new(name, Mark::Fail, "Websites don't load.").fix("Something between here and the internet stops them: try the Firewall check.", None),
    };
    s.detail("Plain page (cp.cloudflare.com)", plain.map_or_else(|e| e, |d| ms(d.as_secs_f32() * 1000.0)));
    s.detail("Secure page (google.com)", secure.map_or_else(|e| e, |d| ms(d.as_secs_f32() * 1000.0)));
    s.detail("This computer's clock", Local::now().format("%Y-%m-%d %H:%M").to_string());
    s
}

fn quality(f: &mut Found) -> Step {
    let name = STEPS[6];
    let v6 = f.ipv6.then(|| TcpStream::connect_timeout(&SocketAddr::from((Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111), 443)), Duration::from_secs(3)));
    // A full-size packet that mustn't be split: 1472 bytes of data plus 28
    // of headers is the usual 1500.
    let big = f.internet_ping.then(|| ping(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 2, &["-M", "do", "-s", "1472"])).flatten();
    let v6_broken = matches!(v6, Some(Err(_)));
    let big_lost = big.as_ref().is_some_and(|p| p.got == 0);
    let mut s = match (v6_broken, big_lost) {
        (true, _) => Step::new(name, Mark::Warn, "IPv6 is set up but doesn't work, which can make some websites slow to open.")
            .fix("Restart the router. If it stays, turn IPv6 off for this connection, or ask your provider.", Some(Help::Network)),
        (_, true) => Step::new(name, Mark::Warn, "Full-size packets don't get through, which can make some websites or VPNs hang.")
            .fix("Lower this connection's packet size (MTU) to 1400. A VPN or some DSL lines need it.", Some(Help::Network)),
        _ => Step::new(name, Mark::Pass, "Nothing else looks wrong."),
    };
    s.detail(
        "IPv6",
        match &v6 {
            None => "not used on this network".into(),
            Some(Ok(_)) => "works".into(),
            Some(Err(e)) => format!("doesn't work ({e})"),
        },
    );
    s.detail(
        "Full-size packets",
        match &big {
            None => "not checked",
            Some(p) if p.got > 0 => "get through",
            Some(_) => "don't get through",
        },
    );
    s
}

/// `net-watch --diagnose`: run the check and print it.
pub fn print_once() {
    let at = Local::now();
    let steps = std::cell::RefCell::new(Vec::new());
    run(&|s| {
        println!("{:<26} {:?}: {}", s.name, s.mark, s.says);
        steps.borrow_mut().push(s);
    });
    println!("\n{}", Report { at, steps: steps.into_inner() }.text());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ping() {
        let text = "PING 1.1.1.1 (1.1.1.1) 56(84) bytes of data.\n\n--- 1.1.1.1 ping statistics ---\n\
                    10 packets transmitted, 8 received, 20% packet loss, time 1803ms\nrtt min/avg/max/mdev = 16.754/17.481/18.209/0.727 ms\n";
        let p = parse_ping(text).unwrap();
        assert_eq!((p.sent, p.got, p.loss()), (10, 8, 20));
        assert_eq!((p.avg_ms, p.max_ms), (Some(17.481), Some(18.209)));
        // Nothing back: no times line.
        let p = parse_ping("3 packets transmitted, 0 received, 100% packet loss, time 2050ms\n").unwrap();
        assert_eq!((p.loss(), p.avg_ms), (100, None));
        // With errors: "+2 errors" sits between the counts and the loss.
        let p = parse_ping("2 packets transmitted, 0 received, +2 errors, 100% packet loss, time 1001ms\n").unwrap();
        assert_eq!(p.loss(), 100);
    }

    #[test]
    fn stops_at_the_first_failure() {
        let r = Report {
            at: Local::now(),
            steps: vec![Step::new(STEPS[0], Mark::Warn, "weak"), Step::new(STEPS[1], Mark::Fail, "none"), Step::new(STEPS[2], Mark::Skipped, "")],
        };
        assert_eq!(r.problem().map(|s| s.name), Some(STEPS[1]));
        assert!(r.text().contains("[FAIL] Address from the router: none"));
    }
}
