//! Windows' own ways of telling what the network is doing, in place of the
//! files Linux keeps under /proc and /sys: the IP Helper tables (adapters,
//! routes, neighbours, connections), the WLAN API for Wi-Fi, ICMP echo for
//! ping, and the registry. None of it needs admin rights; changing an
//! adapter's settings does, and goes through `admin_powershell`.
#![allow(non_snake_case, clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::NetworkManagement::IpHelper::*;
use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows_sys::Win32::NetworkManagement::WiFi::*;
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6, SOCKADDR_INET, SOCKET_ADDRESS};
use windows_sys::core::GUID;

// -- Small helpers -----------------------------------------------------------

/// A string as Windows wants it: UTF-16 with a 0 at the end.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A UTF-16 buffer up to its first 0.
pub fn from_wide(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

unsafe fn from_pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

/// Windows' "CREATE_NO_WINDOW": a console program started from here
/// doesn't flash up a black window.
const NO_WINDOW: u32 = 0x0800_0000;

/// A program to run, without a console window.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(NO_WINDOW);
    c
}

/// "{4D36E972-E325-11CE-BFC1-08002BE10318}".
pub fn guid_string(g: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7]
    )
}

fn mac_string(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

unsafe fn sockaddr(a: &SOCKET_ADDRESS) -> Option<IpAddr> {
    let p = a.lpSockaddr;
    if p.is_null() {
        return None;
    }
    match unsafe { (*p).sa_family } {
        AF_INET => {
            let v4 = unsafe { &*(p as *const SOCKADDR_IN) };
            Some(IpAddr::V4(Ipv4Addr::from(unsafe { v4.sin_addr.S_un.S_addr }.to_ne_bytes())))
        }
        AF_INET6 => {
            let v6 = unsafe { &*(p as *const SOCKADDR_IN6) };
            Some(IpAddr::V6(Ipv6Addr::from(unsafe { v6.sin6_addr.u.Byte })))
        }
        _ => None,
    }
}

unsafe fn inet(a: &SOCKADDR_INET) -> Option<IpAddr> {
    match unsafe { a.si_family } {
        AF_INET => Some(IpAddr::V4(Ipv4Addr::from(unsafe { a.Ipv4.sin_addr.S_un.S_addr }.to_ne_bytes()))),
        AF_INET6 => Some(IpAddr::V6(Ipv6Addr::from(unsafe { a.Ipv6.sin6_addr.u.Byte }))),
        _ => None,
    }
}

// -- Adapters ----------------------------------------------------------------

/// One network adapter, as IP Helper sees it.
#[derive(Clone, Debug, Default)]
pub struct Adapter {
    /// What people see in Settings, like "Wi-Fi" or "Ethernet 2".
    pub name: String,
    /// The driver's description, like "Intel(R) Wi-Fi 6E AX211 160MHz".
    pub description: String,
    /// Its GUID, "{…}".
    pub guid: String,
    pub index: u32,
    pub luid: u64,
    /// 6 is Ethernet, 71 Wi-Fi.
    pub if_type: u32,
    pub up: bool,
    pub mac: String,
    pub mtu: u32,
    /// Link speed in bits a second.
    pub speed: u64,
    pub dhcp: bool,
    /// IPv4 addresses with their prefix length.
    pub v4: Vec<(Ipv4Addr, u8)>,
    /// Global IPv6 addresses.
    pub v6: Vec<Ipv6Addr>,
    pub gateways: Vec<Ipv4Addr>,
    pub dns: Vec<IpAddr>,
    /// A real adapter, not a VPN's, a virtual switch's or a filter.
    pub physical: bool,
}

impl Adapter {
    pub fn wifi(&self) -> bool {
        self.if_type == IF_TYPE_IEEE80211
    }
    pub fn ethernet(&self) -> bool {
        self.if_type == IF_TYPE_ETHERNET_CSMACD
    }
}

/// Which interfaces are real hardware, by LUID.
fn hardware() -> HashMap<u64, bool> {
    let mut out = HashMap::new();
    unsafe {
        let mut t: *mut MIB_IF_TABLE2 = null_mut();
        if GetIfTable2(&mut t) != NO_ERROR || t.is_null() {
            return out;
        }
        let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), (*t).NumEntries as usize);
        for r in rows {
            let flags = r.InterfaceAndOperStatusFlags._bitfield;
            // HardwareInterface, not FilterInterface, ConnectorPresent.
            let real = flags & 1 != 0 && flags & 2 == 0 && flags & 4 != 0;
            out.insert(r.InterfaceLuid.Value, real);
        }
        FreeMibTable(t as *const c_void);
    }
    out
}

