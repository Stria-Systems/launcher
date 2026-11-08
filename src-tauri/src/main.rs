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
    let configured = std::env::var("STRIA_PORTAL_ORIGIN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| PORTAL_ORIGIN.to_string());
    normalized_origin(&configured).unwrap_or_else(|| PORTAL_ORIGIN.to_string())
}

fn normalized_origin(raw: &str) -> Option<String> {
    let url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() {
        return None;
    }
    url.host_str()?;
    if url.path() != "/" && !url.path().is_empty() {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

fn asset_origin() -> String {
    std::env::var("STRIA_ASSET_ORIGIN")
        .ok()
        .and_then(|v| normalized_origin(&v))
        .unwrap_or_else(portal_origin)
}

fn validated_asset_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "Invalid asset URL.".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() {
        return Err("Asset URL must be an HTTP(S) origin without credentials.".to_string());
    }
    if url.origin().ascii_serialization() != asset_origin() {
        return Err("Asset URL is not on the configured asset origin.".to_string());
    }
    Ok(url)
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

/// Non-secret identity returned by the pairing exchange (contract §5.1).
/// The launcher is the only writer of `portal.json`; the portal is the only
/// authority for these values. A team label is NOT an organization.
#[derive(Deserialize)]
struct PairingIdentity {
    #[serde(default)]
    org_id: String,
    #[serde(default)]
    org_name: String,
    #[serde(default)]
    user_email: String,
    #[serde(default)]
    team_id: String,
}

#[derive(Deserialize)]
struct IngestResponse {
    #[serde(default)]
    machine_token: Option<String>,
    #[serde(default)]
    identity: Option<PairingIdentity>,
}

fn portal_error(body: &str, fallback: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| fallback.to_string())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
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

/// Pending `stria://pair` deep link waiting for the UI. The OS can wake
/// the launcher with a pairing URL before the webview exists (cold start),
/// so Rust parks the raw URL here and the UI replays it on load via
/// `take_pending_pair_code`. Warm links (app already open) additionally
/// emit `stria-pair-url`. Shape authority stays in the UI: it already
/// validates and normalizes AAA-BBB-CCC.
struct PendingPairCode(std::sync::Mutex<Option<String>>);

/// Portal "Open in launcher" links look like `stria://pair?code=AAA-BBB-CCC`.
fn is_pair_url(raw: &str) -> bool {
    raw.starts_with("stria://pair?") || raw.starts_with("stria://pair/")
}

fn setup_deep_link(app: &tauri::App) {
    use tauri::{Emitter, Manager};
    use tauri_plugin_deep_link::DeepLinkExt;
    let handle = app.handle().clone();
    // Fail-open: if the OS refuses scheme registration the launcher still
    // works — the operator copies the pairing code by hand.
    if let Err(e) = app.deep_link().register("stria") {
        eprintln!("stria launcher: could not register stria:// handler: {e}");
    }
    let warm = handle.clone();
    handle.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            let raw = url.to_string();
            if is_pair_url(&raw) {
                warm.state::<PendingPairCode>()
                    .0
                    .lock()
                    .unwrap()
                    .replace(raw.clone());
                let _ = warm.emit("stria-pair-url", raw);
            }
        }
    });
    // Cold start straight into a link: park it for the UI to replay.
    if let Ok(Some(urls)) = handle.deep_link().get_current() {
        for url in urls {
            let raw = url.to_string();
            if is_pair_url(&raw) {
                handle
                    .state::<PendingPairCode>()
                    .0
                    .lock()
                    .unwrap()
                    .replace(raw);
            }
        }
    }
}

