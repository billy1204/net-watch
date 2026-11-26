//! The adapters through NetworkManager's `nmcli`, for `netif`.
use super::*;
use std::io::Write;
use std::process::{Command, Stdio};

/// The adapter the internet goes through: the default route with the
/// lowest metric.
pub fn primary_dev() -> Option<String> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    table
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() > 7 && f[1] == "00000000" && f[7] == "00000000").then(|| (f[6].parse::<u32>().unwrap_or(u32::MAX), f[0].to_string()))
        })
        .min()
        .map(|(_, dev)| dev)
}

pub fn adapters() -> Result<Vec<Adapter>, String> {
    let list = nmcli(&["-t", "-f", "DEVICE,TYPE,STATE,CON-UUID", "device"])?;
    let primary = primary_dev();
    let mut found = Vec::new();
    for line in list.lines() {
        let f = fields(line);
        if f.len() < 4 {
            continue;
        }
        let kind = match f[1].as_str() {
            "ethernet" => Kind::Ethernet,
            "wifi" => Kind::Wifi,
            _ => continue,
        };
        let dev = f[0].clone();
        let info = keyed(&nmcli(&["-t", "-f", "GENERAL.VENDOR,GENERAL.PRODUCT,GENERAL.HWADDR,IP4.ADDRESS,IP4.GATEWAY,IP4.DNS", "device", "show", &dev]).unwrap_or_default());
        let get = |k: &str| info.iter().find(|(key, _)| key == k || key.starts_with(&format!("{k}["))).map(|(_, v)| v.clone()).filter(|v| !v.is_empty() && v != "--");
        let sys = |what: &str| std::fs::read_to_string(format!("/sys/class/net/{dev}/{what}")).ok().and_then(|s| s.trim().parse::<i64>().ok());
        let active = !f[3].is_empty();
        let profile = if active { profile(&f[3]).ok() } else if kind == Kind::Ethernet { spare_ethernet(&dev) } else { None };
        let wifi = if kind == Kind::Wifi && active { networks(&dev, false).ok().and_then(|n| n.into_iter().find(|n| n.in_use)) } else { None };
        found.push(Adapter {
            kind,
            state: f[2].clone(),
            model: model(get("GENERAL.VENDOR").unwrap_or_default(), get("GENERAL.PRODUCT").unwrap_or_default()),
            mac: get("GENERAL.HWADDR").unwrap_or_default(),
            profile,
            active,
            address: get("IP4.ADDRESS"),
            gateway: get("IP4.GATEWAY"),
            dns: info.iter().filter(|(k, _)| k.starts_with("IP4.DNS")).map(|(_, v)| v.clone()).collect(),
            speed: sys("speed").filter(|s| *s > 0).map(|s| s as u32),
            mtu: sys("mtu").map(|m| m as u32),
            primary: primary.as_deref() == Some(dev.as_str()),
            wifi,
            dev,
        });
    }
    // The one in use first, then Ethernet, then by name.
    found.sort_by_key(|a| (!a.primary, !a.connected(), a.kind != Kind::Ethernet, a.dev.clone()));
    Ok(found)
}

/// "MEDIATEK Corp." and "MT7922 802.11ax PCI Express Wireless Network
/// Adapter" make "MediaTek MT7922".
fn model(vendor: String, product: String) -> String {
    let vendor = vendor.split([' ', ',']).next().unwrap_or("").to_string();
    let vendor = match vendor.to_lowercase().as_str() {
        "mediatek" => "MediaTek".into(),
        "intel" => "Intel".into(),
        "realtek" => "Realtek".into(),
        "qualcomm" => "Qualcomm".into(),
        "broadcom" => "Broadcom".into(),
        _ => vendor,
    };
    let product = product.split_whitespace().next().unwrap_or("");
    let product = product.trim_start_matches(&vendor).trim();
    format!("{vendor} {product}").trim().to_string()
}

/// For an Ethernet adapter with nothing plugged in: the wired settings it
/// would use.
fn spare_ethernet(dev: &str) -> Option<Profile> {
    let list = nmcli(&["-t", "-f", "UUID,TYPE", "connection", "show"]).ok()?;
    list.lines().map(fields).filter(|f| f.len() >= 2 && f[1] == "802-3-ethernet").find_map(|f| {
        let bound = nmcli(&["-g", "connection.interface-name", "connection", "show", &f[0]]).ok()?;
        let bound = bound.trim();
        if bound.is_empty() || bound == dev { profile(&f[0]).ok() } else { None }
    })
}