pub fn adapters() -> Vec<Adapter> {
    let hw = hardware();
    let mut out = Vec::new();
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_INCLUDE_ALL_INTERFACES;
    let mut size: u32 = 32 * 1024;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..4 {
        buf = vec![0u64; size as usize / 8 + 1];
        let r = unsafe { GetAdaptersAddresses(AF_UNSPEC as u32, flags, null(), buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH, &mut size) };
        match r {
            NO_ERROR => break,
            ERROR_BUFFER_OVERFLOW => continue,
            _ => return out,
        }
    }
    let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    unsafe {
        while !p.is_null() {
            let a = &*p;
            let mut ad = Adapter {
                name: from_pwstr(a.FriendlyName),
                description: from_pwstr(a.Description),
                guid: std::ffi::CStr::from_ptr(a.AdapterName as *const i8).to_string_lossy().into_owned(),
                index: a.Anonymous1.Anonymous.IfIndex,
                luid: a.Luid.Value,
                if_type: a.IfType,
                up: a.OperStatus == 1,
                mac: mac_string(&a.PhysicalAddress[..(a.PhysicalAddressLength as usize).min(8)]),
                mtu: a.Mtu,
                speed: a.ReceiveLinkSpeed.min(a.TransmitLinkSpeed.max(a.ReceiveLinkSpeed)),
                dhcp: a.Anonymous2.Flags & IP_ADAPTER_DHCP_ENABLED != 0,
                physical: hw.get(&a.Luid.Value).copied().unwrap_or(false),
                ..Default::default()
            };
            if ad.index == 0 {
                ad.index = a.Ipv6IfIndex;
            }
            let mut u = a.FirstUnicastAddress;
            while !u.is_null() {
                match sockaddr(&(*u).Address) {
                    Some(IpAddr::V4(v)) => ad.v4.push((v, (*u).OnLinkPrefixLength)),
                    Some(IpAddr::V6(v)) if (v.segments()[0] & 0xe000) == 0x2000 => ad.v6.push(v),
                    _ => {}
                }
                u = (*u).Next;
            }
            let mut g = a.FirstGatewayAddress;
            while !g.is_null() {
                if let Some(IpAddr::V4(v)) = sockaddr(&(*g).Address) {
                    ad.gateways.push(v);
                }
                g = (*g).Next;
            }
            let mut d = a.FirstDnsServerAddress;
            while !d.is_null() {
                if let Some(ip) = sockaddr(&(*d).Address) {
                    // fec0:0:0:ffff::1 and friends: old placeholders, not real servers.
                    let placeholder = matches!(ip, IpAddr::V6(v) if v.segments()[0] == 0xfec0);
                    if !placeholder {
                        ad.dns.push(ip);
                    }
                }
                d = (*d).Next;
            }
            out.push(ad);
            p = a.Next;
        }
    }
    out
}

/// An interface's IPv4 settings: its metric and whether Windows picks it.
pub fn ip_interface(luid: u64) -> Option<(u32, bool)> {
    unsafe {
        let mut row: MIB_IPINTERFACE_ROW = std::mem::zeroed();
        row.Family = AF_INET;
        row.InterfaceLuid = NET_LUID_LH { Value: luid };
        (GetIpInterfaceEntry(&mut row) == NO_ERROR).then_some((row.Metric, row.UseAutomaticMetric))
    }
}

/// A default route: its adapter, its router (`None` when there's none on
/// it, as with many VPNs), and its metric with the adapter's added.
#[derive(Clone, Debug)]
pub struct Route {
    pub luid: u64,
    pub router: Option<Ipv4Addr>,
    pub metric: u32,
}

/// The IPv4 default routes, best first.
pub fn default_routes() -> Vec<Route> {
    let mut out = Vec::new();
    unsafe {
        let mut t: *mut MIB_IPFORWARD_TABLE2 = null_mut();
        if GetIpForwardTable2(AF_INET, &mut t) != NO_ERROR || t.is_null() {
            return out;
        }
        let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), (*t).NumEntries as usize);
        for r in rows.iter().filter(|r| r.DestinationPrefix.PrefixLength == 0) {
            let router = match inet(&r.NextHop) {
                Some(IpAddr::V4(v)) if !v.is_unspecified() => Some(v),
                _ => None,
            };
            let base = ip_interface(r.InterfaceLuid.Value).map_or(0, |m| m.0);
            out.push(Route { luid: r.InterfaceLuid.Value, router, metric: r.Metric.saturating_add(base) });
        }
        FreeMibTable(t as *const c_void);
    }
    out.sort_by_key(|r| r.metric);
    out
}

