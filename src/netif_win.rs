//! The adapters on Windows, for `netif`: read through IP Helper and the
//! WLAN API (no admin rights needed), changed through the WLAN API for
//! Wi-Fi networks and, for addresses, DNS, the metric and turning a cable
//! adapter off, a PowerShell script that Windows asks permission to run.
//!
//! Windows keeps IP settings per adapter rather than per network, so an
//! adapter's `Profile` is its own settings; its `uuid` is the adapter's
//! GUID, followed by "|" and the Wi-Fi network's saved profile when there
//! is one. A saved Wi-Fi network's id is the same: "{GUID}|name".
use super::*;
use crate::win::{self, Wlan, ps_quote};

/// "{GUID}|Home Wi-Fi" into the GUID and the Wi-Fi profile.
fn split_id(uuid: &str) -> (&str, Option<&str>) {
    match uuid.split_once('|') {
        Some((g, p)) => (g, Some(p)),
        None => (uuid, None),
    }
}

fn join_id(guid: &str, profile: &str) -> String {
    format!("{guid}|{profile}")
}

/// Cable and Wi-Fi adapters that Windows has turned off: still in the
/// interface table, though not present for networking.
fn disabled() -> Vec<win::Adapter> {
    use windows_sys::Win32::NetworkManagement::IpHelper::*;
    let mut out = Vec::new();
    unsafe {
        let mut t: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut t) != 0 || t.is_null() {
            return out;
        }
        let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), (*t).NumEntries as usize);
        for r in rows {
            let flags = r.InterfaceAndOperStatusFlags._bitfield;
            let real = flags & 1 != 0 && flags & 2 == 0;
            // IfOperStatusNotPresent with the adapter switched off.
            let off = r.OperStatus == 6 || r.AdminStatus == 2;
            if real && off && (r.Type == IF_TYPE_ETHERNET_CSMACD || r.Type == IF_TYPE_IEEE80211) {
                out.push(win::Adapter {
                    name: win::from_wide(&r.Alias),
                    description: win::from_wide(&r.Description),
                    guid: win::guid_string(&r.InterfaceGuid),
                    index: r.InterfaceIndex,
                    luid: r.InterfaceLuid.Value,
                    if_type: r.Type,
                    physical: true,
                    ..Default::default()
                });
            }
        }
        FreeMibTable(t as *const std::ffi::c_void);
    }
    out
}

/// The Ethernet and Wi-Fi adapters worth showing.
fn hardware() -> Vec<win::Adapter> {
    let mut list: Vec<win::Adapter> = win::adapters().into_iter().filter(|a| a.physical && (a.ethernet() || a.wifi())).collect();
    for d in disabled() {
        if !list.iter().any(|a| a.guid.eq_ignore_ascii_case(&d.guid)) {
            list.push(d);
        }
    }
    list
}

fn find(dev: &str) -> Result<win::Adapter, String> {
    hardware().into_iter().find(|a| a.name == dev).ok_or_else(|| format!("{dev} isn't there any more"))
}

/// The adapter the internet goes through: the default route with the
/// lowest metric.
pub fn primary_dev() -> Option<String> {
    let r = win::default_routes().into_iter().next()?;
    win::adapters().into_iter().find(|a| a.luid == r.luid).map(|a| a.name)
}

