/*
 * Stria launcher — core commands.
 *
 * First-run flow:
 *   1. register_workspace  — POST /api/auth/register on the portal
 *   2. pair_machine        — POST /api/portal/ingest with the pairing code;
 *                            the one-time exchange returns a machine token
 *                            and the launcher persists ~/.stria/portal.json
 *                            (the CLI + Stria Works read the same file)
 *   3. fetch_software_updates — check what the org policy says to install
 *   4. download_suite_asset — download + verify + install any package
 *   5. report_software_versions — report what's installed back to portal
 *
 * The launcher is the hub that installs and updates all Stria software on
 * a machine. The portal is the compliance dashboard that shows org-wide
 * version consistency. Every machine in an org runs the same version —
 * the launcher enforces this by checking the org manifest on every sync.
 */
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Deserialize;
use sha2::{Digest, Sha256};

mod works;

const PORTAL_ORIGIN: &str = "https://portal.striasystems.com";

/// Portal origin for all API calls. Production default; `STRIA_PORTAL_ORIGIN`
/// override exists ONLY for staging/local acceptance (the acceptance gate
/// must exercise the real code path against a non-production Portal).
fn portal_origin() -> String {
    std::env::var("STRIA_PORTAL_ORIGIN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| PORTAL_ORIGIN.to_string())
}

fn machine_id() -> String {
    let raw = format!(
        "{}|{}|{}",
        username(),
        platform_name(),
        std::env::consts::ARCH
    );
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    let digest = hex::encode(h.finalize());
    format!("stria-{}", &digest[..16])
}

#[tauri::command]
fn platform_name() -> String {
    match std::env::consts::OS {
        "macos" => "macos".into(),
        "windows" => "windows".into(),
        _ => "linux".into(),
    }
}

#[tauri::command]
fn arch_name() -> String {
    match std::env::consts::ARCH {
        "aarch64" => "arm64".into(),
        "x86_64" => "x64".into(),
        other => other.into(),
    }
}

#[derive(Deserialize)]
struct PairingResponse {
    // Portal returns `{ ok, code, expiresAt }`. Accept the legacy
    // `pairing_code` alias too so older portal builds keep working.
    #[serde(default, alias = "pairing_code")]
    code: Option<String>,
}

/// Resolve the user's home dir on every OS. `HOME` is unset in default
/// Windows shells — fall back to `USERPROFILE` before giving up.
fn home_dir() -> Result<std::path::PathBuf, String> {
    for key in ["HOME", "USERPROFILE"] {
        let v = std::env::var(key).unwrap_or_default();
        if !v.trim().is_empty() {
            return Ok(std::path::PathBuf::from(v));
        }
    }
    Err("Cannot resolve home directory (HOME/USERPROFILE unset).".to_string())
}

/// Username for machine-id derivation. `USER` is unset on Windows —
/// fall back to `USERNAME`.
fn username() -> String {
    for key in ["USER", "USERNAME"] {
        let v = std::env::var(key).unwrap_or_default();
        if !v.trim().is_empty() {
            return v;
        }
    }
    "unknown".to_string()
}

#[derive(Deserialize)]
struct IngestResponse {
    #[serde(default)]
    machine_token: Option<String>,
}

fn portal_error(body: &str, fallback: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| fallback.to_string())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))
}