#[tauri::command]
fn take_pending_pair_code(state: tauri::State<'_, PendingPairCode>) -> Option<String> {
    state.0.lock().unwrap().take()
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    fn sample() -> PairingIdentity {
        PairingIdentity {
            org_id: "org_acme".into(),
            org_name: "Acme Test Co".into(),
            user_email: "ada@acme.test".into(),
            team_id: "org_acme__team_default".into(),
        }
    }

    #[test]
    fn pairing_record_separates_employee_customer_and_device() {
        let rec = pairing_record("stria_mt_test", "https://portal.example", Some(&sample()));
        assert_eq!(rec["schemaVersion"], 2);
        assert_eq!(rec["orgId"], "org_acme", "customer workspace id");
        assert_eq!(rec["orgName"], "Acme Test Co");
        assert_eq!(rec["userEmail"], "ada@acme.test", "employee identity");
        assert!(rec.get("machineId").is_some(), "device identity");
        // A team is stored, but only as its own routing label: it must never
        // occupy or derive the organization field.
        assert_eq!(rec["teamId"], "org_acme__team_default");
        assert_ne!(rec["orgId"], rec["teamId"]);
    }

    #[test]
    fn public_identity_never_exposes_the_machine_token() {
        let rec = pairing_record("stria_mt_secret", "https://portal.example", Some(&sample()));
        let view = public_identity(&rec);
        assert_eq!(view["paired"], true);
        assert_eq!(view["orgId"], "org_acme");
        assert!(view.get("machineToken").is_none());
        let rendered = view.to_string();
        assert!(!rendered.contains("stria_mt_secret"));
    }

    #[test]
    fn unpaired_identity_omits_empty_profile_fields() {
        let rec = pairing_record("stria_mt_test", "https://portal.example", None);
        assert!(rec.get("orgId").is_none());
        assert!(rec.get("orgName").is_none());
        assert!(rec.get("userEmail").is_none());
        assert_eq!(public_identity(&rec)["orgId"], "");
    }

    #[test]
    fn portal_json_write_is_atomic_and_private() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("stria-launcher-identity-{stamp}"));
        let path = dir.join(".stria").join("portal.json");
        let first = pairing_record("stria_mt_a", "https://portal.example", Some(&sample()));
        write_portal_json(&path, &first).expect("first write");
        let second = pairing_record("stria_mt_b", "https://portal.example", None);
        write_portal_json(&path, &second).expect("atomic replace");
        let raw = std::fs::read_to_string(&path).expect("read back");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["machineToken"], "stria_mt_b");
        assert!(
            !path.with_extension("json.tmp").exists(),
            "temp file must be renamed away, not left behind"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "machine token file must be owner-only");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod deep_link_tests {
    use super::is_pair_url;

    #[test]
    fn pair_urls_route_to_pairing() {
        assert!(is_pair_url("stria://pair?code=ABC-DEF-GHI"));
        assert!(is_pair_url("stria://pair/?code=ABC-DEF-GHI"));
        assert!(!is_pair_url("stria://downloads"));
        assert!(!is_pair_url("https://portal.striasystems.com/downloads"));
        assert!(!is_pair_url("stria://pair"));
        assert!(!is_pair_url(""));
    }
}

/// Which portal this launcher talks to. Production by default;
/// `STRIA_PORTAL_ORIGIN` points it at staging/local for acceptance.
/// The UI surfaces this as an env pill so a staging sign-in can never
/// be mistaken for production (and vice versa).
#[tauri::command]
fn portal_env() -> serde_json::Value {
    let origin = portal_origin();
    let env = if origin == PORTAL_ORIGIN {
        "production"
    } else if origin.contains("localhost")
        || origin.contains("127.0.0.1")
        || origin.contains(".local")
    {
        "development"
    } else {
        "staging"
    };
    serde_json::json!({ "origin": origin, "env": env })
}

/// In-flight password logins awaiting a second factor. The cookie jar
/// (pre-2FA, no session yet) lives here keyed by challenge id so the
/// verify step resumes the SAME login — a fresh client would have no
/// session to mint a pairing code with. Entries are short-lived
/// (challenge TTL is 5 minutes) and pruned on every insert/verify.
struct PendingLogins(
    std::sync::Mutex<std::collections::HashMap<String, (reqwest::Client, std::time::Instant)>>,
);

