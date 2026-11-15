# Net Watch

A small desktop app that sits in the tray, keeps an eye on your internet
connection, and keeps a record you can show your provider when it "keeps
dropping": every outage with when it started, how long it lasted and whether
your own router was still answering, plus regular speed tests.

A Linux network diagnostic tool: Rust, eframe/egui 0.36, a ksni tray and an
AppImage. It reads Linux's own network files (`/proc/net`, `/sys/class/net`)
and asks NetworkManager, so it runs on Linux only.

## What you get

- **Tray icon** that is green when online, amber when some checks fail, red
  when the internet is down. Its menu shows the status, the last speed test
  and the last outage, and can run a speed test, pause checks for an hour or
  start Net Watch when you log in.
- **Pop-ups** when the internet drops and when it's back, with how long it
  was gone (can be turned off).
- **Window** that opens on Home: the status with the latency and a small
  line of recent latency, a "Check now" button, and a card for each tool
  (Connection, Speed test, Firewall check, Devices, Network, Settings) saying how
  things stand, with a small picture: the last 24 hours, the speed against
  your plan, each kind of traffic tried, the kinds of device found. The
  Speed test card's button runs a test straight away. Clicking a card opens
  that tool; "All tools", Esc, Alt+Left or the mouse's back button goes
  back. Keys 1–6 open the tools from Home, and F5 checks now. Pointing at a
  card's picture tells you more: an hour, a kind of traffic, which devices.
  Pages fade in and the cards come in one by one, unless Animations is off. Connection has uptime, outage count, last speed and latency, an
  hour-by-hour grid (green, amber, red), the latency graph and the outage
  log; Speed test has the live test, the speed graph and every test. 24
  hours, 7 days or 30 days. The layout stacks in a narrow window.
- **What's wrong?**, when you ask for it (or from Home's "What's wrong?"
  when the connection is down or shaky): checks the connection in order,
  from this computer out (cable or Wi-Fi and its signal, an address from
  the router, the router answering, the internet, names (DNS), websites
  and sign-in pages, then IPv6 and full-size packets), stops at the first
  step that fails and says in plain words what it means and what to try.
  "Show details" has the numbers; "Copy report" puts it all on the
  clipboard for your provider. `net-watch --diagnose` prints it.
- **Where's the lag?**, when you ask for it: follows the way to Cloudflare,
  Google or a website you type, hop by hop, times each hop (10 pings) and
  splits the delay into your home network, your provider (named, like
  "AT&T Enterprises") and beyond, and finds where lost messages start.
  "Copy report" gives your provider every hop. `net-watch --route [site]`
  prints it.
- **Firewall check**, when you ask for it: whether this computer's firewall
  is on, and which kinds of traffic get out (websites, name lookups, the
  clock, video calls, email, remote login, IPv6), each tried against a
  well-known server. About 5 seconds and almost no data.
- **Device scan**, when you ask for it: what's connected to your home
  network, with names, makers and a guess at what each one is (Roku,
  printer, iPhone, camera…). Devices first seen in the last day are marked
  New. About 4 seconds; nothing leaves your network.
