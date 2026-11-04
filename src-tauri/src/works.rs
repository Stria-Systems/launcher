/*
 * Stria Works launch + liveness monitoring.
 *
 * The launcher is responsible for starting/stopping the real local Stria
 * Works process and monitoring its health. Identity flows through the
 * shared pairing file (~/.stria/portal.json) which Works reads itself;
 * nothing is duplicated here.
 *
 * Liveness: Works enforces a single instance and records its PID in
 * ~/.stria-works/stria-works-gpui.lock — the launcher reads that file to
 * check whether Works is running (kill -0 on unix, process probe on
 * Windows).
 */

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

/// PID of the Works child this launcher process spawned (if any). Lets us
/// reap our own child with waitpid instead of mistaking a zombie for a
/// live process (kill -0 succeeds on zombies).
static MANAGED_PID: Mutex<Option<u32>> = Mutex::new(None);

/// Resolve the user's home dir on every OS (same rules as main.rs).
fn home_dir() -> Result<PathBuf, String> {
    for key in ["HOME", "USERPROFILE"] {
        let v = std::env::var(key).unwrap_or_default();
        if !v.trim().is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    Err("Cannot resolve home directory (HOME/USERPROFILE unset).".to_string())
}

#[derive(serde::Serialize, Debug)]
pub struct LaunchReport {
    pub launched: bool,
    pub already_running: bool,
    pub pid: Option<u32>,
    pub binary: String,
    pub note: String,
}

/// PID of the running Stria Works instance from its single-instance lock.
pub fn works_lock_pid_in(home: &std::path::Path) -> Option<u32> {
    let path = home.join(".stria-works").join("stria-works-gpui.lock");
    let contents = std::fs::read_to_string(path).ok()?;
    contents.trim().parse::<u32>().ok()
}

pub fn works_lock_pid() -> Option<u32> {
    works_lock_pid_in(&home_dir().ok()?)
}

pub fn is_process_alive(pid: u32) -> bool {
    if managed_pid() == Some(pid) {
        // Our own child: waitpid(WNOHANG) distinguishes running (0) from
        // exited-and-reaped (pid) without zombie false-positives.
        #[cfg(unix)]
        {
            let mut status = 0;
            // SAFETY: waitpid on our own child pid with WNOHANG never blocks.
            let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
            if r == 0 {
                return true;
            }
            *MANAGED_PID.lock().unwrap() = None;
            return false;
        }
        #[cfg(windows)]
        {
            return probe_windows(pid);
        }
    }
    #[cfg(unix)]
    {
        // kill -0: signal 0 checks existence without delivering a signal.
        // SAFETY: `kill` is an async-signal-safe POSIX call; 0 perms check.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return false;
        }
        // A PID alone is not identity: PIDs are recycled, so a stale lock
        // file can point at an unrelated new process. Confirm the process
        // image is actually Stria Works before reporting "running".
        owns_works_image(pid)
    }
    #[cfg(windows)]
    {
        probe_windows(pid)
    }
}

#[cfg(unix)]
fn owns_works_image(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output();
    match out {
        Ok(o) => {
            let comm = String::from_utf8_lossy(&o.stdout).to_lowercase();
            comm.contains("stria-works")
        }
        Err(_) => false,
    }
}

fn managed_pid() -> Option<u32> {
    *MANAGED_PID.lock().unwrap()
}

