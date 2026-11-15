//! "Where's the lag?": the hops between this computer and a site, how long
//! each takes to answer and how much gets lost, and whose part of the way
//! the delay is in: your home network, your provider or beyond. Uses only
//! `ping` (hop by hop, like traceroute), so no admin rights are needed.
use crate::diag::{self, ms};
use crate::store::Time;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::process::Command;
use std::time::Duration;

/// The most hops followed.
const MAX_HOPS: u8 = 30;

/// Whose part of the way a hop is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Who {
    Home,
    Provider,
    Beyond,
    Destination,
}

impl Who {
    pub fn word(self) -> &'static str {
        match self {
            Who::Home => "Your home",
            Who::Provider => "Your provider",
            Who::Beyond => "Beyond",
            Who::Destination => "The site",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hop {
    /// How many steps out it is, from 1.
    pub n: u8,
    /// `None`: nothing there answered.
    pub ip: Option<Ipv4Addr>,
    /// Its name, like "lag-1.sbcglobal.net", when it has one.
    pub name: Option<String>,
    /// The number of the network it's in, which says whose it is.
    pub asn: Option<u32>,
    pub who: Who,
    /// Pings sent and answered once measured; `None` before.
    pub sent: Option<u32>,
    pub got: u32,
    pub avg_ms: Option<f32>,
    pub max_ms: Option<f32>,
}

impl Hop {
    /// Percent of pings lost; `None` before it's measured, or when it
    /// never answers pings at all (many routers don't).
    pub fn loss(&self) -> Option<u32> {
        let sent = self.sent.filter(|&s| s > 0 && self.got > 0)?;
        Some((sent - self.got) * 100 / sent)
    }

    /// Its name if it has one, else its address.
    pub fn label(&self) -> String {
        match (&self.name, self.ip) {
            (Some(n), _) => n.clone(),
            (None, Some(ip)) => ip.to_string(),
            (None, None) => "(no answer)".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Trace {
    pub at: Time,
    /// What was asked for, like "Cloudflare" or "example.com".
    pub target: String,
    pub ip: Option<Ipv4Addr>,
    /// Who the provider is, like "AT&T Enterprises".
    pub provider: Option<String>,
    pub hops: Vec<Hop>,
    /// Every hop has been measured.
    pub done: bool,
    /// Why it couldn't run at all.
    pub error: Option<String>,
}

/// Where the time goes, in ms: (home, provider, beyond).
pub type Split = (f32, f32, f32);

impl Trace {
    /// The hop that's the site itself, when the trace got there.
    pub fn arrived(&self) -> Option<&Hop> {
        self.hops.last().filter(|h| h.who == Who::Destination)
    }

    /// How the time to the site splits between home, provider and beyond:
    /// each part is the time to its last hop less the time before it.
    pub fn split(&self) -> Option<Split> {
        let end = self.arrived().or_else(|| self.hops.iter().rev().find(|h| h.avg_ms.is_some()))?.avg_ms?;
        let last = |who: Who| self.hops.iter().rev().find(|h| h.who == who && h.avg_ms.is_some()).and_then(|h| h.avg_ms);
        let home = last(Who::Home).unwrap_or(0.0).min(end);
        let provider = last(Who::Provider).unwrap_or(home).clamp(home, end);
        Some((home, provider - home, end - provider))
    }

    /// The first hop from which messages keep getting lost all the way to
    /// the end. Routers often don't bother answering pings when busy, so
    /// loss at one hop that doesn't carry on to the next ones isn't real.
    pub fn loss_from(&self) -> Option<&Hop> {
        let measured: Vec<&Hop> = self.hops.iter().filter(|h| h.loss().is_some() && h.ip.is_some()).collect();
        let end = measured.last()?;
        if end.loss()? < 5 {
            return None;
        }
        let mut from = measured.len() - 1;
        while from > 0 && measured[from - 1].loss().is_some_and(|l| l >= 5) {
            from -= 1;
        }
        Some(measured[from])
    }

    /// The answer in plain words: a headline, and what it means or what to
    /// try. The bool is whether it's a problem.
    pub fn verdict(&self) -> (String, String, bool) {
        if let Some(e) = &self.error {
            return ("Couldn't trace the route".into(), e.clone(), true);
        }
        if let Some(h) = self.loss_from() {
            let (lost, what) = (h.loss().unwrap_or(0), self.hops.last().and_then(Hop::loss).unwrap_or(0));
            let fix = match h.who {
                Who::Home => "That's on your own network: weak or crowded Wi-Fi, or a bad cable. Move closer to the router or try a cable.",
                Who::Provider => "That's on your provider's network. If it keeps up, tell them: \"Copy report\" gives them the hops.",
                _ => "That's far away, past your provider, so it isn't something you can fix; it may only affect this site.",
            };
            return (format!("Messages get lost from hop {} ({}): {what}% by the end", h.n, h.who.word().to_lowercase()), format!("{lost}% lost there. {fix}"), true);
        }
        let Some((home, provider, beyond)) = self.split() else {
            return ("Nothing answered on the way".into(), "Some networks don't answer these messages. Try another site.".into(), true);
        };
        let total = home + provider + beyond;
        let arrived = self.arrived().is_some();
        let lead = if arrived { format!("{} to {}", ms(total), self.target) } else { format!("{} as far as it got", ms(total)) };
        if total < 80.0 && home < 20.0 {
            let tail = if arrived { "No lag to speak of." } else { "The site doesn't answer these messages itself, but the way there is quick." };
            return (format!("{lead}. {tail}"), parts(home, provider, beyond), false);
        }
        let (biggest, fix) = if home >= provider && home >= beyond {
            ("your home network", "Wi-Fi or the router is slowing things down: move closer to the router, try a cable, or restart the router.")
        } else if provider >= beyond {
            ("your provider's network", "If it's always like this, tell your provider; \"Copy report\" gives them the hops.")
        } else {
            ("the internet beyond your provider", "That's the distance to the site, or the site itself; nothing at your end will fix it.")
        };
        (format!("{lead}. Most of the wait is in {biggest}."), format!("{} {fix}", parts(home, provider, beyond)), true)
    }

    /// The trace as text, to paste into a message to a provider.
    pub fn text(&self) -> String {
        let (head, detail, _) = self.verdict();
        let provider = self.provider.as_ref().map_or(String::new(), |p| format!("Provider: {p}\n"));
        let mut out = format!("Net Watch route to {} ({}), {}\n{provider}{head}\n{detail}\n\n", self.target, self.ip.map_or("?".into(), |ip| ip.to_string()), self.at.format("%Y-%m-%d %H:%M"));
        for h in &self.hops {
            let time = match (h.avg_ms, h.max_ms) {
                (Some(a), Some(m)) => format!("{} avg, {} worst", ms(a), ms(m)),
                _ => "-".into(),
            };
            let loss = match (h.sent, h.loss()) {
                (_, Some(l)) => format!("{l}% lost"),
                (Some(_), None) => "no reply".into(),
                (None, None) => "-".into(),
            };
            let ip = h.ip.map_or("*".into(), |ip| ip.to_string());
            out += &format!("{:>2}  {:<15}  {:<13}  {:<22}  {:<9}  {:<8}  {}\n", h.n, ip, h.who.word(), time, loss, h.asn.map_or(String::new(), |a| format!("AS{a}")), h.name.as_deref().unwrap_or(""));
        }
        out
    }
}

fn parts(home: f32, provider: f32, beyond: f32) -> String {
    format!("Your home network {} · your provider {} · beyond {}.", ms(home), ms(provider), ms(beyond))
}

/// The host in what someone typed: "https://Example.com/page" gives
/// "example.com". Only letters, digits, dots and dashes get through.
pub fn host_of(input: &str) -> Result<String, String> {
    let s = input.trim();
    let s = s.split_once("://").map_or(s, |(_, rest)| rest);
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let s = s.rsplit_once('@').map_or(s, |(_, h)| h);
    let host = s.split(':').next().unwrap_or("").trim_end_matches('.').to_ascii_lowercase();
    if host.parse::<Ipv4Addr>().is_ok() {
        return Ok(host);
    }
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    if ok { Ok(host) } else { Err("That doesn't look like a website. Try something like example.com.".into()) }
}

/// The site's IPv4 address, given up on after 5 seconds.
fn resolve(host: &str) -> Result<Ipv4Addr, String> {
    if let Ok(ip) = host.parse() {
        return Ok(ip);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let h = host.to_string();
    std::thread::spawn(move || {
        let found = (h.as_str(), 443).to_socket_addrs().map(|a| a.filter_map(|a| if let SocketAddr::V4(v) = a { Some(*v.ip()) } else { None }).next());
        let _ = tx.send(found);
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(Some(ip))) => Ok(ip),
        Ok(Ok(None)) => Err(format!("{host} has no IPv4 address to trace.")),
        Ok(Err(_)) => Err(format!("Couldn't find {host}. Check the spelling, or run \"What's wrong?\".")),
        Err(_) => Err(format!("Looking up {host} took too long. Run \"What's wrong?\" to see why.")),
    }
}

/// What came back from a ping that may only get `ttl` hops out.
#[derive(Debug, PartialEq)]
enum Reply {
    /// A router on the way said it had to stop there.
    Hop(Ipv4Addr),
    /// The site itself answered.
    Arrived,
    Nothing,
}

fn parse_reply(text: &str, to: Ipv4Addr) -> Reply {
    for line in text.lines() {
        // "From 192.168.1.254 icmp_seq=1 Time to live exceeded"
        if line.contains("Time to live exceeded")
            && let Some(ip) = line.strip_prefix("From ").and_then(|r| r.split_whitespace().next()).and_then(|s| s.parse().ok())
        {
            return Reply::Hop(ip);
        }
        // "64 bytes from 1.1.1.1: icmp_seq=1 ttl=58 time=17.2 ms"
        if line.contains(&format!("bytes from {to}:")) {
            return Reply::Arrived;
        }
    }
    Reply::Nothing
}

fn probe(to: Ipv4Addr, ttl: u8) -> Reply {
    let out = Command::new("ping")
        .env("LC_ALL", "C")
        .args(["-n", "-c", "1", "-W", "1", "-t"])
        .arg(ttl.to_string())
        .arg(to.to_string())
        .output();
    out.map_or(Reply::Nothing, |o| parse_reply(&String::from_utf8_lossy(&o.stdout), to))
}

/// The hops out to `to`, ten at a time, each tried twice if it stays quiet.
fn find_hops(to: Ipv4Addr) -> Vec<Option<Ipv4Addr>> {
    let mut hops = Vec::new();
    for first in (1..=MAX_HOPS).step_by(10) {
        let ttls: Vec<u8> = (first..(first + 10).min(MAX_HOPS + 1)).collect();
        let replies: Vec<Reply> = std::thread::scope(|s| {
            let jobs: Vec<_> = ttls
                .iter()
                .map(|&t| {
                    s.spawn(move || match probe(to, t) {
                        Reply::Nothing => probe(to, t),
                        r => r,
                    })
                })
                .collect();
            jobs.into_iter().map(|j| j.join().unwrap_or(Reply::Nothing)).collect()
        });
        for r in replies {
            match r {
                Reply::Hop(ip) if ip == to => {
                    hops.push(Some(to));
                    return hops;
                }
                Reply::Hop(ip) => hops.push(Some(ip)),
                Reply::Arrived => {
                    hops.push(Some(to));
                    return hops;
                }
                Reply::Nothing => hops.push(None),
            }
        }
        // Ten quiet hops in a row: it won't get further.
        if hops.len() >= 10 && hops[hops.len() - 10..].iter().all(Option::is_none) {
            break;
        }
    }
    while hops.last() == Some(&None) {
        hops.pop();
    }
    hops
}

/// This computer's own name server, when it's an IPv4 one.
fn name_server() -> Option<Ipv4Addr> {
    std::fs::read_to_string("/etc/resolv.conf")
        .ok()?
        .lines()
        .find_map(|l| l.trim().strip_prefix("nameserver").and_then(|s| s.trim().parse().ok()))
}

/// Ask the name server every question at once (each with its index as its
/// ID) and read each answer with `read`; 1.5 seconds for them all.
fn ask(questions: &[Vec<u8>], read: fn(&[u8]) -> Option<String>) -> Vec<Option<String>> {
    let mut found = vec![None; questions.len()];
    let (Some(server), Ok(sock)) = (name_server(), UdpSocket::bind("0.0.0.0:0")) else { return found };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
    for q in questions {
        let _ = sock.send_to(q, (server, 53));
    }
    let until = std::time::Instant::now() + Duration::from_millis(1500);
    let mut buf = [0u8; 1500];
    while std::time::Instant::now() < until && found.iter().any(Option::is_none) {
        let Ok((len, from)) = sock.recv_from(&mut buf) else { continue };
        if from.ip() != IpAddr::V4(server) || len < 12 {
            continue;
        }
        let id = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        if let Some(slot) = found.get_mut(id) {
            *slot = read(&buf[..len]);
        }
    }
    found
}

/// A DNS question for `name`'s text record.
fn txt_question(id: u16, name: &str) -> Vec<u8> {
    let mut q = vec![(id >> 8) as u8, id as u8, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 16, 0, 1]);
    q
}

/// The first text in a DNS answer to `txt_question`.
fn txt_answer(b: &[u8]) -> Option<String> {
    if b.len() < 12 || b[2] & 0x80 == 0 {
        return None;
    }
    let count = |at: usize| u16::from_be_bytes([b[at], b[at + 1]]) as usize;
    let mut i = 12;
    for _ in 0..count(4) {
        i = crate::devices::read_name(b, i)?.1 + 4;
    }
    for _ in 0..count(6) {
        let (_, j) = crate::devices::read_name(b, i)?;
        let kind = u16::from_be_bytes([*b.get(j)?, *b.get(j + 1)?]);
        let len = u16::from_be_bytes([*b.get(j + 8)?, *b.get(j + 9)?]) as usize;
        if kind == 16 {
            let n = *b.get(j + 10)? as usize;
            return b.get(j + 11..j + 11 + n).map(|t| String::from_utf8_lossy(t).into_owned());
        }
        i = j + 10 + len;
    }
    None
}

/// The network number in Team Cymru's answer: "7018 | 99.56.0.0/13 | US …".
fn asn_of(text: &str) -> Option<u32> {
    text.split('|').next()?.split_whitespace().next()?.parse().ok()
}

/// The owner in its answer about a network: "… | ATT-INTERNET4 - AT&T
/// Enterprises, LLC, US" gives "AT&T Enterprises".
fn owner_of(text: &str) -> Option<String> {
    let owner = text.rsplit('|').next()?.trim();
    let name = owner.split_once(" - ").map_or(owner, |(_, n)| n);
    let name = name.split(',').next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Each hop's name, and which network it's in (from Team Cymru's public
/// lookup, asked through this computer's own name server).
fn whose(ips: &[Ipv4Addr]) -> (Vec<Option<String>>, Vec<Option<u32>>) {
    let ptr: Vec<Vec<u8>> = ips.iter().enumerate().map(|(i, ip)| crate::devices::name_question(i as u16, *ip)).collect();
    let origin: Vec<Vec<u8>> = ips
        .iter()
        .enumerate()
        .map(|(i, ip)| {
            let o = ip.octets();
            txt_question(i as u16, &format!("{}.{}.{}.{}.origin.asn.cymru.com", o[3], o[2], o[1], o[0]))
        })
        .collect();
    let public: Vec<bool> = ips.iter().map(|ip| !private(*ip) && !(ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64)).collect();
    std::thread::scope(|s| {
        let names = s.spawn(|| ask(&ptr, crate::devices::name_answer));
        let asns: Vec<Option<u32>> = ask(&origin, txt_answer).into_iter().zip(&public).map(|(t, &p)| if p { t.as_deref().and_then(asn_of) } else { None }).collect();
        (names.join().unwrap_or_default(), asns)
    })
}

/// Who runs network `asn`.
fn owner(asn: u32) -> Option<String> {
    ask(&[txt_question(0, &format!("AS{asn}.asn.cymru.com"))], txt_answer).pop().flatten().as_deref().and_then(owner_of)
}

fn private(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local()
}

/// The provider's own domain in a hop's name: "sbcglobal.net" in
/// "99-61-136-1.lightspeed.sbcglobal.net".
fn domain(name: &str) -> Option<String> {
    let labels: Vec<&str> = name.trim_end_matches('.').split('.').collect();
    (labels.len() >= 2).then(|| labels[labels.len() - 2..].join("."))
}

/// Whose part of the way each hop is in: home is the private addresses at
/// the start; the provider starts at the first hop after and runs while
/// the hops are in its network (by number, else by the domain in their
/// names; hops that don't say stay with the ones before).
fn label(hops: &mut [Hop], to: Ipv4Addr) {
    let mut i = 0;
    while i < hops.len() && hops[i].ip.is_none_or(private) && hops[i].ip != Some(to) {
        hops[i].who = Who::Home;
        i += 1;
    }
    let by_asn = hops[i..].iter().any(|h| h.asn.is_some());
    let key = |h: &Hop| if by_asn { h.asn.map(|a| a.to_string()) } else { h.name.as_deref().and_then(domain) };
    let provider = hops[i..].iter().find_map(key);
    let mut in_provider = true;
    for j in i..hops.len() {
        if hops[j].ip == Some(to) {
            hops[j].who = Who::Destination;
            continue;
        }
        in_provider &= match (&provider, key(&hops[j])) {
            (Some(p), Some(k)) => k == *p,
            (Some(_), None) => true,
            (None, _) => j == i,
        };
        hops[j].who = if in_provider { Who::Provider } else { Who::Beyond };
    }
}

/// Trace the way to `target` ("Cloudflare", "Google", or a website).
/// `report` hears the trace as it fills in. Takes about 5 to 10 seconds.
pub fn run(target: &str, report: &dyn Fn(&Trace)) {
    let mut t = Trace { at: chrono::Local::now(), target: target.to_string(), ip: None, provider: None, hops: Vec::new(), done: false, error: None };
    let ip = match crate::probe::HOSTS.iter().find(|h| h.0 == target) {
        Some(h) => Ok(Ipv4Addr::from(h.1)),
        None => host_of(target).and_then(|h| {
            t.target = h.clone();
            resolve(&h)
        }),
    };
    let to = match ip {
        Ok(ip) => ip,
        Err(e) => {
            t.error = Some(e);
            t.done = true;
            return report(&t);
        }
    };
    t.ip = Some(to);
    let found = find_hops(to);
    if found.is_empty() {
        t.error = Some("Nothing answered on the way. Is the internet up? Try \"What's wrong?\".".into());
        t.done = true;
        return report(&t);
    }
    let ips: Vec<Ipv4Addr> = found.iter().flatten().copied().collect();
    let (names, asns) = whose(&ips);
    let mut named = names.into_iter().zip(asns);
    t.hops = found
        .iter()
        .enumerate()
        .map(|(i, ip)| Hop {
            n: i as u8 + 1,
            ip: *ip,
            name: None,
            asn: None,
            who: Who::Beyond,
            sent: None,
            got: 0,
            avg_ms: None,
            max_ms: None,
        })
        .collect();
    for h in t.hops.iter_mut().filter(|h| h.ip.is_some()) {
        (h.name, h.asn) = named.next().unwrap_or_default();
    }
    label(&mut t.hops, to);
    t.provider = t.hops.iter().find(|h| h.who == Who::Provider).and_then(|h| h.asn).and_then(owner);
    report(&t);
    // Then ten pings to every hop at once.
    let measured: Vec<Option<diag::Ping>> = std::thread::scope(|s| {
        let jobs: Vec<_> = t.hops.iter().map(|h| h.ip.map(|ip| s.spawn(move || diag::ping(IpAddr::V4(ip), 10, &[])))).collect();
        jobs.into_iter().map(|j| j.and_then(|j| j.join().ok().flatten())).collect()
    });
    for (h, p) in t.hops.iter_mut().zip(measured) {
        if let Some(p) = p {
            (h.sent, h.got, h.avg_ms, h.max_ms) = (Some(p.sent), p.got, p.avg_ms, p.max_ms);
        }
    }
    t.done = true;
    report(&t);
}

/// `net-watch --route [site]`: trace and print it.
pub fn print_once(target: &str) {
    run(target, &|t| {
        if t.done {
            println!("{}", t.text());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hop(n: u8, ip: [u8; 4], name: Option<&str>, avg: f32, loss: u32) -> Hop {
        Hop { n, ip: Some(ip.into()), name: name.map(str::to_string), asn: None, who: Who::Beyond, sent: Some(10), got: 10 - loss / 10, avg_ms: Some(avg), max_ms: Some(avg), }
    }

    #[test]
    fn reads_owners() {
        assert_eq!(asn_of("7018 | 99.56.0.0/13 | US | arin | 2008-02-25"), Some(7018));
        assert_eq!(owner_of("7018 | US | arin | 1996-07-30 | ATT-INTERNET4 - AT&T Enterprises, LLC, US").as_deref(), Some("AT&T Enterprises"));
        assert_eq!(owner_of("13335 | US | arin | 2010-07-14 | CLOUDFLARENET - Cloudflare, Inc., US").as_deref(), Some("Cloudflare"));
        // A text answer read back from the wire.
        let mut b = txt_question(7, "x.example");
        b[2] |= 0x80;
        b[7] = 1;
        b.extend_from_slice(&[0xc0, 12, 0, 16, 0, 1, 0, 0, 0, 60, 0, 6, 5]);
        b.extend_from_slice(b"7018 ");
        assert_eq!(txt_answer(&b).as_deref(), Some("7018 "));
    }

    #[test]
    fn labels_by_network() {
        let to: Ipv4Addr = [1, 1, 1, 1].into();
        let mut hops = vec![hop(1, [192, 168, 1, 254], None, 3.0, 0), hop(2, [99, 61, 136, 1], None, 4.0, 0), hop(3, [32, 130, 89, 162], None, 6.0, 0), hop(4, [108, 162, 235, 103], None, 20.0, 0), hop(5, [1, 1, 1, 1], None, 19.0, 0)];
        for (h, a) in hops.iter_mut().zip([None, Some(7018), Some(7018), Some(13335), Some(13335)]) {
            h.asn = a;
        }
        label(&mut hops, to);
        let who: Vec<Who> = hops.iter().map(|h| h.who).collect();
        assert_eq!(who, [Who::Home, Who::Provider, Who::Provider, Who::Beyond, Who::Destination]);
    }

    #[test]
    fn reads_what_typed() {
        assert_eq!(host_of(" https://Example.com/a?b ").unwrap(), "example.com");
        assert_eq!(host_of("user@example.com:8080").unwrap(), "example.com");
        assert_eq!(host_of("1.1.1.1").unwrap(), "1.1.1.1");
        for bad in ["", "localhost", "-x.com", "a b.com", "x.com;rm -rf", "ex$ample.com", "--help.com"] {
            assert!(host_of(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reads_ping_replies() {
        let to = Ipv4Addr::new(1, 1, 1, 1);
        assert_eq!(parse_reply("PING 1.1.1.1 (1.1.1.1) 56(84) bytes of data.\nFrom 192.168.1.254 icmp_seq=1 Time to live exceeded\n", to), Reply::Hop(Ipv4Addr::new(192, 168, 1, 254)));
        assert_eq!(parse_reply("PING 1.1.1.1 (1.1.1.1) 56(84) bytes of data.\n64 bytes from 1.1.1.1: icmp_seq=1 ttl=58 time=17.2 ms\n", to), Reply::Arrived);
        assert_eq!(parse_reply("PING 1.1.1.1 (1.1.1.1) 56(84) bytes of data.\n\n--- 1.1.1.1 ping statistics ---\n", to), Reply::Nothing);
    }

    #[test]
    fn splits_the_way() {
        let to = [1, 1, 1, 1];
        let mut t = Trace {
            at: chrono::Local::now(),
            target: "Cloudflare".into(),
            ip: Some(to.into()),
            provider: None,
            hops: vec![
                hop(1, [192, 168, 1, 254], None, 3.0, 0),
                hop(2, [99, 61, 136, 1], Some("99-61-136-1.lightspeed.sbcglobal.net"), 10.0, 0),
                hop(3, [99, 55, 24, 20], Some("x.sbcglobal.net"), 15.0, 40),
                hop(4, [32, 130, 89, 162], None, 16.0, 0),
                hop(5, [108, 162, 235, 103], Some("cloudflare.net"), 19.0, 0),
                hop(6, to, Some("one.one.one.one"), 20.0, 0),
            ],
            done: true,
            error: None,
        };
        label(&mut t.hops, to.into());
        let who: Vec<Who> = t.hops.iter().map(|h| h.who).collect();
        assert_eq!(who, [Who::Home, Who::Provider, Who::Provider, Who::Provider, Who::Beyond, Who::Destination]);
        assert_eq!(t.split(), Some((3.0, 13.0, 4.0)));
        // Loss at one hop only isn't real.
        assert!(t.loss_from().is_none());
        assert!(!t.verdict().2);
        // Loss from hop 3 to the end is.
        for h in &mut t.hops[3..] {
            h.got = 7;
        }
        assert_eq!(t.loss_from().map(|h| h.n), Some(3));
        assert!(t.verdict().0.contains("hop 3 (your provider)"));
    }
}