pub fn profile(uuid: &str) -> Result<Profile, String> {
    let text = nmcli(&[
        "-t",
        "-f",
        "connection.id,connection.uuid,connection.autoconnect,ipv4.method,ipv4.addresses,ipv4.gateway,ipv4.dns,ipv4.ignore-auto-dns,ipv4.route-metric",
        "connection",
        "show",
        uuid,
    ])?;
    let kv = keyed(&text);
    let get = |k: &str| kv.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).filter(|v| v != "--").unwrap_or_default();
    let list = |k: &str| get(k).split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect::<Vec<_>>();
    Ok(Profile {
        uuid: get("connection.uuid"),
        name: get("connection.id"),
        manual: get("ipv4.method") == "manual",
        address: list("ipv4.addresses").into_iter().next().unwrap_or_default(),
        gateway: get("ipv4.gateway"),
        dns: list("ipv4.dns"),
        ignore_auto_dns: get("ipv4.ignore-auto-dns") == "yes",
        autoconnect: get("connection.autoconnect") != "no",
        metric: get("ipv4.route-metric").parse().unwrap_or(-1),
    })
}

/// Save `p`, and when `dev` is given (it's in use there) bring it in now.
pub fn save_profile(p: &Profile, dev: Option<&str>) -> Result<(), String> {
    let yes = |b: bool| if b { "yes" } else { "no" };
    let dns = p.dns.join(",");
    let metric = p.metric.to_string();
    let (address, gateway) = if p.manual { (p.address.as_str(), p.gateway.as_str()) } else { ("", "") };
    nmcli(&[
        "connection",
        "modify",
        &p.uuid,
        "ipv4.method",
        if p.manual { "manual" } else { "auto" },
        "ipv4.addresses",
        address,
        "ipv4.gateway",
        gateway,
        "ipv4.dns",
        &dns,
        "ipv4.ignore-auto-dns",
        yes(p.ignore_auto_dns),
        "connection.autoconnect",
        yes(p.autoconnect),
        "ipv4.route-metric",
        &metric,
    ])?;
    if let Some(dev) = dev {
        // Gently if it can; some changes need the connection restarted.
        if nmcli(&["device", "reapply", dev]).is_err() {
            nmcli(&["-w", "30", "connection", "up", &p.uuid, "ifname", dev])?;
        }
    }
    Ok(())
}

/// Save a new Wi-Fi network; the password goes in through the editor's
/// input, so it never shows in the list of running programs. Its id.
pub fn add_wifi(dev: &str, ssid: &str, security: &str, password: &str, auto: bool, hidden: bool) -> Result<String, String> {
    let key_mgmt = if security.contains("802.1X") {
        return Err("this network needs a user name and certificates; use the desktop's own Wi-Fi settings".into());
    } else if security.contains("WEP") {
        return Err("this network uses old WEP security, which isn't supported".into());
    } else if security.is_empty() && password.is_empty() {
        None
    } else if security.contains("WPA3") && !security.contains("WPA2") {
        Some("sae")
    } else {
        Some("wpa-psk")
    };
    let mut args = vec!["connection", "add", "type", "wifi", "ifname", dev, "con-name", ssid, "ssid", ssid];
    args.extend(["connection.autoconnect", if auto { "yes" } else { "no" }]);
    if hidden {
        args.extend(["802-11-wireless.hidden", "yes"]);
    }
    if let Some(k) = key_mgmt {
        args.extend(["802-11-wireless-security.key-mgmt", k]);
    }
    let out = nmcli(&args)?;
    // "Connection 'Name' (uuid) successfully added."
    let uuid = out.rsplit_once('(').and_then(|(_, r)| r.split_once(')')).map(|(u, _)| u.to_string()).ok_or("couldn't save the network")?;
    if key_mgmt.is_some() {
        if password.contains('\n') {
            let _ = nmcli(&["connection", "delete", &uuid]);
            return Err("the password can't have a line break in it".into());
        }
        let script = format!("set 802-11-wireless-security.psk {password}\nsave persistent\nquit\n");
        if let Err(e) = nmcli_input(&["connection", "edit", &uuid], &script) {
            let _ = nmcli(&["connection", "delete", &uuid]);
            return Err(e);
        }
    }
    Ok(uuid)
}