/// Neighbours that answered, from the address table: (address, hardware
/// address, interface index).
pub fn neighbours() -> Vec<(Ipv4Addr, String, u32)> {
    let mut out = Vec::new();
    unsafe {
        let mut t: *mut MIB_IPNET_TABLE2 = null_mut();
        if GetIpNetTable2(AF_INET, &mut t) != NO_ERROR || t.is_null() {
            return out;
        }
        let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), (*t).NumEntries as usize);
        for r in rows {
            // Probe, Delay, Stale, Reachable or Permanent: it answered.
            let answered = r.State >= 2;
            let len = (r.PhysicalAddressLength as usize).min(32);
            let mac = &r.PhysicalAddress[..len];
            if !answered || len != 6 || mac.iter().all(|&b| b == 0) || mac.iter().all(|&b| b == 0xff) {
                continue;
            }
            if let Some(IpAddr::V4(ip)) = inet(&r.Address)
                && !ip.is_multicast()
                && !ip.is_broadcast()
            {
                out.push((ip, mac_string(mac), r.InterfaceIndex));
            }
        }
        FreeMibTable(t as *const c_void);
    }
    out
}

/// Bytes in and out on every real adapter.
pub fn byte_totals() -> Option<(u64, u64)> {
    unsafe {
        let mut t: *mut MIB_IF_TABLE2 = null_mut();
        if GetIfTable2(&mut t) != NO_ERROR || t.is_null() {
            return None;
        }
        let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), (*t).NumEntries as usize);
        let (mut rx, mut tx) = (0u64, 0u64);
        for r in rows {
            let flags = r.InterfaceAndOperStatusFlags._bitfield;
            if flags & 1 != 0 && flags & 2 == 0 && flags & 4 != 0 {
                rx += r.InOctets;
                tx += r.OutOctets;
            }
        }
        FreeMibTable(t as *const c_void);
        Some((rx, tx))
    }
}

// -- Connections -------------------------------------------------------------

/// A socket from the TCP or UDP table: (tcp?, local, remote, listening, pid).
#[derive(Clone, Debug)]
pub struct Sock {
    pub tcp: bool,
    pub local: std::net::SocketAddr,
    pub remote: Option<std::net::SocketAddr>,
    pub listening: bool,
    pub pid: u32,
}

fn table(get: impl Fn(*mut c_void, *mut u32) -> u32) -> Vec<u64> {
    let mut size = 64 * 1024u32;
    for _ in 0..5 {
        let mut buf = vec![0u64; size as usize / 8 + 1];
        match get(buf.as_mut_ptr() as *mut c_void, &mut size) {
            NO_ERROR => return buf,
            ERROR_INSUFFICIENT_BUFFER => size += 16 * 1024,
            _ => return Vec::new(),
        }
    }
    Vec::new()
}

fn port(p: u32) -> u16 {
    u16::from_be(p as u16)
}