#[tauri::command]
async fn register_workspace(
    email: String,
    organization: String,
    password: String,
) -> Result<(), String> {
    let origin = portal_origin();
    let res = client()?
        .post(format!("{origin}/api/auth/register"))
        .json(&serde_json::json!({
            "email": email,
            "organization": organization,
            "password": password,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if res.status().is_success() {
        Ok(())
    } else {
        let text = res.text().await.unwrap_or_default();
        Err(portal_error(&text, "Registration failed."))
    }
}

#[tauri::command]
async fn sign_in(email: String, password: String) -> Result<String, String> {
    let origin = portal_origin();
    // 1. Login → session cookie
    let jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
    let c = reqwest::Client::builder()
        .cookie_provider(jar.clone())
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;
    let res = c
        .post(format!("{origin}/api/auth/login"))
        .json(&serde_json::json!({
            "email": email,
            "password": password,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(portal_error(&text, "Invalid email or password."));
    }
    // 2. Mint pairing code (session cookie attached)
    let pair_res = c
        .post(format!("{origin}/api/auth/pairing"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let pair_text = pair_res.text().await.unwrap_or_default();
    let parsed: PairingResponse = serde_json::from_str(&pair_text)
        .map_err(|_| "Portal returned an unreadable response.".to_string())?;
    let code = parsed
        .code
        .ok_or_else(|| portal_error(&pair_text, "Could not mint a pairing code. Sign in first."))?;
    // 3. Exchange pairing code for machine token
    pair_machine(code).await
}

#[tauri::command]
async fn mint_pairing_code() -> Result<String, String> {
    let origin = portal_origin();
    let res = client()?
        .post(format!("{origin}/api/auth/pairing"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let text = res.text().await.unwrap_or_default();
    let parsed: PairingResponse = serde_json::from_str(&text)
        .map_err(|_| "Portal returned an unreadable response.".to_string())?;
    parsed
        .code
        .ok_or_else(|| portal_error(&text, "Could not mint a pairing code. Sign in first."))
}

#[tauri::command]
async fn pair_machine(pairing_code: String) -> Result<String, String> {
    let origin = portal_origin();
    let res = client()?
        .post(format!("{origin}/api/portal/ingest"))
        .bearer_auth(&pairing_code)
        .json(&serde_json::json!({
            "organization_id": "",
            "team_id": "default",
            "machine_id": machine_id(),
            "objective": "launcher pairing",
            "source": "pi",
            "status": "working",
            "started_at": chrono_now_rfc3339(),
            "platform": platform_name(),
            "arch": arch_name(),
            "launcher_version": env!("CARGO_PKG_VERSION"),
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let text = res.text().await.unwrap_or_default();
    let parsed: IngestResponse = serde_json::from_str(&text)
        .map_err(|_| "Portal returned an unreadable response.".to_string())?;
    let token = parsed
        .machine_token
        .ok_or_else(|| portal_error(&text, "Pairing failed. Check the code and try again."))?;
    persist_pairing(&token, &origin)?;
    Ok(token)
}

fn persist_pairing(machine_token: &str, portal_origin: &str) -> Result<(), String> {
    let dir = home_dir()?.join(".stria");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir ~/.stria: {e}"))?;
    let record = serde_json::json!({
        "portalUrl": portal_origin,
        "machineId": machine_id(),
        "machineToken": machine_token,
        "teamId": "team_default",
        "platform": platform_name(),
        "arch": arch_name(),
        "launcherVersion": env!("CARGO_PKG_VERSION"),
        "pairedAt": chrono_now_rfc3339(),
    });
    let path = dir.join("portal.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&record).unwrap_or_default() + "\n",
    )
    .map_err(|e| format!("write portal.json: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn read_pairing_token() -> Result<String, String> {
    let path = home_dir()?.join(".stria").join("portal.json");
    let raw = std::fs::read_to_string(&path).map_err(|_| "Not paired.".to_string())?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "~/.stria/portal.json is corrupt.".to_string())?;
    let t = v
        .get("machineToken")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    if t.is_empty() {
        return Err("Not paired.".to_string());
    }
    Ok(t)
}

#[tauri::command]
async fn fetch_software_updates() -> Result<String, String> {
    let origin = portal_origin();
    let token = read_pairing_token()?;
    let res = client()?
        .get(format!("{origin}/api/portal/software/updates"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let text = res.text().await.map_err(|e| format!("Read failed: {e}"))?;
    Ok(text)
}

#[tauri::command]
async fn download_suite_asset(url: String, expected_sha256: String) -> Result<String, String> {
    let res = client()?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("Download failed: HTTP {}", res.status()));
    }
    let bytes = res
        .bytes()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let digest = hex::encode(h.finalize());
    if !digest.eq_ignore_ascii_case(&expected_sha256) {
        return Err(format!(
            "Checksum mismatch: expected {expected_sha256}, got {digest}"
        ));
    }
    let dir = home_dir()?.join(".stria").join("suite");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir suite dir: {e}"))?;
    let file_name = url.rsplit('/').next().unwrap_or("stria-suite.bin");
    let path = dir.join(file_name);
    std::fs::write(&path, &bytes).map_err(|e| format!("write asset: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
async fn download_text(url: String) -> Result<String, String> {
    let res = client()?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("Download failed: HTTP {}", res.status()));
    }
    res.text().await.map_err(|e| format!("Read failed: {e}"))
}

#[tauri::command]
async fn report_software_versions(packages: serde_json::Value) -> Result<(), String> {
    let origin = portal_origin();
    let token = read_pairing_token()?;
    let mut report = serde_json::json!({});
    // Always report launcher version
    report["stria-launcher"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    // Merge caller-supplied package versions (from JS after download)
    if let Some(obj) = packages.as_object() {
        for (k, v) in obj {
            report[k] = v.clone();
        }
    }
    let body = serde_json::json!({ "packages": report });
    let res = client()?
        .post(format!("{origin}/api/portal/software"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("report failed: HTTP {}", res.status()));
    }
    Ok(())
}

fn chrono_now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (now / 86400) as i64;
    let secs = now % 86400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let d = doy - (153 * mp + 2) / 5 + 1;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

#[tauri::command]
fn launch_works() -> Result<works::LaunchReport, String> {
    works::launch_works()
}

#[tauri::command]
fn works_running() -> Result<bool, String> {
    Ok(works::works_lock_pid()
        .map(works::is_process_alive)
        .unwrap_or(false))
}

#[tauri::command]
fn stop_works() -> Result<bool, String> {
    works::stop_works()
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            register_workspace,
            sign_in,
            mint_pairing_code,
            pair_machine,
            fetch_software_updates,
            download_suite_asset,
            download_text,
            report_software_versions,
            launch_works,
            works_running,
            stop_works,
            platform_name,
            arch_name
        ])
        .run(tauri::generate_context!())
        .expect("error while running stria launcher");
}

/// Release acceptance tests — the actual customer path, run ONLY with
/// STRIA_ACCEPTANCE=1 against a live (staging or local) Portal:
///   HOME=<fresh sandbox> STRIA_ACCEPTANCE=1 \
///   STRIA_ACCEPTANCE_PAIRING_CODE=AAA-BBB-CCC cargo test -p stria-launcher acceptance -- --test-threads=1
/// These touch the network, pair a real device, and spawn the real Works
/// GUI — never in CI.
#[cfg(test)]
mod acceptance {
    use super::*;

    fn enabled() -> bool {
        std::env::var("STRIA_ACCEPTANCE").ok().as_deref() == Some("1")
    }

    fn sandbox_home() -> std::path::PathBuf {
        std::env::var("STRIA_ACCEPTANCE_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("stria-acceptance-home"))
    }

    /// The real launcher pairing code path: pairing code → machine token →
    /// ~/.stria/portal.json in the sandbox. Uses the same sign of the same
    /// endpoint the UI calls.
    #[tokio::test]
    async fn acceptance_pairing_writes_portal_json() {
        if !enabled() {
            return;
        }
        let code = std::env::var("STRIA_ACCEPTANCE_PAIRING_CODE")
            .expect("STRIA_ACCEPTANCE_PAIRING_CODE required");
        std::env::set_var("HOME", sandbox_home());
        let token = pair_machine(code).await.expect("pairing must succeed");
        assert!(token.starts_with("stria_mt"), "machine token shape");
        let raw = std::fs::read_to_string(sandbox_home().join(".stria").join("portal.json"))
            .expect("portal.json written");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["machineToken"], token);
        assert!(!v["machineId"].as_str().unwrap_or("").is_empty());
    }

    /// The real launch path: release Works binary installed in the sandbox
    /// suite dir → launcher spawns it (no $STRIA_WORKS_BIN override) →
    /// liveness true → stop → dead.
    #[test]
    fn acceptance_launch_and_stop_packaged_works() {
        if !enabled() {
            return;
        }
        std::env::remove_var("STRIA_WORKS_BIN");
        std::env::set_var("HOME", sandbox_home());
        // Release binary must be suite-installed by the release flow first.
        let bin = crate::works::resolve_works_binary_in(&sandbox_home());
        assert!(bin.is_some(), "release Works must be suite-installed");
        let report = crate::works::launch_works().expect("launch must succeed");
        assert!(!report.already_running, "fresh sandbox: nothing running");
        // Dwell so the booted Works can initialize backend + sync and drain.
        let dwell: u64 = std::env::var("STRIA_ACCEPTANCE_DWELL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);
        std::thread::sleep(std::time::Duration::from_secs(dwell));
        let pid = crate::works::works_lock_pid_in(&sandbox_home());
        assert!(pid.is_some(), "Works must record its PID");
        assert!(crate::works::is_process_alive(pid.unwrap()));
        assert!(crate::works::stop_works().expect("stop must succeed"));
        for _ in 0..30 {
            if !crate::works::is_process_alive(pid.unwrap()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        panic!("Works did not stop");
    }
}
