/*
 * Stria launcher — core commands.
 *
 * First-run flow:
 *   1. register_workspace  — POST /api/auth/register on the portal
 *   2. pair_machine        — POST /api/portal/ingest with the pairing code;
 *                            the one-time exchange returns a machine token
 *                            which the UI stores (keychain lands with the
 *                            per-OS installer work)
 *   3. launch_works        — hand off to the installed Stria Works app
 *
 * The pairing endpoint contract mirrors the portal: single-use code,
 * AAA-BBB-CCC format, 15-minute expiry. The ingest call records platform,
 * arch, machine id, and launcher version in the portal's Machines list.
 */
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Deserialize;
use sha2::{Digest, Sha256};

const PORTAL_ORIGIN: &str = "https://portal.striasystems.com";

fn machine_id() -> String {
    // Stable per-machine id. Real hardware UUID (IOPlatformUUID /
    // MachineGuid / /etc/machine-id) lands with the per-OS installer work;
    // this keeps the contract compiling everywhere today.
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

fn platform_name() -> String {
    match std::env::consts::OS {
        "macos" => "macos".into(),
        "windows" => "windows".into(),
        _ => "linux".into(),
    }
}

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

/// POST the registration form to the portal. Ok(()) on 2xx.
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

/// Mint a fresh single-use pairing code for the signed-in session.
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

/// Exchange the user's pairing code for a long-lived machine token.
/// The first ingest call both pairs the machine and registers it in the
/// portal (platform, arch, launcher version, last_seen).
#[tauri::command]
async fn pair_machine(pairing_code: String) -> Result<String, String> {
    let res = client()?
        .post(format!("{PORTAL_ORIGIN}/api/portal/ingest"))
        .bearer_auth(&pairing_code)
        .json(&serde_json::json!({
            "organization_id": "default",
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
    parsed.machine_token.ok_or_else(|| {
        portal_error(&text, "Pairing failed. Check the code and try again.")
    })
}

/// RFC3339 timestamp without pulling chrono into the bundle.
fn chrono_now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Days-to-civil conversion (Howard Hinnant's algorithm).
    let days = (now / 86400) as i64;
    let secs = now % 86400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, (secs % 3600) / 60, secs % 60)
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            register_workspace,
            mint_pairing_code,
            pair_machine
        ])
        .run(tauri::generate_context!())
        .expect("error while running stria launcher");
}