/// Every TCP and UDP socket, with the program holding it.
pub fn sockets() -> Vec<Sock> {
    use std::net::SocketAddr;
    let mut out = Vec::new();
    unsafe {
        let b = table(|p, s| GetExtendedTcpTable(p, s, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_ALL, 0));
        if !b.is_empty() {
            let t = &*(b.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
            for r in std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize) {
                let local = SocketAddr::from((Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes()), port(r.dwLocalPort)));
                let remote = SocketAddr::from((Ipv4Addr::from(r.dwRemoteAddr.to_ne_bytes()), port(r.dwRemotePort)));
                let state = r.dwState as i32;
                if state == MIB_TCP_STATE_ESTAB || state == MIB_TCP_STATE_LISTEN {
                    let listening = state == MIB_TCP_STATE_LISTEN;
                    out.push(Sock { tcp: true, local, remote: (!listening).then_some(remote), listening, pid: r.dwOwningPid });
                }
            }
        }
        let b = table(|p, s| GetExtendedTcpTable(p, s, 0, AF_INET6 as u32, TCP_TABLE_OWNER_PID_ALL, 0));
        if !b.is_empty() {
            let t = &*(b.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
            for r in std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize) {
                let v6 = |b: [u8; 16]| {
                    let v = Ipv6Addr::from(b);
                    v.to_ipv4_mapped().map_or(IpAddr::V6(v), IpAddr::V4)
                };
                let local = SocketAddr::new(v6(r.ucLocalAddr), port(r.dwLocalPort));
                let remote = SocketAddr::new(v6(r.ucRemoteAddr), port(r.dwRemotePort));
                let state = r.dwState as i32;
                if state == MIB_TCP_STATE_ESTAB || state == MIB_TCP_STATE_LISTEN {
                    let listening = state == MIB_TCP_STATE_LISTEN;
                    out.push(Sock { tcp: true, local, remote: (!listening).then_some(remote), listening, pid: r.dwOwningPid });
                }
            }
        }
        let b = table(|p, s| GetExtendedUdpTable(p, s, 0, AF_INET as u32, UDP_TABLE_OWNER_PID, 0));
        if !b.is_empty() {
            let t = &*(b.as_ptr() as *const MIB_UDPTABLE_OWNER_PID);
            for r in std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize) {
                let local = SocketAddr::from((Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes()), port(r.dwLocalPort)));
                out.push(Sock { tcp: false, local, remote: None, listening: true, pid: r.dwOwningPid });
            }
        }
        let b = table(|p, s| GetExtendedUdpTable(p, s, 0, AF_INET6 as u32, UDP_TABLE_OWNER_PID, 0));
        if !b.is_empty() {
            let t = &*(b.as_ptr() as *const MIB_UDP6TABLE_OWNER_PID);
            for r in std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize) {
                let local = SocketAddr::new(IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)), port(r.dwLocalPort));
                out.push(Sock { tcp: false, local, remote: None, listening: true, pid: r.dwOwningPid });
            }
        }
    }
    out
}

/// Every running program's file name ("chrome.exe"), by process id.
pub fn process_names() -> HashMap<u32, String> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    let mut out = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            out.insert(e.th32ProcessID, from_wide(&e.szExeFile));
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    out
}

// -- Ping --------------------------------------------------------------------

/// What came back from one echo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Echo {
    Reply(Duration),
    /// A router on the way said the message ran out of hops.
    Hop(Ipv4Addr),
    /// Too big to pass without being split, and not allowed to be.
    TooBig,
    Nothing,
}

/// One ICMP echo, as `ping` sends: `ttl` hops at most, `size` bytes of
/// data, `df` for "don't split it". Needs no admin rights on Windows.
pub fn echo(to: Ipv4Addr, ttl: u8, size: u16, df: bool, timeout: Duration) -> Echo {
    unsafe {
        let h = IcmpCreateFile();
        if h == INVALID_HANDLE_VALUE {
            return Echo::Nothing;
        }
        let data = vec![0x61u8; size as usize];
        let opts = IP_OPTION_INFORMATION { Ttl: ttl, Tos: 0, Flags: if df { IP_FLAG_DF as u8 } else { 0 }, OptionsSize: 0, OptionsData: null_mut() };
        let mut reply = vec![0u64; (std::mem::size_of::<ICMP_ECHO_REPLY>() + size as usize + 64) / 8 + 1];
        let start = Instant::now();
        let n = IcmpSendEcho(
            h,
            u32::from_ne_bytes(to.octets()),
            data.as_ptr() as *const c_void,
            size,
            &opts,
            reply.as_mut_ptr() as *mut c_void,
            (reply.len() * 8) as u32,
            timeout.as_millis() as u32,
        );
        let took = start.elapsed();
        IcmpCloseHandle(h);
        let r = &*(reply.as_ptr() as *const ICMP_ECHO_REPLY);
        let from = Ipv4Addr::from(r.Address.to_ne_bytes());
        if n == 0 && r.Address == 0 {
            return Echo::Nothing;
        }
        match r.Status {
            IP_SUCCESS if from == to => Echo::Reply(took),
            IP_TTL_EXPIRED_TRANSIT => Echo::Hop(from),
            IP_PACKET_TOO_BIG => Echo::TooBig,
            _ => Echo::Nothing,
        }
    }
}

// -- Wi-Fi -------------------------------------------------------------------

/// A WLAN API session.
pub struct Wlan(HANDLE);

impl Drop for Wlan {
    fn drop(&mut self) {
        unsafe { WlanCloseHandle(self.0, null()) };
    }
}

