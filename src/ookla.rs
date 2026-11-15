//! Speed tests through Ookla's official Speedtest CLI: Net Watch runs the
//! program and reads its progress as it goes. Ookla's license allows
//! personal, non-commercial use; the user accepts it in Settings, and only
//! then is the program downloaded (from Ookla) and run.
use crate::probe::{Live, Speed};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const LICENSE_URL: &str = "https://www.speedtest.net/about/eula";
pub const TERMS_URL: &str = "https://www.speedtest.net/about/terms";
pub const PRIVACY_URL: &str = "https://www.speedtest.net/about/privacy";

// Where to get the program, and the SHA-256 of that file. Ookla doesn't
// publish checksums, so these were taken from its files (unchanged since
// August 2022; each matches the MD5 its server gives as the ETag). A
// download that doesn't match isn't installed.
#[cfg(target_arch = "aarch64")]
const DOWNLOAD: (&str, &str) = (
    "https://install.speedtest.net/app/cli/ookla-speedtest-1.2.0-linux-aarch64.tgz",
    "3953d231da3783e2bf8904b6dd72767c5c6e533e163d3742fd0437affa431bd3",
);
#[cfg(not(target_arch = "aarch64"))]
const DOWNLOAD: (&str, &str) = (
    "https://install.speedtest.net/app/cli/ookla-speedtest-1.2.0-linux-x86_64.tgz",
    "5690596c54ff9bed63fa3732f818a05dbc2db19ad36ed68f21ca5f64d5cfeeb7",
);

/// How long a test may run before it's stopped; `NET_WATCH_TEST_LIMIT=secs`
/// to try it.
fn time_limit() -> Duration {
    Duration::from_secs(std::env::var("NET_WATCH_TEST_LIMIT").ok().and_then(|s| s.parse().ok()).unwrap_or(120))
}

const EXE: &str = "speedtest";

/// Where Net Watch keeps its own copy.
fn own_copy() -> Option<PathBuf> {
    Some(crate::store::data_dir()?.join("ookla").join(EXE))
}

/// The Speedtest program: Net Watch's own copy, else one installed on the
/// system (Ookla's packages put it on the PATH). The search is remembered
/// for a few seconds, as the window asks every frame.
pub fn program() -> Option<PathBuf> {
    if let Some(p) = own_copy().filter(|p| p.is_file()) {
        return Some(p);
    }
    static FOUND: Mutex<Option<(Instant, Option<PathBuf>)>> = Mutex::new(None);
    let mut found = FOUND.lock().unwrap();
    if let Some((at, p)) = found.as_ref()
        && at.elapsed() < Duration::from_secs(10)
    {
        return p.clone();
    }
    let path = std::env::var_os("PATH")?;
    let p = std::env::split_paths(&path).map(|d| d.join(EXE)).find(|p| p.is_file() && is_ookla(p));
    *found = Some((Instant::now(), p.clone()));
    p
}

/// Whether speed tests can run: the license accepted and the program there.
pub fn ready(prefs: &crate::store::Prefs) -> bool {
    std::env::var_os("NET_WATCH_FAKE_SPEED").is_some() || (prefs.ookla_accepted && program().is_some())
}

/// Another program called `speedtest` (the old Python speedtest-cli) takes
/// different options; Ookla's says so in its version line.
fn is_ookla(p: &PathBuf) -> bool {
    let out = Command::new(p).arg("--version").stdin(Stdio::null()).output();
    out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("Ookla"))
}

/// The SHA-256 of `bytes`, in lowercase hex.
fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether a download is the file Ookla published.
fn genuine(body: &[u8], expected: &str) -> Result<(), String> {
    if sha256(body) == expected {
        Ok(())
    } else {
        Err("the download didn't match Ookla's official file, so it wasn't installed. Try again later".into())
    }
}

/// Download Ookla's program into Net Watch's folder, checked before it's
/// saved or unpacked.
pub fn install() -> Result<PathBuf, String> {
    let dest = own_copy().ok_or("no data folder")?;
    let dir = dest.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let archive = dir.join("download.tgz");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();
    let mut body = Vec::new();
    agent
        .get(DOWNLOAD.0)
        .call()
        .map_err(|e| format!("couldn't download it ({e})"))?
        .body_mut()
        .with_config()
        .limit(20_000_000)
        .reader()
        .read_to_end(&mut body)
        .map_err(|e| format!("the download stopped ({e})"))?;
    genuine(&body, DOWNLOAD.1)?;
    std::fs::write(&archive, body).map_err(|e| e.to_string())?;
    // Unpacked to one side, then moved into place, so a half-unpacked
    // program is never run.
    let unpack = dir.join("unpack");
    let _ = std::fs::remove_dir_all(&unpack);
    let ok = std::fs::create_dir_all(&unpack)
        .map_err(|e| e.to_string())
        .and_then(|_| Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&unpack).arg(EXE).status().map_err(|e| e.to_string()));
    let _ = std::fs::remove_file(&archive);
    let unpacked = unpack.join(EXE);
    let done = match ok {
        Ok(s) if s.success() && unpacked.is_file() => std::fs::rename(&unpacked, &dest).map(|_| dest).map_err(|e| format!("couldn't put it in place ({e})")),
        Ok(_) => Err("couldn't unpack the download".into()),
        Err(e) => Err(format!("couldn't unpack the download ({e})")),
    };
    let _ = std::fs::remove_dir_all(&unpack);
    done
}

/// Stop the Speedtest program and anything it started, by its process id
/// (the `Child` is busy being read from on the other thread).
fn kill(pid: u32) {
    let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).status();
}

fn num(v: &Value, path: &[&str]) -> Option<f64> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_f64()
}