/// The Wi-Fi networks `dev` can see, strongest first; `rescan` looks again
/// (a few seconds) rather than using what it saw last. Not marked saved.
pub fn networks(dev: &str, rescan: bool) -> Result<Vec<WifiNet>, String> {
    let text = nmcli(&["-t", "-f", "IN-USE,SSID,SECURITY,FREQ,SIGNAL", "device", "wifi", "list", "ifname", dev, "--rescan", if rescan { "yes" } else { "no" }])?;
    let mut found: Vec<WifiNet> = Vec::new();
    for f in text.lines().map(fields) {
        if f.len() < 5 || f[1].is_empty() {
            continue;
        }
        let net = WifiNet {
            ssid: f[1].clone(),
            security: if f[2] == "--" { String::new() } else { f[2].clone() },
            freq: f[3].split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0),
            signal: f[4].parse().unwrap_or(0),
            in_use: f[0] == "*",
            saved: None,
        };
        // One line per name: the strongest, or the one in use.
        match found.iter_mut().find(|n| n.ssid == net.ssid) {
            Some(n) if net.in_use || (!n.in_use && net.signal > n.signal) => *n = net,
            Some(_) => {}
            None => found.push(net),
        }
    }
    found.sort_by_key(|n| (!n.in_use, std::cmp::Reverse(n.signal)));
    Ok(found)
}

/// Saved Wi-Fi networks: name and settings id.
pub fn saved_wifi() -> Vec<(String, String)> {
    let Ok(list) = nmcli(&["-t", "-f", "UUID,TYPE", "connection", "show"]) else { return Vec::new() };
    list.lines()
        .map(fields)
        .filter(|f| f.len() >= 2 && f[1] == "802-11-wireless")
        .filter_map(|f| {
            let ssid = nmcli(&["-g", "802-11-wireless.ssid", "connection", "show", &f[0]]).ok()?;
            Some((unescape(ssid.trim()), f[0].clone()))
        })
        .collect()
}

fn nmcli(args: &[&str]) -> Result<String, String> {
    nmcli_input(args, "")
}

fn nmcli_input(args: &[&str], input: &str) -> Result<String, String> {
    let mut child = Command::new("nmcli")
        .args(args)
        .env("LC_ALL", "C")
        .stdin(if input.is_empty() { Stdio::null() } else { Stdio::piped() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "needs NetworkManager (nmcli), which isn't here".to_string() } else { e.to_string() })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    // The editor says what went wrong on its output, and still exits 0.
    let editor_error = !input.is_empty() && stdout.lines().any(|l| l.contains("Error:"));
    if out.status.success() && !editor_error {
        return Ok(stdout);
    }
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &stdout;
    let line = text.lines().find(|l| l.contains("Error:")).or(text.lines().find(|l| !l.trim().is_empty())).unwrap_or("nmcli failed");
    let line = line.split("Error:").last().unwrap_or(line).trim().trim_end_matches('.');
    Err(line.to_string())
}

/// A line of `nmcli -t`: fields split on ':', with "\:" and "\\" kept.
fn fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.last_mut().unwrap().extend(chars.next()),
            ':' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    fields(s).join(":")
}

/// "KEY:value" lines, as `nmcli -t ... show` gives them.
fn keyed(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Bring in saved settings `uuid` on `dev`, waiting up to 30 seconds.
pub fn up(uuid: &str, dev: &str) -> Result<(), String> {
    nmcli(&["-w", "30", "connection", "up", uuid, "ifname", dev]).map(drop)
}

pub fn disconnect(dev: &str) -> Result<(), String> {
    nmcli(&["device", "disconnect", dev]).map(drop)
}

pub fn connect(dev: &str) -> Result<(), String> {
    nmcli(&["-w", "30", "device", "connect", dev]).map(drop)
}

/// Forget saved settings.
pub fn delete(uuid: &str) -> Result<(), String> {
    nmcli(&["connection", "delete", uuid]).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terse_fields() {
        assert_eq!(fields(r"*:Home\:Net:WPA2:5240 MHz:79"), ["*", "Home:Net", "WPA2", "5240 MHz", "79"]);
        assert_eq!(fields("a::b"), ["a", "", "b"]);
    }

    #[test]
    fn models() {
        assert_eq!(model("MEDIATEK Corp.".into(), "MT7922 802.11ax PCI Express Wireless Network Adapter".into()), "MediaTek MT7922");
        assert_eq!(model("Realtek Semiconductor Co., Ltd.".into(), "RTL8111/8168/8211/8411 PCI Express Gigabit Ethernet Controller".into()), "Realtek RTL8111/8168/8211/8411");
    }
}