- **Watch for new devices** (Settings, off by default): scans every 5, 15,
  30 or 60 minutes while online, with a pop-up naming anything that joins
  for the first time. Only on the network you're on when you turn it on
  (told apart by the router's hardware address); elsewhere it waits. Phones that change their made-up hardware address are
  recognised by name, so they don't set it off.
- **Network adapters** (through NetworkManager): Home shows the one
  the internet goes through, Ethernet or Wi-Fi, with its name (like
  `enp6s0`) and link speed or Wi-Fi network and signal; its gear opens the
  Network screen. There, pick an adapter at the top to turn it on or off
  and change its settings: automatic or fixed address, DNS (the router's,
  Cloudflare, Google, Quad9 or your own), connecting on its own, and
  preferring it when cable and Wi-Fi are both connected. For Wi-Fi,
  choose a network from a list of those in range (signal, security,
  band), with a password for new ones, forget saved ones, or join a hidden
  one. The password goes to NetworkManager through its editor's input, so
  it never shows in the list of running programs. After a change to the
  adapter in use, Net Watch checks the internet; if there's no answer
  within 15 s, 30 s, 1 min or 2 min (your choice), the old settings come
  back on their own. Keep or Undo now end the wait early.
- **Export report**: a plain-text summary of the period with every outage
  and speed test (and the last firewall check; never the device list), saved to Downloads, ready to
  send to your provider.

- **Looks**: Clean light (the default) and Mica dark, after a mockup in
  the style of macOS and Windows 11, plus Roku Remote's dark themes
  (Violet, Ocean, Forest, Sunset, Rose, Graphite). Settings › Theme.

## How it checks

**Is the internet up?** Every 30 seconds (adjustable), a few tiny checks at
once, well under 1 KB in all:

| Check | Why |
|---|---|
| Knock on the router (TCP to the gateway) | Tells "Wi-Fi/router problem" apart from "provider problem" |
| TCP handshake to 1.1.1.1, 8.8.8.8, 9.9.9.9 on port 443 | Three different companies, so one of them having a bad day isn't an outage. The fastest handshake is the latency. |
| One DNS question to each of them | Catches "connected, but names don't resolve" |
| `http://cp.cloudflare.com/generate_204` | Catches a login page or broken proxy in the way |

It counts as an outage when every outside check fails twice in a row; the
outage starts at the first failed check and ends at the first one that works.
While the internet is down it checks every 5 seconds, so drops are timed
closely; when only some checks fail, for 6 rounds. With no router to knock
on (a VPN or a phone's tether) an outage isn't put down to the provider. Time the computer spent asleep isn't counted as an outage. No admin
rights are needed (no ping).

**How fast is it?** Every 4 hours by default, at a slightly random minute and
only while online, a speed test, shown live in the window while it runs: the
speed both ways four times a second on a graph, the result so far, ping, data
used and the server. The tray menu shows the speed too. Every result is saved
with the server that measured it.

They're run by Ookla's official, free Speedtest CLI; Net Watch reads its
progress as it goes. In Settings you read and accept Ookla's license, terms
and privacy policy, and Net Watch then downloads the program from
`install.speedtest.net` into its data folder (one already installed on the
system is used too). Until then there are no speed tests; connection checks
carry on. The test picks a server near you, often your provider's own,
takes about 30–40 seconds, and each result gets a page on speedtest.net; the
link is saved and goes in the report. Ookla's license allows personal,
non-commercial use, and results are sent to Ookla with your IP address.

A test that hangs is stopped after 2 minutes. A failed test shows as a red mark on the graph and is tried again 15 minutes
later.

**What gets out?** The firewall check opens a connection (or, for UDP,
sends one question) to Cloudflare, Google or GitHub on each port: HTTPS 443,
HTTP 80, DNS 53 over UDP and TCP, DNS over TLS 853, NTP 123, STUN 19302,
IMAP 993, SMTP 587, SSH 22, SMTP 25, and HTTPS over IPv6. No answer means
something in between quietly drops it; a "no" means something turns it
away. Port 25 and IPv6 being blocked are shown as normal. For this
computer's firewall it reads ufw's settings or asks systemd about firewalld
and nftables; no admin rights needed.

**What's on the network?** The device scan sends one tiny UDP packet to
every address on the home network (up to 1024 of them), then reads the
computer's own address table (`/proc/net/arp`) to
see which answered. It asks those for their names three ways: the router
(which often knows the names people gave their devices), the device itself
over mDNS, and the Windows way (NetBIOS). It then knocks on a few telling
ports (8060 Roku, 8009 Chromecast, 62078 iPhone, 9100 printer, 554 camera
and so on). Makers come from the IEEE list when the system has one
(`ieee-data` on Debian and Ubuntu). Phones and laptops often use a made-up
hardware address, shown as "Private address". Every device seen is kept in
`devices-<router>.csv`, one list per network.

**Where's the lag?** It runs `ping` with a limit on how many hops the
message may go (`-t 1`, `-t 2`, …): each router where it runs out says so,
which gives the way out, like traceroute but without admin rights. Each hop
then gets 10 pings. Hop names come from this computer's own name server;
whose network each hop is in comes from Team Cymru's public lookup
(`origin.asn.cymru.com`), asked the same way, so only router addresses
leave the computer. What's typed is cut down to the site's name (letters,
digits, dots and dashes) and looked up first; `ping` only ever gets an
address.

### Why Ookla

Providers know and trust speedtest.net results, the servers are close by
(often the provider's own), and the CLI is built for fast connections.
Cloudflare's free test server was tried first (`speed.cloudflare.com`),
but it turns clients away after a lot of data in a short time. M-Lab's NDT
was ruled out because it asks automated clients to run at most 4 tests a day
and publishes every result with the home IP address.

## Your data

Everything stays on this computer, as CSV files you can open in a
spreadsheet: `~/.local/share/net-watch` (Settings has an "Open folder"
button).

| File | What's in it |
|---|---|
| `checks-YYYY-MM.csv` | Every check: time, latency, how many of the 7 worked |
| `outages.csv` | Every outage: start, end, seconds, cause, details |
| `speed.csv` | Every speed test |
| `outage-now.csv` | An outage still going on (so it survives a restart) |
| `devices-<router>.csv` | Every device a scan has found on that network, with when it was first and last seen |

A month of checks is about 3 MB.

## Build and run

```bash
cargo run --release
```

```bash
cargo run --release -- --probe        # one check and one speed test, printed
```

```bash
cargo run --release -- --scan         # one device scan, printed
```

```bash
cargo run --release -- --diagnose     # the "What's wrong?" check, printed
```

```bash
cargo run --release -- --route example.com   # the route, hop by hop, printed
```

```bash
bash packaging/build_appimage.sh      # Net-Watch-x86_64.AppImage
```

Dev aids: `NET_WATCH_DATA=dir` keeps data and settings in `dir`;
`NET_WATCH_SIZE=1000x700`, `NET_WATCH_SCREENSHOT=out.ppm`,
`NET_WATCH_SCROLL=400`, `NET_WATCH_SHOT_AT=20` (seconds to wait), `NET_WATCH_RANGE=0..2` and `NET_WATCH_SETTINGS=1`
help take screenshots;
`NET_WATCH_TOOL=devices` opens a tool by name, and `NET_WATCH_PICK=1` the
Wi-Fi list on Network;
`NET_WATCH_FIREWALL=1`, `NET_WATCH_SCAN=1` and `NET_WATCH_DIAGNOSE=1` run
the firewall check, the device scan and "What's wrong?" on start, and
`NET_WATCH_ROUTE=Cloudflare` (or a site) traces the route
(`NET_WATCH_DETAILS=1` with its details showing);
`NET_WATCH_TEST_LIMIT=secs` shortens the 2-minute limit on a speed test;
`NET_WATCH_FAKE_SPEED=1` makes speed tests up without Speedtest or the network; `NET_WATCH_TEST_SOON=1` runs the first speed test a few seconds after
starting; a file named `simulate-down` in the data folder fakes
an outage.

Each tool's screen is its own file in `src/tools/`; a new tool is one more
file there and one more line in `tools::all()`.

`packaging/licenses.sh` regenerates `THIRD-PARTY-LICENSES.md`.

Net Watch is not made by or affiliated with Ookla, Cloudflare, Google, Quad9 or GitHub.
