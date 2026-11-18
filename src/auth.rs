//! Microsoft (device-code OAuth2 → Xbox Live → XSTS → Minecraft services)
//! and offline accounts, with a locally persisted refresh token so the login
//! survives restarts.

use crate::net;
use crate::progress::{Event, ProgressSender};
use crate::util;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Public client used by the launcher. This is the well known community client
/// id (also used by PrismLauncher) — Microsoft's old `00000000402b5328` id from
/// the legacy launcher was decommissioned and now answers AADSTS700016.
/// Replace it in settings with your own Entra application id if you run your own build.
pub const DEFAULT_CLIENT_ID: &str = "c36a9fb6-4f2a-41ff-90bd-ae7cc92031eb";

/// Ids that are tried when the configured one is rejected. Keep this short:
/// each entry is a public application owned by another project.
const FALLBACK_CLIENT_IDS: &[&str] = &["00000000402b5328", "000000004c12ae6f"];

const SCOPE: &str = "XboxLive.signin offline_access";
const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Microsoft,
    Offline,
}

impl Default for AccountKind {
    fn default() -> Self {
        AccountKind::Offline
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Account {
    /// Minecraft profile uuid, no dashes.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: AccountKind,
    /// Microsoft refresh token, encrypted at rest with DPAPI on Windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// unix seconds when the minecraft access token expires.
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<String>,
}

impl Account {
    pub fn offline(name: &str) -> Self {
        Self {
            id: util::offline_uuid(name),
            name: name.to_string(),
            kind: AccountKind::Offline,
            refresh_token: None,
            access_token: Some("0".into()),
            expires_at: i64::MAX / 2,
            skin: None,
        }
    }

    pub fn is_offline(&self) -> bool {
        self.kind == AccountKind::Offline
    }

    pub fn token_valid(&self) -> bool {
        self.is_offline() || self.expires_at - 60 > util::now_ts()
    }

    pub fn seal(&mut self) {
        if let Some(rt) = self.refresh_token.take() {
            self.refresh_token = Some(secure::protect(&rt));
        }
    }

    pub fn unseal(&mut self) {
        if let Some(rt) = self.refresh_token.take() {
            self.refresh_token = secure::unprotect(&rt).ok();
        }
    }
}

/// Result of a full login chain.
#[derive(Debug, Clone)]
pub struct McProfile {
    pub uuid: String,
    pub name: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: i64,
    pub skin: Option<String>,
    /// The client id that actually worked (fallback included), so the caller can
    /// store it and reuse it later when refreshing.
    pub used_client_id: Option<String>,
}

impl McProfile {
    pub fn into_account(self) -> Account {
        Account {
            id: self.uuid,
            name: self.name,
            kind: AccountKind::Microsoft,
            refresh_token: self.refresh_token.map(|t| secure::protect(&t)),
            access_token: Some(self.access_token),
            expires_at: self.expires_at,
            skin: self.skin,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
    #[serde(default)]
    pub message: String,
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XblResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: XblClaims,
}

#[derive(Debug, Deserialize)]
struct XblClaims {
    xui: Vec<Xui>,
}

#[derive(Debug, Deserialize)]
struct Xui {
    #[serde(default)]
    uhs: String,
}

#[derive(Debug, Deserialize)]
struct McTokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
pub struct McProfileResponse {
    pub id: String,
    pub name: String,
    #[serde(default)]
    skins: Vec<Skin>,
}

#[derive(Debug, Deserialize)]
struct Skin {
    #[serde(default)]
    url: String,
    #[serde(default)]
    state: String,
}

impl McProfileResponse {
    fn skin_url(&self) -> Option<String> {
        self.skins
            .iter()
            .find(|s| s.state == "ACTIVE")
            .map(|s| s.url.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| self.skins.first().map(|s| s.url.clone()))
    }
}

/// Step 1 of the device-code flow: ask Microsoft for a user code.
pub async fn start_device_flow(
    client: &reqwest::Client,
    client_id: &str,
) -> Result<DeviceCode> {
    let resp = client
        .post(DEVICE_CODE_URL)
        .form(&[("client_id", client_id), ("scope", SCOPE)])
        .send()
        .await
        .context("requesting a device code")?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        if text.contains("AADSTS700016") || text.contains("unauthorized_client") {
            bail!(
                "Microsoft 应用 ID 不可用（AADSTS700016）：{client_id} 不存在或未开放公共客户端。\
                 请到「设置 → 微软 Client ID」填一个你自己在 Azure 注册的公共客户端 ID。"
            );
        }
        bail!("device code 请求失败: HTTP {status} {text}");
    }
    serde_json::from_str::<DeviceCode>(&text).context("parsing device code response")
}

async fn poll_token(
    client: &reqwest::Client,
    client_id: &str,
    device_code: &str,
) -> Result<TokenResponse> {
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", client_id),
            ("device_code", device_code),
        ])
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    let parsed: TokenResponse = serde_json::from_str(&text)
        .with_context(|| format!("parsing token poll response: {text}"))?;
    if let Some(err) = &parsed.error {
        // authorization_pending / slow_down are expected while the user is in the browser
        return Err(anyhow!("{err}|{}", parsed.error_description.unwrap_or_default()));
    }
    if !status.is_success() {
        bail!("token poll failed: HTTP {status} {text}");
    }
    Ok(parsed)
}

async fn refresh_ms_token(
    client: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse> {
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("scope", SCOPE),
        ])
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        bail!("refresh token rejected: HTTP {status} {text}");
    }
    let parsed: TokenResponse =
        serde_json::from_str(&text).context("parsing refresh response")?;
    if parsed.access_token.is_none() {
        bail!("refresh response had no access token");
    }
    Ok(parsed)
}