/// A Wi-Fi network in range.
#[derive(Clone, Debug)]
pub struct Seen {
    pub ssid: String,
    /// "WPA2", "WPA3", "WPA2 802.1X", "" for none.
    pub security: String,
    /// 0 to 100.
    pub signal: u8,
    pub connected: bool,
    /// The saved profile it would use.
    pub profile: Option<String>,
    /// MHz.
    pub freq: u32,
}

/// The connection an adapter is on now.
#[derive(Clone, Debug)]
pub struct Current {
    pub ssid: String,
    pub profile: String,
    pub signal: u8,
}

fn ssid_string(s: &DOT11_SSID) -> String {
    String::from_utf8_lossy(&s.ucSSID[..(s.uSSIDLength as usize).min(32)]).into_owned()
}

fn security_word(auth: i32, secure: bool) -> String {
    match auth {
        DOT11_AUTH_ALGO_80211_OPEN if secure => "WEP".into(),
        DOT11_AUTH_ALGO_80211_OPEN => String::new(),
        DOT11_AUTH_ALGO_80211_SHARED_KEY => "WEP".into(),
        DOT11_AUTH_ALGO_WPA => "WPA1 802.1X".into(),
        DOT11_AUTH_ALGO_WPA_PSK => "WPA1".into(),
        DOT11_AUTH_ALGO_RSNA => "WPA2 802.1X".into(),
        DOT11_AUTH_ALGO_RSNA_PSK => "WPA2".into(),
        DOT11_AUTH_ALGO_WPA3 | DOT11_AUTH_ALGO_WPA3_ENT => "WPA3 802.1X".into(),
        DOT11_AUTH_ALGO_WPA3_SAE => "WPA3".into(),
        DOT11_AUTH_ALGO_OWE => "OWE".into(),
        _ => "WPA2".into(),
    }
}

fn wlan_error(code: u32) -> String {
    match code {
        ERROR_ACCESS_DENIED => "Windows only lets programs see Wi-Fi networks with location turned on: \
                                Settings › Privacy & security › Location, and \"Let desktop apps access your location\""
            .into(),
        ERROR_SERVICE_NOT_ACTIVE | ERROR_SERVICE_DOES_NOT_EXIST => "the WLAN AutoConfig service isn't running".into(),
        ERROR_NOT_FOUND => "the Wi-Fi adapter wasn't found".into(),
        ERROR_INVALID_STATE => "the Wi-Fi adapter isn't ready (is it switched off?)".into(),
        e => std::io::Error::from_raw_os_error(e as i32).to_string(),
    }
}

impl Wlan {
    pub fn open() -> Option<Self> {
        let (mut version, mut h) = (0u32, null_mut());
        (unsafe { WlanOpenHandle(2, null(), &mut version, &mut h) } == 0).then_some(Wlan(h))
    }

