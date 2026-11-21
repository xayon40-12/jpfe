//! Dropbox as a sync [`Provider`]: the app's own folder in the user's Dropbox (App folder access), signed in
//! with OAuth 2 and PKCE, so no app secret is needed anywhere (see `docs/dropbox-setup.md`).
//!
//! - **Web:** the page goes to Dropbox, which sends it back to the same address with a code; the PKCE
//!   verifier waits in the tab's session storage, and the code is exchanged when the page loads again.
//! - **Desktop:** Dropbox's no-redirect flow: the browser shows a code, which the user pastes into the app.
//!
//! The refresh token, which has no fixed expiry, is kept on the device (local storage on the web, a file in
//! the data folder on the desktop), never in the synced state. Access tokens last 4 hours and are renewed
//! with it when they expire.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::engine::{Done, Fetched, Listing, Provider, ProviderError, WriteMode};

/// The app key of the Dropbox app, a public value for an app using PKCE; set when building, with the
/// `JPFE_DROPBOX_APP_KEY` environment variable. Without it, sync is not offered.
pub const APP_KEY: Option<&str> = match option_env!("JPFE_DROPBOX_APP_KEY") {
    Some(key) if !key.is_empty() => Some(key),
    _ => None,
};

const AUTHORIZE: &str = "https://www.dropbox.com/oauth2/authorize";
const TOKEN: &str = "https://api.dropboxapi.com/oauth2/token";
const API: &str = "https://api.dropboxapi.com/2";
const CONTENT: &str = "https://content.dropboxapi.com/2";
/// Renew the access token when it has less than this left (seconds).
const EXPIRY_MARGIN: f64 = 300.0;

fn now_seconds() -> f64 {
    crate::state::now_millis() as f64 / 1000.0
}

/// Base64 for URLs, without padding (RFC 4648 §5), as PKCE wants.
pub fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..chunk.len() + 1 {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Random bytes from the operating system (the browser's crypto on the web).
fn random_bytes(len: usize) -> Vec<u8> {
    #[cfg(target_arch = "wasm32")]
    {
        let mut bytes = vec![0u8; len];
        if let Some(crypto) = web_sys::window().and_then(|w| w.crypto().ok()) {
            let _ = crypto.get_random_values_with_u8_array(&mut bytes);
        }
        bytes
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // SipHash keyed with 128 random bits from the OS, over a counter
        use std::hash::{BuildHasher, Hasher};
        let state = std::collections::hash_map::RandomState::new();
        (0..len.div_ceil(8))
            .flat_map(|i| {
                let mut hasher = state.build_hasher();
                hasher.write_usize(i);
                hasher.finish().to_le_bytes()
            })
            .take(len)
            .collect()
    }
}

/// A sign-in in progress: where to send the user, and the PKCE verifier to exchange the code with.
#[derive(Clone, Debug)]
pub struct SignIn {
    pub url: String,
    pub verifier: String,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub state: String,
}

/// Start signing in; Dropbox sends the user back to `redirect` with a code, or shows the code when `None`.
pub fn sign_in(key: &str, redirect: Option<&str>) -> SignIn {
    let verifier = base64_url(&random_bytes(48));
    let state = base64_url(&random_bytes(16));
    let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));
    let mut url = format!(
        "{AUTHORIZE}?client_id={}&response_type=code&code_challenge={challenge}&code_challenge_method=S256&token_access_type=offline&state={state}",
        percent_encode(key)
    );
    if let Some(redirect) = redirect {
        url += &format!("&redirect_uri={}", percent_encode(redirect));
    }
    SignIn { url, verifier, state }
}

/// What signing in gives: the refresh token, kept on the device, and a first access token.
#[derive(Clone, Debug)]
pub struct Grant {
    pub refresh: String,
    access: Option<(String, f64)>,
}

impl Grant {
    pub fn stored(refresh: String) -> Self {
        Self { refresh, access: None }
    }
}

fn form(pairs: &[(&str, &str)]) -> Vec<u8> {
    pairs.iter().map(|(k, v)| format!("{k}={}", percent_encode(v))).collect::<Vec<_>>().join("&").into_bytes()
}

fn token_request(body: Vec<u8>) -> ehttp::Request {
    ehttp::Request::new(ehttp::Method::POST, TOKEN, &[("Content-Type", "application/x-www-form-urlencoded")]).with_body(body)
}