/// Full device-code login. Emits `Event::DeviceCode` so the UI can show the code.
pub async fn login_device_code(
    client: &reqwest::Client,
    client_id: &str,
    tx: &ProgressSender,
) -> Result<McProfile> {
    // Try the configured app id first, then the known community fallbacks, so a
    // decommissioned id (AADSTS700016) does not dead-end the user.
    let configured = client_id.trim().to_string();
    let mut ids: Vec<String> = Vec::new();
    if !configured.is_empty() {
        ids.push(configured.clone());
    }
    for f in FALLBACK_CLIENT_IDS {
        let s = f.to_string();
        if !ids.contains(&s) {
            ids.push(s);
        }
    }

    let mut chosen: Option<(String, DeviceCode)> = None;
    let mut last_err: Option<anyhow::Error> = None;
    for id in ids {
        match start_device_flow(client, &id).await {
            Ok(dc) => {
                chosen = Some((id, dc));
                break;
            }
            Err(e) => {
                tx.log(format!("client id {id} 不可用: {e}"));
                last_err = Some(e);
            }
        }
    }
    let (used_id, dc) = chosen
        .ok_or_else(|| last_err.unwrap_or_else(|| anyhow!("无法发起微软设备码登录")))?;
    if !configured.is_empty() && used_id != configured {
        tx.log(format!("已自动切换到可用的 Microsoft 应用 ID: {used_id}"));
    }

    tx.send(Event::DeviceCode {
        user_code: dc.user_code.clone(),
        verification_uri: dc.verification_uri.clone(),
        message: if dc.message.is_empty() {
            format!("请在浏览器打开 {} 并输入代码 {}", dc.verification_uri, dc.user_code)
        } else {
            dc.message.clone()
        },
    });
    tx.log(format!(
        "登录代码 {} · 打开 {}",
        dc.user_code, dc.verification_uri
    ));

    let deadline = std::time::Instant::now() + Duration::from_secs(dc.expires_in.max(60));
    let mut interval = dc.interval.max(2);
    loop {
        if std::time::Instant::now() > deadline {
            bail!("登录超时，请重试");
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
        match poll_token(client, &used_id, &dc.device_code).await {
            Ok(tok) => {
                let ms_token = tok
                    .access_token
                    .ok_or_else(|| anyhow!("token response had no access_token"))?;
                return complete_login(client, &ms_token, tok.refresh_token, Some(used_id.clone()))
                    .await;
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.starts_with("authorization_pending") {
                    continue;
                }
                if msg.starts_with("slow_down") {
                    interval += 5;
                    continue;
                }
                if msg.starts_with("expired_token") {
                    bail!("登录代码已过期，请重新开始");
                }
                if msg.starts_with("authorization_declined") {
                    bail!("用户取消了授权");
                }
                return Err(e);
            }
        }
    }
}

/// Use a stored refresh token to mint a fresh Minecraft session.
pub async fn login_refresh_token(
    client: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<McProfile> {
    let tok = refresh_ms_token(client, client_id, refresh_token).await?;
    let ms_token = tok
        .access_token
        .ok_or_else(|| anyhow!("refresh response had no access token"))?;
    let fallback = Some(refresh_token.to_string());
    complete_login(client, &ms_token, tok.refresh_token.or(fallback), None).await
}

/// XBL → XSTS → Minecraft. Shared by both login paths.
async fn complete_login(
    client: &reqwest::Client,
    ms_access_token: &str,
    refresh_token: Option<String>,
    used_client_id: Option<String>,
) -> Result<McProfile> {
    // 1. Xbox Live user token
    let body = serde_json::json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("d={ms_access_token}")
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT"
    });
    let xbl: XblResponse = client
        .post("https://user.auth.xboxlive.com/user/authenticate")
        .json(&body)
        .send()
        .await
        .context("Xbox Live authentication")?
        .json()
        .await
        .context("parsing Xbox Live response")?;

    // 2. XSTS token for the Minecraft relying party
    let body = serde_json::json!({
        "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl.token] },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT"
    });
    let resp = client
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .json(&body)
        .send()
        .await
        .context("XSTS authorization")?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        if text.contains("2148916233") {
            bail!("该微软账号还没有 Xbox 档案，请先在 xbox.com 创建角色");
        }
        if text.contains("2148916238") {
            bail!("该账号被判定为未成年，需要家长同意（Xbox 家庭组）");
        }
        if text.contains("2148916235") {
            bail!("该账号所在地区不支持 Xbox Live");
        }
        bail!("XSTS 失败: HTTP {status} {text}");
    }
    let xsts: XblResponse = serde_json::from_str(&text).context("parsing XSTS response")?;
    let uhs = xsts
        .display_claims
        .xui
        .first()
        .map(|x| x.uhs.clone())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("XSTS response carried no user hash"))?;

    // 3. Minecraft services token
    let body = serde_json::json!({ "identityToken": format!("XBL3.0 x={uhs};{}", xsts.token) });
    let resp = client
        .post("https://api.minecraftservices.com/authentication/login_with_xbox")
        .json(&body)
        .send()
        .await
        .context("Minecraft services login")?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        bail!("Minecraft 登录失败: HTTP {status} {text}");
    }
    let mc: McTokenResponse = serde_json::from_str(&text).context("parsing Minecraft token")?;

    // 4. profile (also proves the account owns the game)
    let profile: McProfileResponse = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(&mc.access_token)
        .send()
        .await
        .context("fetching Minecraft profile")?
        .json()
        .await
        .context("parsing Minecraft profile")?;

    let skin = profile.skin_url();
    Ok(McProfile {
        uuid: profile.id,
        name: profile.name,
        access_token: mc.access_token,
        refresh_token,
        expires_at: util::now_ts() + mc.expires_in as i64,
        skin,
        used_client_id,
    })
}