    /// The Wi-Fi adapters: GUID and description.
    pub fn interfaces(&self) -> Vec<(GUID, String)> {
        let mut out = Vec::new();
        unsafe {
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = null_mut();
            if WlanEnumInterfaces(self.0, null(), &mut list) != 0 || list.is_null() {
                return out;
            }
            for i in std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), (*list).dwNumberOfItems as usize) {
                out.push((i.InterfaceGuid, from_wide(&i.strInterfaceDescription)));
            }
            WlanFreeMemory(list as *const c_void);
        }
        out
    }

    /// The WLAN GUID of the adapter whose IP Helper name is `guid` ("{…}").
    pub fn find(&self, guid: &str) -> Option<GUID> {
        self.interfaces().into_iter().map(|i| i.0).find(|g| guid_string(g).eq_ignore_ascii_case(guid))
    }

    fn query<T: Copy>(&self, guid: &GUID, op: i32) -> Option<T> {
        unsafe {
            let (mut size, mut data, mut kind) = (0u32, null_mut(), 0);
            if WlanQueryInterface(self.0, guid, op, null(), &mut size, &mut data, &mut kind) != 0 || data.is_null() {
                return None;
            }
            let v = (size as usize >= std::mem::size_of::<T>()).then(|| *(data as *const T));
            WlanFreeMemory(data);
            v
        }
    }

    pub fn current(&self, guid: &GUID) -> Option<Current> {
        let c: WLAN_CONNECTION_ATTRIBUTES = self.query(guid, wlan_intf_opcode_current_connection)?;
        (c.isState == wlan_interface_state_connected).then(|| Current {
            ssid: ssid_string(&c.wlanAssociationAttributes.dot11Ssid),
            profile: from_wide(&c.strProfileName),
            signal: c.wlanAssociationAttributes.wlanSignalQuality.min(100) as u8,
        })
    }

    /// The signal in dBm.
    pub fn rssi(&self, guid: &GUID) -> Option<i32> {
        self.query::<i32>(guid, wlan_intf_opcode_rssi).filter(|&v| v < 0)
    }

    /// Switched off in software (airplane mode) or by a switch.
    pub fn radio_off(&self, guid: &GUID) -> bool {
        self.query::<WLAN_RADIO_STATE>(guid, wlan_intf_opcode_radio_state).is_some_and(|r| {
            let phys = &r.PhyRadioState[..(r.dwNumberOfPhys as usize).min(64)];
            !phys.is_empty() && phys.iter().all(|p| p.dot11SoftwareRadioState == dot11_radio_state_off || p.dot11HardwareRadioState == dot11_radio_state_off)
        })
    }

    /// The networks in range, one per name; `rescan` looks again first
    /// (about 4 seconds).
    pub fn networks(&self, guid: &GUID, rescan: bool) -> Result<Vec<Seen>, String> {
        unsafe {
            if rescan && WlanScan(self.0, guid, null(), null(), null()) == 0 {
                std::thread::sleep(Duration::from_secs(4));
            }
            // Frequencies, from the access points themselves.
            let mut freq: HashMap<String, (i32, u32)> = HashMap::new();
            let mut bss: *mut WLAN_BSS_LIST = null_mut();
            if WlanGetNetworkBssList(self.0, guid, null(), dot11_BSS_type_any, 0, null(), &mut bss) == 0 && !bss.is_null() {
                for b in std::slice::from_raw_parts((*bss).wlanBssEntries.as_ptr(), (*bss).dwNumberOfItems as usize) {
                    let e = freq.entry(ssid_string(&b.dot11Ssid)).or_insert((i32::MIN, 0));
                    if b.lRssi > e.0 {
                        *e = (b.lRssi, b.ulChCenterFrequency / 1000);
                    }
                }
                WlanFreeMemory(bss as *const c_void);
            }
            let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = null_mut();
            let r = WlanGetAvailableNetworkList(self.0, guid, 0, null(), &mut list);
            if r != 0 || list.is_null() {
                return Err(wlan_error(r));
            }
            let mut out: Vec<Seen> = Vec::new();
            for n in std::slice::from_raw_parts((*list).Network.as_ptr(), (*list).dwNumberOfItems as usize) {
                let ssid = ssid_string(&n.dot11Ssid);
                if ssid.is_empty() {
                    continue;
                }
                let profile = from_wide(&n.strProfileName);
                let seen = Seen {
                    security: security_word(n.dot11DefaultAuthAlgorithm, n.bSecurityEnabled != 0),
                    signal: n.wlanSignalQuality.min(100) as u8,
                    connected: n.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                    profile: (!profile.is_empty()).then_some(profile),
                    freq: freq.get(&ssid).map_or(0, |f| f.1),
                    ssid,
                };
                // Windows lists a network once per saved profile, and once
                // without one: keep the one in use, else the one with a profile.
                match out.iter_mut().find(|o| o.ssid == seen.ssid) {
                    Some(o) if seen.connected || (!o.connected && o.profile.is_none() && seen.profile.is_some()) => {
                        let signal = o.signal.max(seen.signal);
                        *o = seen;
                        o.signal = signal;
                    }
                    Some(o) => o.signal = o.signal.max(seen.signal),
                    None => out.push(seen),
                }
            }
            WlanFreeMemory(list as *const c_void);
            Ok(out)
        }
    }

    /// The names of the saved networks (profiles) on an adapter.
    pub fn profiles(&self, guid: &GUID) -> Vec<String> {
        let mut out = Vec::new();
        unsafe {
            let mut list: *mut WLAN_PROFILE_INFO_LIST = null_mut();
            if WlanGetProfileList(self.0, guid, null(), &mut list) != 0 || list.is_null() {
                return out;
            }
            for p in std::slice::from_raw_parts((*list).ProfileInfo.as_ptr(), (*list).dwNumberOfItems as usize) {
                out.push(from_wide(&p.strProfileName));
            }
            WlanFreeMemory(list as *const c_void);
        }
        out
    }

    /// A saved profile's XML (with its key still protected).
    pub fn profile_xml(&self, guid: &GUID, name: &str) -> Option<String> {
        unsafe {
            let (mut xml, mut flags, mut access) = (null_mut(), 0u32, 0u32);
            let w = wide(name);
            if WlanGetProfile(self.0, guid, w.as_ptr(), null(), &mut xml, &mut flags, &mut access) != 0 || xml.is_null() {
                return None;
            }
            let s = from_pwstr(xml);
            WlanFreeMemory(xml as *const c_void);
            Some(s)
        }
    }

    /// Save a profile from its XML; for this user only, so no admin rights
    /// are needed.
    pub fn set_profile(&self, guid: &GUID, xml: &str, user: bool) -> Result<(), String> {
        unsafe {
            let mut reason = 0u32;
            let w = wide(xml);
            match WlanSetProfile(self.0, guid, if user { WLAN_PROFILE_USER } else { 0 }, w.as_ptr(), null(), 1, null(), &mut reason) {
                0 => Ok(()),
                ERROR_BAD_PROFILE => Err(format!("Windows didn't accept the network's settings (reason {reason})")),
                e => Err(wlan_error(e)),
            }
        }
    }

    pub fn delete_profile(&self, guid: &GUID, name: &str) -> Result<(), String> {
        let w = wide(name);
        match unsafe { WlanDeleteProfile(self.0, guid, w.as_ptr(), null()) } {
            0 => Ok(()),
            e => Err(wlan_error(e)),
        }
    }

    pub fn disconnect(&self, guid: &GUID) -> Result<(), String> {
        match unsafe { WlanDisconnect(self.0, guid, null()) } {
            0 => Ok(()),
            e => Err(wlan_error(e)),
        }
    }

    /// Connect with a saved profile and wait up to `wait` for it to be
    /// connected.
    pub fn connect(&self, guid: &GUID, profile: &str, wait: Duration) -> Result<(), String> {
        let w = wide(profile);
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: w.as_ptr(),
            pDot11Ssid: null_mut(),
            pDesiredBssidList: null_mut(),
            dot11BssType: dot11_BSS_type_any,
            dwFlags: 0,
        };
        let r = unsafe { WlanConnect(self.0, guid, &params, null()) };
        if r != 0 {
            return Err(wlan_error(r));
        }
        let start = Instant::now();
        // Give it a moment to drop the old connection first.
        std::thread::sleep(Duration::from_millis(800));
        while start.elapsed() < wait {
            if self.current(guid).is_some_and(|c| c.profile == profile) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        Err(format!("couldn't connect to {profile} (wrong password, or out of range?)"))
    }
}