/// The error a token request answered with.
fn token_error(response: &ehttp::Response) -> String {
    let json: Option<Value> = serde_json::from_slice(&response.bytes).ok();
    let text = json.as_ref().and_then(|j| j["error_description"].as_str().or(j["error"].as_str()).map(str::to_string));
    text.unwrap_or_else(|| format!("HTTP {} {}", response.status, response.status_text))
}

/// Exchange the code Dropbox gave for the tokens.
pub fn exchange(key: &str, code: &str, verifier: &str, redirect: Option<&str>, done: impl FnOnce(Result<Grant, String>) + Send + 'static) {
    let mut pairs = vec![("code", code.trim()), ("grant_type", "authorization_code"), ("code_verifier", verifier), ("client_id", key)];
    if let Some(redirect) = redirect {
        pairs.push(("redirect_uri", redirect));
    }
    ehttp::fetch(token_request(form(&pairs)), move |result| {
        done(match result {
            Ok(r) if r.ok => {
                let json: Value = serde_json::from_slice(&r.bytes).unwrap_or_default();
                match json["refresh_token"].as_str() {
                    Some(refresh) => Ok(Grant {
                        refresh: refresh.to_string(),
                        access: json["access_token"].as_str().map(|a| (a.to_string(), now_seconds() + json["expires_in"].as_f64().unwrap_or(0.0))),
                    }),
                    None => Err("Dropbox gave no refresh token".into()),
                }
            }
            Ok(r) => Err(token_error(&r)),
            Err(e) => Err(e),
        })
    });
}

/// The user's Dropbox, signed in.
#[derive(Clone)]
pub struct Dropbox {
    key: String,
    grant: Arc<Mutex<Grant>>,
}

type MakeRequest = Arc<dyn Fn(&str) -> ehttp::Request + Send + Sync>;

impl Dropbox {
    pub fn new(key: &str, grant: Grant) -> Self {
        Self { key: key.to_string(), grant: Arc::new(Mutex::new(grant)) }
    }

    /// A valid access token: the current one, or a renewed one (always when `renew`).
    fn with_token(&self, renew: bool, then: impl FnOnce(Result<String, ProviderError>) + Send + 'static) {
        let refresh = {
            let grant = self.grant.lock().unwrap();
            match &grant.access {
                Some((token, expires)) if !renew && *expires - EXPIRY_MARGIN > now_seconds() => return then(Ok(token.clone())),
                _ => grant.refresh.clone(),
            }
        };
        let grant = self.grant.clone();
        let body = form(&[("grant_type", "refresh_token"), ("refresh_token", &refresh), ("client_id", &self.key)]);
        ehttp::fetch(token_request(body), move |result| {
            then(match result {
                Ok(r) if r.ok => {
                    let json: Value = serde_json::from_slice(&r.bytes).unwrap_or_default();
                    match json["access_token"].as_str() {
                        Some(token) => {
                            let expires = now_seconds() + json["expires_in"].as_f64().unwrap_or(0.0);
                            grant.lock().unwrap().access = Some((token.to_string(), expires));
                            Ok(token.to_string())
                        }
                        None => Err(ProviderError::Other("Dropbox gave no access token".into())),
                    }
                }
                // The refresh token was revoked (or the app removed)
                Ok(r) if r.status == 400 || r.status == 401 => Err(ProviderError::Auth(token_error(&r))),
                Ok(r) => Err(ProviderError::Other(token_error(&r))),
                Err(e) => Err(ProviderError::Other(e)),
            })
        });
    }

    /// Make a request with a valid access token; once more with a renewed one if Dropbox refuses the token.
    fn call(&self, make: MakeRequest, done: impl FnOnce(Result<ehttp::Response, ProviderError>) + Send + 'static) {
        let this = self.clone();
        self.with_token(false, move |token| {
            let token = match token {
                Ok(token) => token,
                Err(e) => return done(Err(e)),
            };
            ehttp::fetch(make(&token), move |result| match result {
                Ok(r) if r.status == 401 => this.with_token(true, move |token| match token {
                    Ok(token) => ehttp::fetch(make(&token), move |result| {
                        done(match result {
                            Ok(r) if r.status == 401 => Err(ProviderError::Auth(api_error(&r))),
                            Ok(r) => Ok(r),
                            Err(e) => Err(ProviderError::Other(e)),
                        })
                    }),
                    Err(e) => done(Err(e)),
                }),
                Ok(r) => done(Ok(r)),
                Err(e) => done(Err(ProviderError::Other(e))),
            });
        });
    }

