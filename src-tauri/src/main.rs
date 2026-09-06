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

const PORTAL_ORIGIN: &str = "https://portal.striasystems.com";

fn machine_id() -> String {
    let raw = format!(
        "{}|{}|{}",
        std::env::var("USER").unwrap_or_default(),
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
    #[serde(default)]
    pairing_code: Option<String>,
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
    let res = client()?
        .post(format!("{PORTAL_ORIGIN}/api/auth/register"))
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
async fn sign_in(
    email: String,
    password: String,
) -> Result<String, String> {
    // 1. Login → session cookie
    let jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
    let c = reqwest::Client::builder()
        .cookie_provider(jar.clone())
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;
    let res = c
        .post(format!("{PORTAL_ORIGIN}/api/auth/login"))
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
        .post(format!("{PORTAL_ORIGIN}/api/auth/pairing"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let pair_text = pair_res.text().await.unwrap_or_default();
    let parsed: PairingResponse = serde_json::from_str(&pair_text)
        .map_err(|_| "Portal returned an unreadable response.".to_string())?;
    let code = parsed.pairing_code.ok_or_else(|| {
        portal_error(&pair_text, "Could not mint a pairing code. Sign in first.")
    })?;
    // 3. Exchange pairing code for machine token
    pair_machine(code).await
}

#[tauri::command]
async fn mint_pairing_code() -> Result<String, String> {
    let res = client()?
        .post(format!("{PORTAL_ORIGIN}/api/auth/pairing"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let text = res.text().await.unwrap_or_default();
    let parsed: PairingResponse = serde_json::from_str(&text)
        .map_err(|_| "Portal returned an unreadable response.".to_string())?;
    parsed
        .pairing_code
        .ok_or_else(|| portal_error(&text, "Could not mint a pairing code. Sign in first."))
}

#[tauri::command]
async fn pair_machine(pairing_code: String) -> Result<String, String> {
    let res = client()?
        .post(format!("{PORTAL_ORIGIN}/api/portal/ingest"))
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
    let token = parsed.machine_token.ok_or_else(|| {
        portal_error(&text, "Pairing failed. Check the code and try again.")
    })?;
    persist_pairing(&token)?;
    Ok(token)
}

fn persist_pairing(machine_token: &str) -> Result<(), String> {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return Err("Cannot resolve home directory.".to_string());
    }
    let dir = std::path::Path::new(&home).join(".stria");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir ~/.stria: {e}"))?;
    let record = serde_json::json!({
        "portalUrl": PORTAL_ORIGIN,
        "machineId": machine_id(),
        "machineToken": machine_token,
        "teamId": "team_default",
        "platform": platform_name(),
        "arch": arch_name(),
        "launcherVersion": env!("CARGO_PKG_VERSION"),
        "pairedAt": chrono_now_rfc3339(),
    });
    let path = dir.join("portal.json");
    std::fs::write(&path, serde_json::to_string_pretty(&record).unwrap_or_default() + "\n")
        .map_err(|e| format!("write portal.json: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn read_pairing_token() -> Result<String, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::path::Path::new(&home).join(".stria").join("portal.json");
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
    let token = read_pairing_token()?;
    let res = client()?
        .get(format!("{PORTAL_ORIGIN}/api/portal/software/updates"))
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
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::path::Path::new(&home).join(".stria").join("suite");
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
async fn report_software_versions(
    packages: serde_json::Value,
) -> Result<(), String> {
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
        .post(format!("{PORTAL_ORIGIN}/api/portal/software"))
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
            platform_name,
            arch_name
        ])
        .run(tauri::generate_context!())
        .expect("error while running stria launcher");
}