impl PendingLogins {
    fn store(&self, challenge: String, client: reqwest::Client) {
        let mut map = self.0.lock().unwrap();
        let now = std::time::Instant::now();
        map.retain(|_, (_, at)| now.duration_since(*at) < std::time::Duration::from_secs(600));
        map.insert(challenge, (client, now));
    }

    fn get(&self, challenge: &str) -> Option<reqwest::Client> {
        let mut map = self.0.lock().unwrap();
        let now = std::time::Instant::now();
        map.retain(|_, (_, at)| now.duration_since(*at) < std::time::Duration::from_secs(600));
        map.get(challenge).map(|(c, _)| c.clone())
    }

    fn remove(&self, challenge: &str) {
        self.0.lock().unwrap().remove(challenge);
    }
}

#[tauri::command]
async fn sign_in(
    state: tauri::State<'_, PendingLogins>,
    email: String,
    password: String,
) -> Result<(), String> {
    let origin = portal_origin();
    // 1. Login → session cookie (or a second-factor challenge)
    let jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
    let c = reqwest::Client::builder()
        .cookie_provider(jar.clone())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
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
    if res.status().as_u16() == 403 {
        // Password verified; the workspace still owes a second factor.
        let text = res.text().await.unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if v.get("two_factor_required")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            let challenge = v
                .get("challenge_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            if challenge.is_empty() {
                return Err(
                    "Sign-in needs a second factor but the portal sent no challenge. Try again."
                        .to_string(),
                );
            }
            state.store(challenge.clone(), c);
            // Typed wire error the UI matches on (never shown verbatim).
            return Err(format!("TWO_FACTOR_REQUIRED|{challenge}"));
        }
        if v.get("two_factor_setup_required")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            let setup = v
                .get("setup_challenge_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            return Err(format!("TWO_FACTOR_SETUP_REQUIRED|{setup}"));
        }
        return Err(portal_error(&text, "Sign-in refused."));
    }
    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(portal_error(&text, "Invalid email or password."));
    }
    // 2. Mint pairing code (session cookie attached)
    let code = mint_pairing_with(&c, &origin).await?;
    // 3. Exchange pairing code for machine token
    pair_machine(code).await
}