    /// Call an RPC endpoint with JSON arguments; its JSON answer.
    fn rpc(&self, endpoint: &'static str, arg: Value, done: impl FnOnce(Result<Value, ProviderError>) + Send + 'static) {
        let body = arg.to_string().into_bytes();
        let make: MakeRequest = Arc::new(move |token| {
            let auth = format!("Bearer {token}");
            let headers = [("Authorization", auth.as_str()), ("Content-Type", "application/json")];
            ehttp::Request::new(ehttp::Method::POST, format!("{API}{endpoint}"), &headers).with_body(body.clone())
        });
        self.call(make, move |result| {
            done(result.and_then(|r| match r.ok {
                true => serde_json::from_slice(&r.bytes).map_err(|e| ProviderError::Other(e.to_string())),
                false => Err(classify(&r)),
            }))
        });
    }

    /// Pages of a folder listing, from `list_folder` or `list_folder/continue`.
    fn list_pages(&self, endpoint: &'static str, arg: Value, deleted_too: bool, mut files: Vec<String>, done: Done<Listing>) {
        let this = self.clone();
        self.rpc(endpoint, arg, move |result| {
            let page = match result {
                Ok(page) => page,
                Err(e) => return done(Err(e)),
            };
            for entry in page["entries"].as_array().into_iter().flatten() {
                let file = entry[".tag"] == "file" || (deleted_too && entry[".tag"] == "deleted");
                if let Some(path) = entry["path_lower"].as_str().filter(|_| file) {
                    files.push(path.to_string());
                }
            }
            let cursor = page["cursor"].as_str().unwrap_or_default().to_string();
            if page["has_more"].as_bool().unwrap_or(false) {
                this.list_pages("/files/list_folder/continue", json!({ "cursor": cursor }), deleted_too, files, done);
            } else {
                done(Ok(Listing { files, cursor }));
            }
        });
    }

    /// Revoke the access (signing out); best effort.
    pub fn revoke(&self) {
        let make: MakeRequest = Arc::new(|token| {
            let auth = format!("Bearer {token}");
            ehttp::Request::new(ehttp::Method::POST, format!("{API}/auth/token/revoke"), &[("Authorization", auth.as_str())])
        });
        self.call(make, |_| {});
    }
}

/// Dropbox's content hash of a file: the SHA-256 of the SHA-256 of each 4 MiB block (hex).
pub fn dropbox_content_hash(bytes: &[u8]) -> String {
    let mut all = Sha256::new();
    for block in bytes.chunks(4 << 20) {
        all.update(Sha256::digest(block));
    }
    all.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// What Dropbox said about a downloaded file, against what arrived: to tell a damaged download from a file
/// stored damaged.
fn download_detail(r: &ehttp::Response, meta: &Value) -> String {
    let received = dropbox_content_hash(&r.bytes);
    let stored = meta["content_hash"].as_str().unwrap_or("?");
    let same = if stored == received { "matches" } else { "differs from" };
    format!(
        "HTTP {}, Content-Type {}, Dropbox size {}, Dropbox content_hash {} ({same} what arrived: {})",
        r.status,
        r.headers.get("Content-Type").unwrap_or("none"),
        meta["size"].as_u64().map_or("?".to_string(), |s| s.to_string()),
        short(stored),
        short(&received),
    )
}

/// The first 12 characters of a hash.
pub fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

/// A Dropbox error message: its `error_summary`, or the HTTP status.
fn api_error(response: &ehttp::Response) -> String {
    let json: Option<Value> = serde_json::from_slice(&response.bytes).ok();
    match json.as_ref().and_then(|j| j["error_summary"].as_str()) {
        Some(summary) => summary.trim_end_matches(['/', '.']).to_string(),
        None => format!("HTTP {} {}", response.status, response.status_text),
    }
}

fn classify(response: &ehttp::Response) -> ProviderError {
    let summary = api_error(response);
    match response.status {
        409 if summary.starts_with("path/conflict") => ProviderError::Conflict,
        409 if summary.starts_with("reset") => ProviderError::Reset,
        401 => ProviderError::Auth(summary),
        429 => ProviderError::Other("Dropbox asks to slow down".into()),
        _ => ProviderError::Other(summary),
    }
}

/// The `Dropbox-API-Arg` header: JSON with anything outside ASCII escaped, as HTTP headers must be.
fn api_arg(arg: &Value) -> String {
    let mut out = String::new();
    for c in arg.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out += &format!("\\u{unit:04x}");
            }
        }
    }
    out
}