/// The program's message for an error, in its own words.
fn message(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? == "log" && v.get("level")?.as_str()? == "error" {
        return v.get("message")?.as_str().map(str::to_string);
    }
    v.get("error")?.as_str().map(str::to_string)
}

/// Run one test. `report` hears how it's going as it runs.
pub fn speed_test(report: &dyn Fn(&Live)) -> Result<Speed, String> {
    if std::env::var_os("NET_WATCH_FAKE_SPEED").is_some() {
        return crate::probe::fake_test(35, report);
    }
    let exe = program().ok_or("Speedtest isn't installed")?;
    let mut cmd = Command::new(exe);
    // Its own process group, so a stuck test can be stopped with anything
    // it started.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd
        .args(["--format=json", "--progress=yes", "--progress-update-interval=250", "--accept-license", "--accept-gdpr"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start Speedtest ({e})"))?;
    // A test is about 35 seconds; one that hangs is stopped, else it would
    // hold up every later test.
    let pid = child.id();
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog = {
        let timed_out = timed_out.clone();
        std::thread::spawn(move || {
            if finished.recv_timeout(time_limit()) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                timed_out.store(true, std::sync::atomic::Ordering::Relaxed);
                kill(pid);
            }
        })
    };
    let stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || BufReader::new(stderr).lines().map_while(Result::ok).filter_map(|l| message(&l)).last());

    let start = Instant::now();
    // Ookla's test is about 15 seconds each way after a few of ping; the
    // estimate grows if it runs longer.
    let mut live = Live { secs: 35.0, step: "latency", ..Default::default() };
    report(&live);
    let mut last = (0.0f64, 0.0f64); // (bytes, ms) at the last progress line
    let mut result = None;
    let mut error = None;
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let t = start.elapsed().as_secs_f32();
        live.secs = live.secs.max(t + 1.0);
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "testStart" => {
                let name = v.pointer("/server/name").and_then(Value::as_str).unwrap_or("");
                let place = v.pointer("/server/location").and_then(Value::as_str).unwrap_or("");
                live.server = Some(format!("{name} ({place})"));
            }
            "ping" => {
                live.ping_ms = num(&v, &["ping", "latency"]);
                live.jitter_ms = num(&v, &["ping", "jitter"]);
            }
            step @ ("download" | "upload") => {
                let upload = step == "upload";
                if live.step != step {
                    live.step = if upload { "upload" } else { "download" };
                    last = (0.0, 0.0);
                    if upload {
                        live.upload_at = Some(t);
                    }
                }
                let bytes = num(&v, &[step, "bytes"]).unwrap_or(0.0);
                let ms = num(&v, &[step, "elapsed"]).unwrap_or(0.0);
                // Ookla's `bandwidth` is the average so far, in bytes a second.
                let so_far = num(&v, &[step, "bandwidth"]).map(|b| b * 8.0 / 1e6);
                if ms > last.1 && bytes >= last.0 {
                    let now = (bytes - last.0) * 8.0 / ((ms - last.1) / 1000.0) / 1e6;
                    if upload { &mut live.up } else { &mut live.down }.push((t, now as f32));
                    live.bytes += (bytes - last.0) as u64;
                    last = (bytes, ms);
                }
                *if upload { &mut live.up_mbps } else { &mut live.down_mbps } = so_far;
            }
            "result" => result = Some(v),
            "log" if v.get("level").and_then(Value::as_str) == Some("error") => {
                error = v.get("message").and_then(Value::as_str).map(str::to_string);
            }
            _ => {}
        }
        report(&live);
    }
    // Output ended, so the program has stopped or is stopping: stand the
    // watchdog down before reaping it, so it can't hit a reused process id.
    let _ = done.send(());
    let _ = watchdog.join();
    let status = child.wait().map_err(|e| e.to_string());
    if timed_out.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(format!("Ookla: Speedtest took over {} minutes and was stopped", time_limit().as_secs().div_ceil(60)));
    }
    let status = status?;
    let stderr_error = errors.join().ok().flatten();
    let Some(r) = result else {
        let why = error.or(stderr_error).unwrap_or_else(|| format!("Speedtest stopped ({status})"));
        return Err(format!("Ookla: {why}"));
    };
    let mbps = |dir: &str| num(&r, &[dir, "bandwidth"]).map(|b| b * 8.0 / 1e6).ok_or_else(|| format!("Ookla: no {dir} result"));
    let bytes = num(&r, &["download", "bytes"]).unwrap_or(0.0) + num(&r, &["upload", "bytes"]).unwrap_or(0.0);
    Ok(Speed {
        down_mbps: mbps("download")?,
        up_mbps: mbps("upload")?,
        ping_ms: num(&r, &["ping", "latency"]).unwrap_or(0.0),
        jitter_ms: num(&r, &["ping", "jitter"]).unwrap_or(0.0),
        bytes: bytes as u64,
        server: live.server.clone(),
        link: r.pointer("/result/url").and_then(Value::as_str).map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_downloads() {
        assert_eq!(sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let good = sha256(b"the real file");
        assert!(genuine(b"the real file", &good).is_ok());
        assert!(genuine(b"the real fil3", &good).is_err());
        assert!(genuine(b"", &good).is_err());
    }

    /// Downloads the real program into a throwaway folder; needs the
    /// internet, so only when asked: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn installs_for_real() {
        let dir = std::env::temp_dir().join(format!("net-watch-install-test-{}", std::process::id()));
        // SAFETY: the only test that touches the environment, run on its own.
        unsafe { std::env::set_var("NET_WATCH_DATA", &dir) };
        let exe = install().expect("install");
        assert!(is_ookla(&exe));
        assert!(!dir.join("ookla").join("unpack").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