/// Give back a usable Minecraft access token for the account, refreshing if needed.
pub async fn ensure_session(
    client: &reqwest::Client,
    client_id: &str,
    account: &Account,
    tx: &ProgressSender,
) -> Result<String> {
    if account.is_offline() {
        return Ok("0".into());
    }
    if account.token_valid() {
        if let Some(tok) = &account.access_token {
            return Ok(tok.clone());
        }
    }
    let rt = account
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow!("账号没有保存的刷新令牌，请重新登录"))?;
    tx.log(format!("刷新 {} 的登录令牌…", account.name));
    let profile = login_refresh_token(client, client_id, rt).await?;
    Ok(profile.access_token)
}

/// Encrypt small secrets at rest: DPAPI on Windows, base64 elsewhere.
pub mod secure {
    use anyhow::Result;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;

    pub fn protect(plain: &str) -> String {
        #[cfg(windows)]
        {
            if let Ok(blob) = dpapi_protect(plain.as_bytes()) {
                return format!("dpapi:{}", B64.encode(blob));
            }
        }
        format!("b64:{}", B64.encode(plain.as_bytes()))
    }

    pub fn unprotect(stored: &str) -> Result<String> {
        if let Some(rest) = stored.strip_prefix("dpapi:") {
            let raw = B64.decode(rest)?;
            #[cfg(windows)]
            {
                let plain = dpapi_unprotect(&raw)?;
                return Ok(String::from_utf8(plain)?);
            }
            #[cfg(not(windows))]
            {
                let _ = raw;
                anyhow::bail!("DPAPI blob but this build is not for Windows")
            }
        }
        if let Some(rest) = stored.strip_prefix("b64:") {
            let raw = B64.decode(rest)?;
            return Ok(String::from_utf8(raw)?);
        }
        Ok(stored.to_string())
    }