/// "Intel(R) Wi-Fi 6E AX211 160MHz" makes "Intel Wi-Fi 6E AX211 160MHz".
fn model(description: &str) -> String {
    let mut m = description.replace("(R)", "").replace("(TM)", "").replace(['®', '™'], "");
    for end in [" Network Adapter", " Network Connection", " Adapter", " Family Controller", " Controller"] {
        if let Some(cut) = m.strip_suffix(end) {
            m = cut.to_string();
        }
    }
    m.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn adapters() -> Result<Vec<Adapter>, String> {
    let primary = primary_dev();
    let wlan = Wlan::open();
    let disabled: Vec<String> = disabled().into_iter().map(|d| d.guid).collect();
    let mut found = Vec::new();
    for a in hardware() {
        let kind = if a.wifi() { Kind::Wifi } else { Kind::Ethernet };
        let off = disabled.iter().any(|g| g.eq_ignore_ascii_case(&a.guid));
        let wguid = wlan.as_ref().filter(|_| a.wifi()).and_then(|w| w.find(&a.guid));
        let current = wguid.and_then(|g| wlan.as_ref()?.current(&g));
        let radio_off = wguid.is_some_and(|g| wlan.as_ref().is_some_and(|w| w.radio_off(&g)));
        // NetworkManager's words, which the window knows.
        let state = match () {
            _ if off => "disconnected",
            _ if a.up => "connected",
            _ if kind == Kind::Ethernet => "unavailable",
            _ if radio_off => "unavailable",
            _ => "disconnected",
        };
        let active = state == "connected";
        let wifi = match (&current, wguid) {
            (Some(_), Some(_)) => networks(&a.name, false).ok().and_then(|n| n.into_iter().find(|n| n.in_use)).or_else(|| {
                current.as_ref().map(|c| WifiNet { ssid: c.ssid.clone(), security: String::new(), freq: 0, signal: c.signal, in_use: true, saved: None })
            }),
            _ => None,
        };
        // For Wi-Fi, the settings show once it's on a network (as on
        // Linux, where they belong to the network).
        let profile = match (kind, &current) {
            _ if off => None,
            (Kind::Ethernet, _) => Some(profile_of(&a, None, wlan.as_ref())),
            (Kind::Wifi, Some(c)) => Some(profile_of(&a, Some(&c.profile), wlan.as_ref())),
            (Kind::Wifi, None) => None,
        };
        found.push(Adapter {
            kind,
            state: state.into(),
            model: model(&a.description),
            mac: a.mac.to_uppercase(),
            profile,
            active,
            address: a.v4.iter().find(|(ip, _)| !ip.is_link_local()).or(a.v4.first()).map(|(ip, n)| format!("{ip}/{n}")).filter(|_| active),
            gateway: a.gateways.first().map(|g| g.to_string()),
            dns: a.dns.iter().filter(|ip| ip.is_ipv4()).map(|ip| ip.to_string()).collect(),
            speed: Some((a.speed / 1_000_000) as u32).filter(|&s| s > 0 && active && kind == Kind::Ethernet),
            mtu: Some(a.mtu).filter(|&m| m > 0 && m < 65535),
            primary: primary.as_deref() == Some(a.name.as_str()),
            wifi,
            dev: a.name,
        });
    }
    // The one in use first, then Ethernet, then by name.
    found.sort_by_key(|a| (!a.primary, !a.connected(), a.kind != Kind::Ethernet, a.dev.clone()));
    Ok(found)
}

/// The static settings Windows keeps for an adapter, from the registry:
/// fixed addresses, a fixed gateway and fixed DNS servers.
struct Fixed {
    address: Option<(String, String)>,
    gateway: Option<String>,
    dns: Vec<String>,
}

fn fixed(guid: &str) -> Fixed {
    use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    let key = format!(r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\{guid}");
    let multi = |v: &str| reg_multi(HKEY_LOCAL_MACHINE, &key, v);
    let address = multi("IPAddress").into_iter().zip(multi("SubnetMask")).find(|(a, _)| a != "0.0.0.0");
    let gateway = multi("DefaultGateway").into_iter().find(|g| !g.is_empty());
    let dns = win::reg_string(HKEY_LOCAL_MACHINE, &key, "NameServer")
        .unwrap_or_default()
        .split([',', ' '])
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    Fixed { address, gateway, dns }
}

/// A REG_MULTI_SZ (or REG_SZ) value's strings.
fn reg_multi(root: windows_sys::Win32::System::Registry::HKEY, key: &str, value: &str) -> Vec<String> {
    use windows_sys::Win32::System::Registry::*;
    let (k, v) = (win::wide(key), win::wide(value));
    let mut size = 0u32;
    unsafe {
        let kinds = RRF_RT_REG_MULTI_SZ | RRF_RT_REG_SZ;
        if RegGetValueW(root, k.as_ptr(), v.as_ptr(), kinds, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) != 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; size as usize / 2 + 2];
        if RegGetValueW(root, k.as_ptr(), v.as_ptr(), kinds, std::ptr::null_mut(), buf.as_mut_ptr() as *mut std::ffi::c_void, &mut size) != 0 {
            return Vec::new();
        }
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

fn profile_of(a: &win::Adapter, wifi: Option<&str>, wlan: Option<&Wlan>) -> Profile {
    let f = fixed(&a.guid);
    let manual = !a.dhcp;
    let address = match (&f.address, manual) {
        (Some((ip, mask)), true) => format!("{ip}/{}", prefix(mask).unwrap_or(24)),
        _ => String::new(),
    };
    let (metric, auto_metric) = win::ip_interface(a.luid).unwrap_or((0, true));
    let autoconnect = match (wifi, wlan) {
        (Some(p), Some(w)) => w.find(&a.guid).and_then(|g| w.profile_xml(&g, p)).is_none_or(|xml| !xml.contains("<connectionMode>manual</connectionMode>")),
        _ => true,
    };
    Profile {
        uuid: wifi.map_or_else(|| a.guid.clone(), |p| join_id(&a.guid, p)),
        name: wifi.map_or_else(|| a.name.clone(), str::to_string),
        manual,
        address,
        gateway: if manual { f.gateway.unwrap_or_default() } else { String::new() },
        ignore_auto_dns: !f.dns.is_empty(),
        dns: f.dns,
        autoconnect,
        metric: if auto_metric { -1 } else { metric as i64 },
    }
}

/// Save `p`. Windows puts IP settings in at once, so `dev` (given when
/// it's in use) changes nothing here.
pub fn save_profile(p: &Profile, _dev: Option<&str>) -> Result<(), String> {
    let (guid, wifi) = split_id(&p.uuid);
    let a = hardware().into_iter().find(|a| a.guid.eq_ignore_ascii_case(guid)).ok_or("the adapter isn't there any more")?;
    let wlan = Wlan::open();
    let now = profile_of(&a, wifi, wlan.as_ref());
    let i = a.index;
    let mut script = String::new();
    if p.manual != now.manual || (p.manual && (p.address != now.address || p.gateway != now.gateway)) {
        if p.manual {
            let (ip, bits) = p.address.split_once('/').ok_or("the address needs a mask")?;
            let gateway = if p.gateway.is_empty() { String::new() } else { format!(" -DefaultGateway {}", ps_quote(&p.gateway)) };
            script += &format!(
                "Set-NetIPInterface -InterfaceIndex {i} -AddressFamily IPv4 -Dhcp Disabled\r\n\
                 Get-NetRoute -InterfaceIndex {i} -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | Remove-NetRoute -Confirm:$false\r\n\
                 Get-NetIPAddress -InterfaceIndex {i} -AddressFamily IPv4 -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false\r\n\
                 New-NetIPAddress -InterfaceIndex {i} -AddressFamily IPv4 -IPAddress {} -PrefixLength {}{gateway} | Out-Null\r\n",
                ps_quote(ip),
                bits.parse::<u8>().map_err(|_| "the mask isn't right")?
            );
        } else {
            script += &format!(
                "Get-NetRoute -InterfaceIndex {i} -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | Remove-NetRoute -Confirm:$false\r\n\
                 Get-NetIPAddress -InterfaceIndex {i} -AddressFamily IPv4 -PrefixOrigin Manual -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false\r\n\
                 Set-NetIPInterface -InterfaceIndex {i} -AddressFamily IPv4 -Dhcp Enabled\r\n"
            );
        }
    }
    // Windows has no "the router's and these too": servers given are the
    // only ones asked.
    if p.dns != now.dns {
        if p.dns.is_empty() {
            script += &format!("Set-DnsClientServerAddress -InterfaceIndex {i} -ResetServerAddresses\r\n");
        } else {
            let list: Vec<String> = p.dns.iter().map(|d| ps_quote(d)).collect();
            script += &format!("Set-DnsClientServerAddress -InterfaceIndex {i} -ServerAddresses @({})\r\n", list.join(","));
        }
    }
    if p.metric != now.metric {
        script += &if p.metric < 0 {
            format!("Set-NetIPInterface -InterfaceIndex {i} -AddressFamily IPv4 -AutomaticMetric Enabled\r\n")
        } else {
            format!("Set-NetIPInterface -InterfaceIndex {i} -AddressFamily IPv4 -InterfaceMetric {}\r\n", p.metric)
        };
    }
    if !script.is_empty() {
        win::admin_powershell(&script)?;
    }
    if let (Some(name), true) = (wifi, p.autoconnect != now.autoconnect) {
        set_autoconnect(&a, name, p.autoconnect)?;
    }
    Ok(())
}

/// Whether Windows joins a saved Wi-Fi network on its own.
fn set_autoconnect(a: &win::Adapter, name: &str, auto: bool) -> Result<(), String> {
    let w = Wlan::open().ok_or("the WLAN AutoConfig service isn't running")?;
    let g = w.find(&a.guid).ok_or("the Wi-Fi adapter wasn't found")?;
    let xml = w.profile_xml(&g, name).ok_or("couldn't read the network's settings")?;
    let (from, to) = if auto { ("<connectionMode>manual</connectionMode>", "<connectionMode>auto</connectionMode>") } else { ("<connectionMode>auto</connectionMode>", "<connectionMode>manual</connectionMode>") };
    let changed = xml.replacen(from, to, 1);
    if changed == xml {
        return Ok(());
    }
    // A network saved for everyone needs admin rights to change.
    if w.set_profile(&g, &changed, true).is_ok() || w.set_profile(&g, &changed, false).is_ok() {
        return Ok(());
    }
    win::admin_powershell(&format!(
        "netsh wlan set profileparameter name={} interface={} connectionmode={}\r\nif ($LASTEXITCODE -ne 0) {{ throw 'netsh couldn''t change it' }}\r\n",
        ps_quote(name),
        ps_quote(&a.name),
        if auto { "auto" } else { "manual" }
    ))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Save a new Wi-Fi network for this user (the password stays inside
/// Windows' own store; it's never on a command line). Its id.
pub fn add_wifi(dev: &str, ssid: &str, security: &str, password: &str, auto: bool, hidden: bool) -> Result<String, String> {
    let (auth, cipher) = if security.contains("802.1X") {
        return Err("this network needs a user name and certificates; use Windows' own Wi-Fi settings".into());
    } else if security.contains("WEP") {
        return Err("this network uses old WEP security, which isn't supported".into());
    } else if security.is_empty() && password.is_empty() {
        ("open", "none")
    } else if security == "OWE" {
        ("OWE", "AES")
    } else if security.contains("WPA3") && !security.contains("WPA2") {
        ("WPA3SAE", "AES")
    } else if security == "WPA1" {
        ("WPAPSK", "TKIP")
    } else {
        ("WPA2PSK", "AES")
    };
    let a = find(dev)?;
    let w = Wlan::open().ok_or("the WLAN AutoConfig service isn't running")?;
    let g = w.find(&a.guid).ok_or("the Wi-Fi adapter wasn't found")?;
    let hex: String = ssid.bytes().map(|b| format!("{b:02X}")).collect();
    let key = if matches!(auth, "open" | "OWE") {
        String::new()
    } else {
        format!("<sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{}</keyMaterial></sharedKey>", xml_escape(password))
    };
    let xml = format!(
        r#"<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1">
<name>{name}</name>
<SSIDConfig><SSID><hex>{hex}</hex><name>{name}</name></SSID><nonBroadcast>{hidden}</nonBroadcast></SSIDConfig>
<connectionType>ESS</connectionType>
<connectionMode>{mode}</connectionMode>
<MSM><security><authEncryption><authentication>{auth}</authentication><encryption>{cipher}</encryption><useOneX>false</useOneX></authEncryption>{key}</security></MSM>
</WLANProfile>"#,
        name = xml_escape(ssid),
        mode = if auto { "auto" } else { "manual" },
    );
    w.set_profile(&g, &xml, true)?;
    Ok(join_id(&a.guid, ssid))
}

fn wlan_for(dev: &str) -> Result<(Wlan, windows_sys::core::GUID, win::Adapter), String> {
    let a = find(dev)?;
    let w = Wlan::open().ok_or("the WLAN AutoConfig service isn't running")?;
    let g = w.find(&a.guid).ok_or("the Wi-Fi adapter wasn't found")?;
    Ok((w, g, a))
}

/// The Wi-Fi networks `dev` can see, strongest first; `rescan` looks again
/// (a few seconds) rather than using what it saw last. Not marked saved.
pub fn networks(dev: &str, rescan: bool) -> Result<Vec<WifiNet>, String> {
    let (w, g, _) = wlan_for(dev)?;
    let mut found: Vec<WifiNet> = w
        .networks(&g, rescan)?
        .into_iter()
        .map(|n| WifiNet { ssid: n.ssid, security: n.security, freq: n.freq, signal: n.signal, in_use: n.connected, saved: None })
        .collect();
    found.sort_by_key(|n| (!n.in_use, std::cmp::Reverse(n.signal)));
    Ok(found)
}

/// Saved Wi-Fi networks: name and id, on every Wi-Fi adapter.
pub fn saved_wifi() -> Vec<(String, String)> {
    let Some(w) = Wlan::open() else { return Vec::new() };
    let mut out = Vec::new();
    for (g, _) in w.interfaces() {
        let guid = win::guid_string(&g);
        for name in w.profiles(&g) {
            // The network's own name, from inside the profile.
            let ssid = w
                .profile_xml(&g, &name)
                .and_then(|x| {
                    let start = x.find("<SSID>")?;
                    let tail = &x[start..];
                    let n = tail.find("<name>")? + 6;
                    let e = tail[n..].find("</name>")?;
                    Some(tail[n..n + e].replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'"))
                })
                .unwrap_or_else(|| name.clone());
            out.push((ssid, join_id(&guid, &name)));
        }
    }
    out
}

/// Bring in saved settings: join the Wi-Fi network, or for a cable adapter
/// turn it on.
pub fn up(uuid: &str, dev: &str) -> Result<(), String> {
    let (_, wifi) = split_id(uuid);
    match wifi {
        Some(name) => {
            let a = find(dev)?;
            if disabled().iter().any(|d| d.guid.eq_ignore_ascii_case(&a.guid)) {
                enable(&a.name, true)?;
            }
            let (w, g, _) = wlan_for(dev)?;
            w.connect(&g, name, Duration::from_secs(30))
        }
        None => enable(dev, true),
    }
}

fn enable(dev: &str, on: bool) -> Result<(), String> {
    let verb = if on { "Enable-NetAdapter" } else { "Disable-NetAdapter" };
    win::admin_powershell(&format!("{verb} -Name {} -Confirm:$false\r\n", ps_quote(dev)))?;
    // A few seconds for Windows to bring it up or down.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        let off = disabled().iter().any(|d| d.name == dev);
        if off != on {
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

/// Wi-Fi leaves its network; a cable adapter is turned off (Windows asks
/// permission for that).
pub fn disconnect(dev: &str) -> Result<(), String> {
    let a = find(dev)?;
    if a.wifi() {
        let (w, g, _) = wlan_for(dev)?;
        return w.disconnect(&g);
    }
    enable(dev, false)
}

/// Turn it back on; for Wi-Fi, join the strongest saved network in range.
pub fn connect(dev: &str) -> Result<(), String> {
    let a = find(dev)?;
    if disabled().iter().any(|d| d.guid.eq_ignore_ascii_case(&a.guid)) {
        enable(dev, true)?;
        if a.ethernet() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    if !a.wifi() {
        return Ok(());
    }
    let (w, g, _) = wlan_for(dev)?;
    let best = w.networks(&g, false)?.into_iter().filter(|n| n.profile.is_some()).max_by_key(|n| n.signal);
    match best.and_then(|n| n.profile) {
        Some(p) => w.connect(&g, &p, Duration::from_secs(30)),
        None => Err("no saved network is in range; choose one from the list".into()),
    }
}

/// Forget a saved Wi-Fi network.
pub fn delete(uuid: &str) -> Result<(), String> {
    let (guid, Some(name)) = split_id(uuid) else { return Err("that isn't a saved Wi-Fi network".into()) };
    let w = Wlan::open().ok_or("the WLAN AutoConfig service isn't running")?;
    let g = w.find(guid).ok_or("the Wi-Fi adapter wasn't found")?;
    if w.delete_profile(&g, name).is_ok() {
        return Ok(());
    }
    // Saved for everyone on this computer: that needs admin rights.
    win::admin_powershell(&format!("netsh wlan delete profile name={}\r\nif ($LASTEXITCODE -ne 0) {{ throw 'netsh couldn''t forget it' }}\r\n", ps_quote(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models() {
        assert_eq!(model("Intel(R) Wi-Fi 6E AX211 160MHz"), "Intel Wi-Fi 6E AX211 160MHz");
        assert_eq!(model("Realtek PCIe 2.5GbE Family Controller"), "Realtek PCIe 2.5GbE");
        assert_eq!(model("Intel(R) Ethernet Connection (17) I219-LM"), "Intel Ethernet Connection (17) I219-LM");
    }

    #[test]
    fn ids() {
        assert_eq!(split_id("{AB}|Home|Net"), ("{AB}", Some("Home|Net")));
        assert_eq!(split_id("{AB}"), ("{AB}", None));
    }
}