impl Provider for Dropbox {
    fn read(&mut self, path: &str, done: Done<Option<Fetched>>) {
        let arg = api_arg(&json!({ "path": path }));
        let make: MakeRequest = Arc::new(move |token| {
            let auth = format!("Bearer {token}");
            let headers = [("Authorization", auth.as_str()), ("Dropbox-API-Arg", arg.as_str())];
            // Books take longer than the default 30 s on a slow connection
            ehttp::Request::new(ehttp::Method::POST, format!("{CONTENT}/files/download"), &headers).with_timeout(None)
        });
        self.call(make, move |result| {
            done(result.and_then(|r| {
                if r.ok {
                    let meta: Value = r.headers.get("Dropbox-API-Result").and_then(|m| serde_json::from_str(m).ok()).unwrap_or_default();
                    let detail = download_detail(&r, &meta);
                    Ok(Some(Fetched { rev: meta["rev"].as_str().unwrap_or_default().to_string(), bytes: r.bytes, detail }))
                } else if r.status == 409 && api_error(&r).starts_with("path/not_found") {
                    Ok(None)
                } else {
                    Err(classify(&r))
                }
            }))
        });
    }

    fn write(&mut self, path: &str, bytes: Vec<u8>, mode: WriteMode, done: Done<String>) {
        let mode = match mode {
            WriteMode::Overwrite => json!("overwrite"),
            WriteMode::Add => json!("add"),
            WriteMode::Update(rev) => json!({ ".tag": "update", "update": rev }),
        };
        // `strict_conflict`: a conflict even when the file written in between has the same contents
        let arg = api_arg(&json!({ "path": path, "mode": mode, "autorename": false, "mute": true, "strict_conflict": true }));
        let bytes = Arc::new(bytes);
        let make: MakeRequest = Arc::new(move |token| {
            let auth = format!("Bearer {token}");
            let headers = [("Authorization", auth.as_str()), ("Dropbox-API-Arg", arg.as_str()), ("Content-Type", "application/octet-stream")];
            ehttp::Request::new(ehttp::Method::POST, format!("{CONTENT}/files/upload"), &headers)
                .with_body(bytes.to_vec())
                .with_timeout(None)
        });
        self.call(make, move |result| {
            done(result.and_then(|r| match r.ok {
                true => {
                    let meta: Value = serde_json::from_slice(&r.bytes).unwrap_or_default();
                    Ok(meta["rev"].as_str().unwrap_or_default().to_string())
                }
                false => Err(classify(&r)),
            }))
        });
    }

    fn list(&mut self, done: Done<Listing>) {
        self.list_pages("/files/list_folder", json!({ "path": "", "recursive": true }), false, Vec::new(), done);
    }

    fn changes(&mut self, cursor: &str, done: Done<Listing>) {
        self.list_pages("/files/list_folder/continue", json!({ "cursor": cursor }), true, Vec::new(), done);
    }

    fn delete(&mut self, path: &str, done: Done<()>) {
        self.rpc("/files/delete_v2", json!({ "path": path }), move |result| {
            done(match result {
                Err(ProviderError::Other(e)) if e.starts_with("path_lookup/not_found") => Ok(()),
                other => other.map(|_| ()),
            })
        });
    }
}

/// Where the refresh token is kept on this device, apart from the synced state.
#[cfg(not(target_arch = "wasm32"))]
fn token_file() -> Option<std::path::PathBuf> {
    dirs::data_dir().map(|d| d.join("jpfe-viewer").join("dropbox-token"))
}

/// The refresh token kept on this device, if signed in.
pub fn load_token() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    let token = web::local_storage().and_then(|s| s.get_item(web::TOKEN_KEY).ok().flatten());
    #[cfg(not(target_arch = "wasm32"))]
    let token = token_file().and_then(|f| std::fs::read_to_string(f).ok());
    token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Keep the refresh token on this device, or forget it (`None`).
pub fn save_token(token: Option<&str>) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let storage = web::local_storage().ok_or("no local storage")?;
        match token {
            Some(token) => storage.set_item(web::TOKEN_KEY, token),
            None => storage.remove_item(web::TOKEN_KEY),
        }
        .map_err(|e| format!("{e:?}"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let file = token_file().ok_or("no data folder")?;
        match token {
            Some(token) => {
                std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
                std::fs::write(&file, token).map_err(|e| e.to_string())?;
                // Readable by the user only
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
                }
                Ok(())
            }
            None => match std::fs::remove_file(&file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            },
        }
    }
}

/// The web page's side of signing in: going to Dropbox and coming back.
#[cfg(target_arch = "wasm32")]
pub mod web {
    use super::{exchange, percent_decode, sign_in, Grant};