    #[cfg(windows)]
    fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Cryptography::{
            CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };

        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptProtectData(
                &mut input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            anyhow::bail!("CryptProtectData failed");
        }
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe {
            LocalFree(out.pbData as *mut core::ffi::c_void);
        }
        Ok(bytes)
    }

    #[cfg(windows)]
    fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Cryptography::{
            CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };

        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptUnprotectData(
                &mut input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            anyhow::bail!("CryptUnprotectData failed");
        }
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe {
            LocalFree(out.pbData as *mut core::ffi::c_void);
        }
        Ok(bytes)
    }

    #[allow(dead_code)]
    fn unused(_: &str) -> Result<()> {
        let _ = net_unused();
        Ok(())
    }

    #[allow(dead_code)]
    fn net_unused() -> usize {
        0
    }
}

#[allow(dead_code)]
fn _keep_net_import() {
    let _ = net::USER_AGENT;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_round_trip() {
        let secret = "M.R3_BAY.abcdef-1234567890";
        let stored = secure::protect(secret);
        assert!(
            stored.starts_with("dpapi:") || stored.starts_with("b64:"),
            "unexpected envelope: {stored}"
        );
        assert_ne!(stored, secret, "secret must not be stored in clear");
        assert_eq!(secure::unprotect(&stored).unwrap(), secret);
    }

    #[test]
    fn plain_values_still_load() {
        assert_eq!(secure::unprotect("literal-token").unwrap(), "literal-token");
    }

    #[test]
    fn account_seal_hides_refresh_token() {
        let mut a = Account {
            id: "abc".into(),
            name: "Steve".into(),
            kind: AccountKind::Microsoft,
            refresh_token: Some("very-secret".into()),
            access_token: Some("tok".into()),
            expires_at: 0,
            skin: None,
        };
        a.seal();
        let sealed = a.refresh_token.clone().unwrap();
        assert!(!sealed.contains("very-secret"));
        a.unseal();
        assert_eq!(a.refresh_token.as_deref(), Some("very-secret"));
    }

    #[test]
    fn offline_account_is_stable() {
        let a = Account::offline("Notch");
        assert_eq!(a.kind, AccountKind::Offline);
        assert_eq!(a.id.len(), 32);
        assert!(a.token_valid());
        assert_eq!(a.id, Account::offline("Notch").id);
    }
}