// -- The registry ------------------------------------------------------------

use windows_sys::Win32::System::Registry::*;

pub fn reg_dword(root: HKEY, key: &str, value: &str) -> Option<u32> {
    let (k, v) = (wide(key), wide(value));
    let (mut data, mut size) = (0u32, 4u32);
    let r = unsafe { RegGetValueW(root, k.as_ptr(), v.as_ptr(), RRF_RT_REG_DWORD, null_mut(), &mut data as *mut u32 as *mut c_void, &mut size) };
    (r == ERROR_SUCCESS).then_some(data)
}

pub fn reg_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let (k, v) = (wide(key), wide(value));
    let mut size = 0u32;
    unsafe {
        if RegGetValueW(root, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, null_mut(), null_mut(), &mut size) != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u16; size as usize / 2 + 1];
        let r = RegGetValueW(root, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, null_mut(), buf.as_mut_ptr() as *mut c_void, &mut size);
        (r == ERROR_SUCCESS).then(|| from_wide(&buf))
    }
}

/// The names of a key's subkeys.
pub fn reg_subkeys(root: HKEY, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let mut h: HKEY = null_mut();
        let k = wide(key);
        if RegOpenKeyExW(root, k.as_ptr(), 0, KEY_READ, &mut h) != ERROR_SUCCESS {
            return out;
        }
        for i in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(h, i, name.as_mut_ptr(), &mut len, null(), null_mut(), null_mut(), null_mut()) != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        RegCloseKey(h);
    }
    out
}

pub fn reg_set_string(root: HKEY, key: &str, value: &str, data: &str) -> std::io::Result<()> {
    let (k, v, d) = (wide(key), wide(value), wide(data));
    let r = unsafe { RegSetKeyValueW(root, k.as_ptr(), v.as_ptr(), REG_SZ, d.as_ptr() as *const c_void, (d.len() * 2) as u32) };
    if r == ERROR_SUCCESS { Ok(()) } else { Err(std::io::Error::from_raw_os_error(r as i32)) }
}