/// Complete a challenged login: TOTP 6-digit code or single-use recovery
/// code. On success this finishes the SAME login (mint + pair) and the
/// operator lands in the software hub — a relogin after logout always
/// passes through here; there is no trusted-device bypass.
#[tauri::command]
async fn verify_two_factor(
    state: tauri::State<'_, PendingLogins>,
    challenge_id: String,
    code: String,
) -> Result<(), String> {
    let origin = portal_origin();
    let c = state.get(&challenge_id).ok_or_else(|| {
        "That sign-in expired. Enter your password again for a fresh code.".to_string()
    })?;
    let res = c
        .post(format!("{origin}/api/auth/two-factor/verify"))
        .json(&serde_json::json!({
            "challenge_id": challenge_id,
            "code": code,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if !res.status().is_success() {
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        if status == 410 {
            return Err("That code window expired. Sign in again for a fresh code.".to_string());
        }
        if status == 423 {
            return Err("Too many wrong codes. Try again in 15 minutes.".to_string());
        }
        return Err(portal_error(&text, "That code didn't match. Try again."));
    }
    state.remove(&challenge_id);
    let code = mint_pairing_with(&c, &origin).await?;
    pair_machine(code).await
}

/// Start TOTP enrollment for a setup-blocked login (no session yet — the
/// setup challenge from sign_in authorizes this and nothing else).
/// Returns the secret + otpauth URL the UI shows for the authenticator.
#[tauri::command]
async fn totp_setup_enroll(setup_challenge_id: String) -> Result<String, String> {
    let origin = portal_origin();
    let res = client()?
        .post(format!("{origin}/api/auth/two-factor/setup"))
        .json(&serde_json::json!({
            "enrollment_challenge_id": setup_challenge_id,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(portal_error(&text, "Could not start authenticator setup."));
    }
    res.text().await.map_err(|e| format!("Read failed: {e}"))
}

/// Confirm TOTP enrollment. Returns the one-time recovery codes JSON —
/// the UI must show them once; the portal never will again.
#[tauri::command]
async fn totp_confirm_enroll(setup_challenge_id: String, code: String) -> Result<String, String> {
    let origin = portal_origin();
    let res = client()?
        .post(format!("{origin}/api/auth/two-factor/confirm"))
        .json(&serde_json::json!({
            "enrollment_challenge_id": setup_challenge_id,
            "code": code,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(portal_error(
            &text,
            "That code didn't match. Check the authenticator and try again.",
        ));
    }
    res.text().await.map_err(|e| format!("Read failed: {e}"))
}

async fn mint_pairing_with(c: &reqwest::Client, origin: &str) -> Result<String, String> {
    let pair_res = c
        .post(format!("{origin}/api/auth/pairing"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let pair_text = pair_res.text().await.unwrap_or_default();
    let parsed: PairingResponse = serde_json::from_str(&pair_text)
        .map_err(|_| "Portal returned an unreadable response.".to_string())?;
    parsed
        .code
        .ok_or_else(|| portal_error(&pair_text, "Could not mint a pairing code. Sign in first."))
}

#[tauri::command]
async fn pair_machine(pairing_code: String) -> Result<(), String> {
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
    persist_pairing(&token, &origin, parsed.identity.as_ref())?;
    Ok(())
}

/// Path of the launcher-owned identity file. `STRIA_PORTAL_JSON` overrides
/// it for tests and dev — the same override Works already honours, so both
/// sides always read the same file.
fn portal_json_path() -> Result<std::path::PathBuf, String> {
    if let Ok(p) = std::env::var("STRIA_PORTAL_JSON") {
        if !p.trim().is_empty() {
            return Ok(std::path::PathBuf::from(p));
        }
    }
    Ok(home_dir()?.join(".stria").join("portal.json"))
}

/// Build the durable identity record. This is the ONE place the pairing
/// response becomes local state: the machine token stays the only secret,
/// and the employee (userEmail), the customer (orgId/orgName), and the
/// device (machineId) are separate fields — a team label never stands in
/// for an organization.
fn pairing_record(
    machine_token: &str,
    portal_origin: &str,
    identity: Option<&PairingIdentity>,
) -> serde_json::Value {
    let mut record = serde_json::json!({
        "schemaVersion": 2,
        "portalUrl": portal_origin,
        "machineId": machine_id(),
        "machineToken": machine_token,
        "platform": platform_name(),
        "arch": arch_name(),
        "launcherVersion": env!("CARGO_PKG_VERSION"),
        "pairedAt": chrono_now_rfc3339(),
    });
    if let Some(id) = identity {
        for (key, value) in [
            ("orgId", id.org_id.trim()),
            ("orgName", id.org_name.trim()),
            ("userEmail", id.user_email.trim()),
            ("teamId", id.team_id.trim()),
        ] {
            if !value.is_empty() {
                record[key] = serde_json::Value::String(value.to_string());
            }
        }
    }
    record
}

/// The non-secret projection other processes (and the UI) may read.
/// Deliberately built by allowlist so `machineToken` can never be added by
/// accident when the record grows.
fn public_identity(record: &serde_json::Value) -> serde_json::Value {
    let str_at = |key: &str| {
        record
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    serde_json::json!({
        "paired": !str_at("machineToken").is_empty(),
        "portalUrl": str_at("portalUrl"),
        "machineId": str_at("machineId"),
        "orgId": str_at("orgId"),
        "orgName": str_at("orgName"),
        "userEmail": str_at("userEmail"),
        "teamId": str_at("teamId"),
        "launcherVersion": str_at("launcherVersion"),
        "pairedAt": str_at("pairedAt"),
    })
}

/// Write the identity file atomically (temp + rename). A reader can never
/// observe a half-written `portal.json`; that matters because Works reads
/// it on startup and a partial file previously looked like "not paired".
fn write_portal_json(path: &std::path::Path, record: &serde_json::Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(record).unwrap_or_default() + "\n";
    std::fs::write(&tmp, body).map_err(|e| format!("write portal.json.tmp: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod portal.json.tmp: {e}"))?;
    }
    #[cfg(windows)]
    let _ = std::fs::remove_file(path); // rename never replaces on Windows
    std::fs::rename(&tmp, path).map_err(|e| format!("replace portal.json: {e}"))?;
    Ok(())
}

fn persist_pairing(
    machine_token: &str,
    portal_origin: &str,
    identity: Option<&PairingIdentity>,
) -> Result<(), String> {
    let record = pairing_record(machine_token, portal_origin, identity);
    write_portal_json(&portal_json_path()?, &record)
}

fn read_portal_json() -> Result<serde_json::Value, String> {
    let path = portal_json_path()?;
    let raw = std::fs::read_to_string(&path).map_err(|_| "Not paired.".to_string())?;
    serde_json::from_str(&raw).map_err(|_| "~/.stria/portal.json is corrupt.".to_string())
}

/// Non-secret identity for the UI and for diagnostics. Never returns the
/// machine token; callers that need it use `read_pairing_token` internally.
#[tauri::command]
fn portal_identity() -> Result<serde_json::Value, String> {
    Ok(public_identity(&read_portal_json()?))
}

fn read_pairing_token() -> Result<String, String> {
    let v = read_portal_json()?;
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
    let status = res.status();
    let text = res.text().await.map_err(|e| format!("Read failed: {e}"))?;
    if !status.is_success() {
        return Err(portal_error(&text, "Software updates are unavailable."));
    }
    Ok(text)
}

#[tauri::command]
async fn download_suite_asset(url: String, expected_sha256: String) -> Result<String, String> {
    let parsed = validated_asset_url(&url)?;
    if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid asset checksum.".to_string());
    }
    let file_name = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| "Asset URL has no file name.".to_string())?;
    let safe_name: String = file_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let res = client()?
        .get(parsed)
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
    let path = dir.join(safe_name);
    std::fs::write(&path, &bytes).map_err(|e| format!("write asset: {e}"))?;
    Ok(path.to_string_lossy().to_string())
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
            if k != "stria-launcher" {
                report[k] = v.clone();
            }
        }
    }
    report["stria-launcher"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
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
        .plugin(tauri_plugin_deep_link::init())
        .manage(PendingLogins(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )))
        .manage(PendingPairCode(std::sync::Mutex::new(None)))
        .setup(|app| {
            setup_deep_link(app);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            register_workspace,
            sign_in,
            verify_two_factor,
            totp_setup_enroll,
            totp_confirm_enroll,
            portal_env,
            take_pending_pair_code,
            pair_machine,
            portal_identity,
            fetch_software_updates,
            download_suite_asset,
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
        pair_machine(code).await.expect("pairing must succeed");
        let raw = std::fs::read_to_string(sandbox_home().join(".stria").join("portal.json"))
            .expect("portal.json written");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let token = v["machineToken"].as_str().unwrap_or_default();
        assert!(token.starts_with("stria_mt"), "machine token shape");
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

#[cfg(test)]
mod brand_tests {
    /// The launcher is the first surface a customer sees. Its product name and
    /// mint accent are release-contract values, not styling suggestions; the
    /// portal brand test pins the same family on the web side.
    #[test]
    fn brand_surface_names_the_launcher_and_keeps_the_mint() {
        let ui = include_str!("../../ui/index.html");
        assert!(ui.contains("<title>Stria Launcher</title>"));
        assert!(ui.contains("--mint: #7ee8b0"));
        assert!(ui.contains("Stria Works"));
        assert!(ui.contains("Stria-Pi"));
    }
}