#[cfg(windows)]
fn probe_windows(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// Candidate locations for the installed Works binary, in priority order.
/// `$STRIA_WORKS_BIN` wins for dev/CI.
pub fn resolve_works_binary_in(home: &std::path::Path) -> Option<PathBuf> {
    let allow_override = cfg!(debug_assertions)
        || std::env::var("STRIA_ALLOW_UNVERIFIED_WORKS").as_deref() == Ok("1");
    if allow_override {
        if let Ok(p) = std::env::var("STRIA_WORKS_BIN") {
            if !p.trim().is_empty() {
                let path = PathBuf::from(p);
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }
    let suite = home.join(".stria").join("suite");
    // App bundle installed by the launcher suite (macOS layout).
    #[cfg(target_os = "macos")]
    {
        let bundled = suite
            .join("Stria Works.app")
            .join("Contents")
            .join("MacOS")
            .join("stria-works-gpui");
        if bundled.exists() {
            return Some(bundled);
        }
    }
    let plain = suite.join("stria-works-gpui");
    if plain.exists() {
        return Some(plain);
    }
    let plain_exe = suite.join("stria-works-gpui.exe");
    if plain_exe.exists() {
        return Some(plain_exe);
    }
    // Standard install locations.
    let applications = if cfg!(target_os = "macos") {
        home.join("Applications")
    } else {
        suite
    };
    #[cfg(target_os = "macos")]
    for base in [home.join("Applications"), PathBuf::from("/Applications")] {
        // STRIA_WORKS_NO_SYSTEM_APPS=1 skips the machine-global scan so
        // tests resolve purely from the given home + STRIA_WORKS_BIN, even
        // on a Mac with Works installed. Production never sets it.
        if base.as_path() == std::path::Path::new("/Applications")
            && std::env::var("STRIA_WORKS_NO_SYSTEM_APPS").is_ok()
        {
            continue;
        }
        let bundled = base
            .join("Stria Works.app")
            .join("Contents")
            .join("MacOS")
            .join("stria-works-gpui");
        if bundled.exists() {
            return Some(bundled);
        }
    }
    let _ = applications;
    None
}

/// Launch the real local Stria Works process (detached — it must outlive
/// the launcher window). Returns the child pid when we spawned it directly.
pub fn launch_works() -> Result<LaunchReport, String> {
    let home = home_dir()?;
    if let Some(pid) = works_lock_pid_in(&home) {
        if is_process_alive(pid) {
            return Ok(LaunchReport {
                launched: false,
                already_running: true,
                pid: Some(pid),
                binary: String::new(),
                note: "Stria Works is already running.".to_string(),
            });
        }
    }
    let binary = resolve_works_binary_in(&home).ok_or_else(|| {
        "Stria Works is not installed yet — install it from the Software Hub above.".to_string()
    })?;
    let binary_str = binary.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    {
        // Prefer `open` for app bundles so the Dock/LaunchServices contract
        // holds; fall back to direct spawn for bare binaries.
        if binary_str.contains(".app/") {
            let app_path = binary_str
                .split(".app/")
                .next()
                .map(|s| format!("{}.app", s.trim_end_matches('/')))
                .unwrap_or_else(|| binary_str.clone());
            let status = Command::new("open")
                .arg("-a")
                .arg(&app_path)
                .status()
                .map_err(|e| format!("open {app_path}: {e}"))?;
            if !status.success() {
                return Err(format!("open {app_path} failed"));
            }
            // Give LaunchServices a beat, then read the PID Works recorded.
            std::thread::sleep(std::time::Duration::from_millis(700));
            return Ok(LaunchReport {
                launched: true,
                already_running: false,
                pid: works_lock_pid_in(&home),
                binary: app_path,
                note: "Stria Works launched.".to_string(),
            });
        }
    }
    let child = Command::new(&binary)
        .env("STRIA_LAUNCHER_SPAWNED", "1")
        .spawn()
        .map_err(|e| format!("spawn {binary_str}: {e}"))?;
    let pid = child.id();
    // Track our child for zombie-free liveness checks. We intentionally do
    // not wait() here — Works must outlive the launcher — but waitpid(WNOHANG)
    // in is_process_alive reaps it once it exits.
    *MANAGED_PID.lock().unwrap() = Some(pid);
    drop(child);
    let _ = std::fs::create_dir_all(home.join(".stria"));
    let _ = std::fs::write(home.join(".stria").join("works.pid"), pid.to_string());
    Ok(LaunchReport {
        launched: true,
        already_running: false,
        pid: Some(pid),
        binary: binary_str,
        note: "Stria Works launched.".to_string(),
    })
}

/// Stop a running Stria Works instance (SIGTERM, then SIGKILL).
pub fn stop_works() -> Result<bool, String> {
    let home = home_dir()?;
    let Some(pid) = works_lock_pid_in(&home) else {
        return Ok(false);
    };
    if !is_process_alive(pid) {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        // SAFETY: standard POSIX termination signals.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        for _ in 0..20 {
            if !is_process_alive(pid) {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        // Reap our own child so the stop is observable (no zombie).
        for _ in 0..50 {
            if !is_process_alive(pid) {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    #[cfg(windows)]
    {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .map_err(|e| format!("taskkill: {e}"))?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("launcher-test-{}", uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uuid() -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_le_bytes(),
        );
        hex::encode(&h.finalize()[..8])
    }

    #[test]
    fn lock_pid_read_write() {
        let home = temp_home();
        assert_eq!(works_lock_pid_in(&home), None);
        std::fs::create_dir_all(home.join(".stria-works")).unwrap();
        std::fs::write(
            home.join(".stria-works").join("stria-works-gpui.lock"),
            "4242",
        )
        .unwrap();
        assert_eq!(works_lock_pid_in(&home), Some(4242));
    }

    #[test]
    #[cfg(unix)]
    fn dead_pid_is_not_alive() {
        // Spawn a process that exits immediately, reap it, then probe: the
        // pid must report dead (a recycled pid within the same instant is
        // vanishingly unlikely on test runners).
        let mut child = Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        let _ = child.wait();
        for _ in 0..50 {
            if !is_process_alive(pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("pid {pid} still reports alive after exit");
    }

    #[test]
    fn resolve_prefers_suite_binary() {
        // Process-global env is shared across test threads: serialize with
        // the sibling env-mutating test and pin every variable resolve reads.
        let _guard = env_lock();
        let _bin = EnvGuard::set("STRIA_WORKS_BIN", "");
        let _sys = EnvGuard::set("STRIA_WORKS_NO_SYSTEM_APPS", "1");
        let home = temp_home();
        assert!(resolve_works_binary_in(&home).is_none());
        let dir = home.join(".stria").join("suite");
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("stria-works-gpui");
        std::fs::write(&bin, b"binary").unwrap();
        assert_eq!(resolve_works_binary_in(&home), Some(bin));
    }

    #[test]
    fn launch_requires_installed_binary() {
        // No STRIA_WORKS_BIN and empty home → clear install hint.
        let _guard = env_lock();
        let _bin = EnvGuard::set("STRIA_WORKS_BIN", "");
        let _sys = EnvGuard::set("STRIA_WORKS_NO_SYSTEM_APPS", "1");
        let home = temp_home();
        let home_str = home.to_string_lossy();
        let _home = EnvGuard::set("HOME", &home_str);
        let err = launch_works().unwrap_err();
        assert!(err.contains("not installed"), "{err}");
    }

    /// Serializes tests that mutate process-global env (cargo runs tests
    /// on threads sharing one environment — parallel set/remove is a race).
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap()
    }

    /// Save/remove/restore one env var for the guard's lifetime.
    struct EnvGuard {
        key: &'static str,
        saved: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let saved = std::env::var(key).ok();
            if value.is_empty() {
                std::env::remove_var(key);
            } else {
                std::env::set_var(key, value);
            }
            Self { key, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