pub fn reg_delete(root: HKEY, key: &str, value: &str) -> std::io::Result<()> {
    let (k, v) = (wide(key), wide(value));
    match unsafe { RegDeleteKeyValueW(root, k.as_ptr(), v.as_ptr()) } {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        r => Err(std::io::Error::from_raw_os_error(r as i32)),
    }
}

// -- Folders, files and the shell ---------------------------------------------

/// Open a file, folder or web address with what Windows uses for it.
pub fn open(target: &std::ffi::OsStr) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let t: Vec<u16> = target.encode_wide().chain(std::iter::once(0)).collect();
    let verb = wide("open");
    unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), t.as_ptr(), null(), null(), SW_SHOWNORMAL) };
}

/// Whether this program runs with admin rights.
pub fn elevated() -> bool {
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

/// Run a PowerShell script with admin rights; Windows asks the user first
/// (unless Net Watch already has them). The script's error, if it fails.
pub fn admin_powershell(script: &str) -> Result<(), String> {
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let dir = crate::store::data_dir().ok_or("no data folder")?;
    let _ = std::fs::create_dir_all(&dir);
    let stamp = format!("{}-{}", std::process::id(), Instant::now().elapsed().as_nanos() ^ chrono::Local::now().timestamp_micros() as u128);
    let (ps1, err) = (dir.join(format!("change-{stamp}.ps1")), dir.join(format!("change-{stamp}.err")));
    let quoted = err.display().to_string().replace('\'', "''");
    let body = format!(
        "$ErrorActionPreference = 'Stop'\r\n$ProgressPreference = 'SilentlyContinue'\r\ntry {{\r\n{script}\r\n}} catch {{\r\n  $_.Exception.Message | Out-File -Encoding utf8 -FilePath '{quoted}'\r\n  exit 1\r\n}}\r\nexit 0\r\n"
    );
    // With a byte-order mark, so PowerShell reads names with accents right.
    std::fs::write(&ps1, [&[0xEF, 0xBB, 0xBF][..], body.as_bytes()].concat()).map_err(|e| e.to_string())?;
    let args = format!("-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\"", ps1.display());
    let result = if elevated() {
        command("powershell.exe").args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]).arg(&ps1).status().map(|s| s.code().unwrap_or(1) as u32).map_err(|e| e.to_string())
    } else {
        let (verb, file, params) = (wide("runas"), wide("powershell.exe"), wide(&args));
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;
        unsafe {
            if ShellExecuteExW(&mut info) == 0 {
                let e = GetLastError();
                let _ = std::fs::remove_file(&ps1);
                return Err(if e == ERROR_CANCELLED { "Windows asked for permission to change it, and it wasn't given".into() } else { std::io::Error::from_raw_os_error(e as i32).to_string() });
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hProcess);
            Ok(code)
        }
    };
    let _ = std::fs::remove_file(&ps1);
    let message = std::fs::read(&err).ok().map(|b| String::from_utf8_lossy(&b).trim_start_matches('\u{feff}').trim().to_string());
    let _ = std::fs::remove_file(&err);
    match result? {
        0 => Ok(()),
        _ => Err(message.filter(|m| !m.is_empty()).unwrap_or_else(|| "Windows couldn't make the change".into())),
    }
}

/// Quote a value for a PowerShell script: 'it''s'.
pub fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

// -- The window --------------------------------------------------------------

use std::sync::atomic::{AtomicIsize, Ordering};
static MAIN: AtomicIsize = AtomicIsize::new(0);

/// Remember the main window, to show and hide it from the tray.
pub fn set_main_window(hwnd: isize) {
    MAIN.store(hwnd, Ordering::Relaxed);
}

/// Hide the main window (it keeps running, in the tray).
pub fn hide_main() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    let h = MAIN.load(Ordering::Relaxed);
    if h != 0 {
        unsafe { ShowWindow(h as HWND, SW_HIDE) };
    }
}

/// Bring the main window back to the front.
pub fn show_main() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow};
    let h = MAIN.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            ShowWindow(h as HWND, if IsIconic(h as HWND) != 0 { SW_RESTORE } else { SW_SHOW });
            SetForegroundWindow(h as HWND);
        }
    }
}

/// Command-line use (`net-watch --probe` and so on) from a terminal: this
/// is a windowed program, so it borrows the terminal's console to print.
pub fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}