    pub const TOKEN_KEY: &str = "jpfe-dropbox-refresh-token";
    /// The PKCE verifier and state of a sign-in in progress, kept across the trip to Dropbox
    const PENDING_KEY: &str = "jpfe-dropbox-sign-in";

    pub fn local_storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    fn session_storage() -> Option<web_sys::Storage> {
        web_sys::window()?.session_storage().ok().flatten()
    }

    /// The page's own address, without query or fragment: where Dropbox sends the user back.
    pub fn redirect_uri() -> Option<String> {
        let location = web_sys::window()?.location();
        Some(format!("{}{}", location.origin().ok()?, location.pathname().ok()?))
    }

    /// Go to Dropbox to sign in; it comes back to this page.
    pub fn start(key: &str) -> Result<(), String> {
        let redirect = redirect_uri().ok_or("no page address")?;
        let sign_in = sign_in(key, Some(&redirect));
        let storage = session_storage().ok_or("no session storage")?;
        storage.set_item(PENDING_KEY, &format!("{} {}", sign_in.verifier, sign_in.state)).map_err(|e| format!("{e:?}"))?;
        web_sys::window().ok_or("no window")?.location().set_href(&sign_in.url).map_err(|e| format!("{e:?}"))
    }

    /// If the page was opened by Dropbox after signing in: take the code from the address (removing it), and
    /// exchange it; `done` gets the result. `None` when the page was not opened by Dropbox.
    pub fn finish(key: &str, done: impl FnOnce(Result<Grant, String>) + Send + 'static) -> Option<()> {
        let window = web_sys::window()?;
        let search = window.location().search().ok()?;
        let params: Vec<(String, String)> = search
            .trim_start_matches('?')
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.to_string(), percent_decode(v)))
            .collect();
        let get = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let (code, error, state) = (get("code"), get("error"), get("state"));
        if code.is_none() && error.is_none() {
            return None;
        }
        // Remove the code from the address, so that a reload does not use it again
        if let (Some(redirect), Ok(history)) = (redirect_uri(), window.history()) {
            let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&redirect));
        }
        let pending = session_storage().and_then(|s| {
            let pending = s.get_item(PENDING_KEY).ok().flatten();
            let _ = s.remove_item(PENDING_KEY);
            pending
        });
        let pending = pending.as_deref().and_then(|p| p.split_once(' '));
        match (code, error, pending) {
            (_, Some(error), _) => {
                let description = get("error_description").unwrap_or(error);
                done(Err(if description.contains("denied") || description == "access_denied" { "Sign-in cancelled".into() } else { description }));
            }
            (Some(code), None, Some((verifier, expected))) if state.as_deref() == Some(expected) => {
                let redirect = redirect_uri();
                exchange(key, &code, verifier, redirect.as_deref(), done);
            }
            _ => done(Err("The sign-in did not come from this page; try again".into())),
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_for_urls() {
        assert_eq!(base64_url(b""), "");
        assert_eq!(base64_url(b"f"), "Zg");
        assert_eq!(base64_url(b"fo"), "Zm8");
        assert_eq!(base64_url(b"foo"), "Zm9v");
        assert_eq!(base64_url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn pkce_challenge_is_the_verifiers_hash() {
        // RFC 7636, appendix B
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(base64_url(&Sha256::digest(verifier.as_bytes())), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let a = sign_in("key", None);
        let b = sign_in("key", Some("http://localhost:8766/"));
        assert_ne!(a.verifier, b.verifier);
        assert!(a.verifier.len() >= 43 && a.verifier.len() <= 128);
        assert!(a.url.contains("token_access_type=offline") && a.url.contains("code_challenge_method=S256"));
        assert!(!a.url.contains("redirect_uri"));
        assert!(b.url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8766%2F"));
    }

    #[test]
    fn dropbox_content_hash_of_blocks() {
        // Dropbox's documented example hash for an empty file is the SHA-256 of nothing hashed once more
        assert_eq!(dropbox_content_hash(b""), format!("{:x}", Sha256::digest(b"")));
        let big = vec![7u8; (4 << 20) + 5];
        let mut all = Sha256::new();
        all.update(Sha256::digest(&big[..4 << 20]));
        all.update(Sha256::digest(&big[4 << 20..]));
        assert_eq!(dropbox_content_hash(&big), format!("{:x}", all.finalize()));
    }

    #[test]
    fn headers_are_ascii() {
        assert_eq!(api_arg(&json!({ "path": "/本.epub" })), r#"{"path":"/\u672c.epub"}"#);
        assert_eq!(percent_decode("a%2Fb+c"), "a/b c");
    }
}
