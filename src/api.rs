use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rand::Rng;
use reqwest::{
    Client, Method, Response, StatusCode,
    header::{ACCEPT_ENCODING, CONTENT_TYPE, COOKIE, HeaderValue, RANGE},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::{Mutex, RwLock};
use url::Url;

const SIGN_PREFIX: &str = "NDzZTVxnRKP8Z0jXg1VAMonaG8akvh";
const SIGN_KEY: &str = "6D5602D4-A342-4799-A0F0-BB795E7167D0";
const JSON_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("无法连接服务器，请检查地址、网络及 HTTPS 证书")]
    Network,
    #[error("服务器响应异常（HTTP {0}）")]
    Http(StatusCode),
    #[error("服务器返回的数据格式不兼容")]
    Format,
    #[error("服务器拒绝请求（代码 {0}）")]
    Rejected(i64),
    #[error("登录已失效，请重新授权")]
    Expired,
    #[error("授权已取消")]
    Cancelled,
    #[error("无法读写本地会话或播放记录")]
    Storage,
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn normalize_base(input: &str) -> Result<Url> {
    let input = input.trim();
    let address = if input.contains("://") {
        input.to_owned()
    } else {
        format!("http://{input}")
    };
    let mut url = http_url(&address)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::Invalid("服务器地址不能包含查询参数或片段".into()));
    }
    let path = url.path().trim_end_matches('/');
    let path = path.strip_suffix("/login").unwrap_or(path);
    let path = if path.to_ascii_lowercase().ends_with("/music") {
        path.to_owned()
    } else {
        format!("{path}/music")
    };
    url.set_path(&path);
    Ok(url)
}

fn http_url(address: &str) -> Result<Url> {
    let url = Url::parse(address)
        .map_err(|_| Error::Invalid("请输入有效的 HTTP 或 HTTPS 地址".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::Invalid(
            "请输入不含账号或密码的 HTTP 或 HTTPS 地址".into(),
        ));
    }
    Ok(url)
}

fn endpoint(base: &Url, suffix: &str) -> Url {
    let mut url = base.clone();
    url.set_path(&format!("{}{suffix}", base.path()));
    url
}

// URLSearchParams sorts by UTF-16 code units and the upstream signs the decoded
// form string. Retain repeated keys and their relative value order.
pub fn authx(method: &Method, url: &Url, body: &[u8], nonce: u32, timestamp: u128) -> String {
    let payload = if method == Method::GET {
        let mut sorted = BTreeMap::<Vec<u16>, Vec<(String, String)>>::new();
        for (key, value) in url.query_pairs() {
            if !matches!(value.as_ref(), "undefined" | "null") {
                sorted
                    .entry(key.encode_utf16().collect())
                    .or_default()
                    .push((key.into_owned(), value.into_owned()));
            }
        }
        let decoded = sorted
            .into_values()
            .flatten()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        md5::compute(decoded)
    } else {
        md5::compute(body)
    };
    let signature = md5::compute(format!(
        "{SIGN_PREFIX}_{}_{nonce}_{timestamp}_{payload:x}_{SIGN_KEY}",
        url.path()
    ));
    format!("nonce={nonce}&timestamp={timestamp}&sign={signature:x}")
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub base: Url,
    pub device_id: String,
    pub profile: String,
    pub username: String,
    pub account_id: String,
    token: String,
}

#[derive(Clone, Debug)]
pub struct Authorization {
    pub sign_in: Url,
    pub callback: Url,
}

impl Authorization {
    pub fn callback_code(&self, target: &Url) -> Option<Result<String>> {
        if target.origin() != self.callback.origin()
            || target.path() != self.callback.path()
            || !target.username().is_empty()
            || target.password().is_some()
        {
            return None;
        }
        let mut code = None;
        for (key, value) in target.query_pairs() {
            match key.as_ref() {
                "error" => return Some(Err(Error::Invalid("NAS 拒绝了授权".into()))),
                "code" if code.is_none() && !value.is_empty() => code = Some(value.into_owned()),
                "code" => return Some(Err(Error::Invalid("NAS 返回了无效的授权码".into()))),
                _ => {}
            }
        }
        Some(code.ok_or_else(|| Error::Invalid("NAS 未返回授权码".into())))
    }

    pub fn permits(&self, target: &Url) -> bool {
        target.origin() == self.sign_in.origin()
            && target.username().is_empty()
            && target.password().is_none()
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    code: i64,
    data: Option<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(rename = "nasOAuth")]
    nas_o_auth: Option<NasOAuth>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NasOAuth {
    client_id: String,
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Exchange {
    user_token: String,
    user: Option<User>,
}

#[derive(Default, Deserialize)]
struct User {
    guid: Option<String>,
    username: Option<String>,
    nickname: Option<String>,
    name: Option<String>,
}

#[derive(Clone)]
pub struct Api {
    client: Client,
}

impl Api {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .use_native_tls()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(8))
            .read_timeout(Duration::from_secs(15))
            .user_agent(concat!("FnMusic/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| Error::Network)?;
        Ok(Self { client })
    }

    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<Vec<u8>>,
        token: Option<&str>,
        range: Option<HeaderValue>,
    ) -> Result<Response> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let nonce = rand::rng().random_range(100_000..1_000_000);
        let signature = authx(
            &method,
            &url,
            body.as_deref().unwrap_or_default(),
            nonce,
            timestamp,
        );
        let mut request = self
            .client
            .request(method, url)
            .header("authx", signature)
            .header(ACCEPT_ENCODING, "identity");
        if let Some(token) = token {
            request = request.header(COOKIE, format!("music-token={token}"));
        }
        if let Some(range) = range {
            request = request.header(RANGE, range);
        }
        if let Some(body) = body {
            request = request.header(CONTENT_TYPE, "application/json").body(body);
        }
        request.send().await.map_err(|_| Error::Network)
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        body: Option<Vec<u8>>,
        token: Option<&str>,
    ) -> Result<Option<T>> {
        let response = self.request(method, url, body, token, None).await?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED && token.is_some() {
            return Err(Error::Expired);
        }
        if !status.is_success() {
            return Err(Error::Http(status));
        }
        let envelope: Envelope<serde_json::Value> =
            serde_json::from_slice(&limited_body(response, JSON_LIMIT).await?)
                .map_err(|_| Error::Format)?;
        match envelope.code {
            0 => envelope
                .data
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| Error::Format),
            99999 => Err(Error::Expired),
            code => Err(Error::Rejected(code)),
        }
    }

    pub async fn authorization(&self, base: &Url) -> Result<Authorization> {
        let config: Config = self
            .json(
                Method::GET,
                endpoint(base, "/api/v1/sys/config"),
                None,
                None,
            )
            .await?
            .ok_or(Error::Format)?;
        let oauth = config
            .nas_o_auth
            .filter(|cfg| !cfg.client_id.is_empty())
            .ok_or_else(|| Error::Invalid("该服务器未开启 NAS 账号授权".into()))?;
        let auth_base = oauth
            .url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| base.origin().ascii_serialization());
        let auth_base = http_url(&auth_base)?;
        if auth_base.query().is_some()
            || auth_base.fragment().is_some()
            || (base.scheme() == "https" && auth_base.scheme() != "https")
        {
            return Err(Error::Invalid("服务器提供的 NAS 授权地址不安全".into()));
        }
        let callback = endpoint(base, "/oauth/result");
        let mut sign_in = auth_base;
        sign_in.set_path(&format!("{}/signin", sign_in.path().trim_end_matches('/')));
        sign_in
            .query_pairs_mut()
            .append_pair("client_id", &oauth.client_id)
            .append_pair("redirect_uri", callback.as_str())
            .append_pair("app_name", "飞牛音乐");
        Ok(Authorization { sign_in, callback })
    }

    pub async fn exchange(
        &self,
        base: Url,
        device_id: String,
        profile: String,
        code: String,
    ) -> Result<Credentials> {
        let body = serde_json::to_vec(&serde_json::json!({ "code": code, "deviceId": device_id }))
            .map_err(|_| Error::Format)?;
        let result: Exchange = self
            .json(
                Method::POST,
                endpoint(&base, "/api/v1/user/auth-login"),
                Some(body),
                None,
            )
            .await?
            .ok_or(Error::Format)?;
        if result.user_token.is_empty()
            || !result
                .user_token
                .bytes()
                .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
        {
            return Err(Error::Format);
        }
        let User {
            guid,
            username,
            nickname,
            name,
        } = result.user.unwrap_or_default();
        let username = username.filter(|s| !s.is_empty());
        let account_id = guid
            .filter(|s| !s.is_empty())
            .or_else(|| username.clone())
            .unwrap_or_else(|| profile.clone());
        let username = username
            .or(nickname)
            .or(name)
            .unwrap_or_else(|| "NAS 用户".into());
        Ok(Credentials {
            base,
            device_id,
            profile,
            username,
            account_id,
            token: result.user_token,
        })
    }
}

pub async fn limited_body(mut response: Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(Error::Format);
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
        if chunk.len() > limit - data.len() {
            return Err(Error::Format);
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

pub type AuthFuture = Pin<Box<dyn Future<Output = Result<String>> + Send>>;

pub trait SessionHooks: Send + Sync {
    fn authorize(&self, request: Authorization, profile: String, silent: bool) -> AuthFuture;
    fn save(&self, credentials: &Credentials) -> Result<()>;
}

#[derive(Clone)]
pub struct Session {
    state: Arc<SessionState>,
}

struct SessionState {
    api: Api,
    credentials: RwLock<(u64, Credentials)>,
    hooks: Box<dyn SessionHooks>,
    // Serialize renewals and logout. An already renewed token is reused by waiting requests.
    renewal: Mutex<Option<(Instant, Error)>>,
    expired: AtomicBool,
    closed: AtomicBool,
}

impl Session {
    pub fn new(api: Api, credentials: Credentials, hooks: Box<dyn SessionHooks>) -> Self {
        Self {
            state: Arc::new(SessionState {
                api,
                credentials: RwLock::new((0, credentials)),
                hooks,
                renewal: Mutex::new(None),
                expired: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            }),
        }
    }

    pub fn expired(&self) -> bool {
        self.state.expired.load(Ordering::Acquire)
    }

    fn check_active(&self) -> Result<()> {
        if self.expired() || self.state.closed.load(Ordering::Acquire) {
            return Err(Error::Expired);
        }
        Ok(())
    }

    async fn refresh(&self, failed_revision: u64) -> Result<Credentials> {
        let mut previous_error = self.state.renewal.lock().await;
        self.check_active()?;
        let (revision, current) = self.state.credentials.read().await.clone();
        if revision != failed_revision {
            return Ok(current);
        }
        if let Some((when, error)) = previous_error.as_ref()
            && when.elapsed() < Duration::from_secs(2)
        {
            return Err(error.clone());
        }
        let renew = async {
            let request = self.state.api.authorization(&current.base).await?;
            self.check_active()?;
            let code = self
                .state
                .hooks
                .authorize(request, current.profile.clone(), true)
                .await?;
            self.check_active()?;
            let updated = self
                .state
                .api
                .exchange(
                    current.base.clone(),
                    current.device_id.clone(),
                    current.profile.clone(),
                    code,
                )
                .await?;
            if updated.account_id != current.account_id {
                return Err(Error::Expired);
            }
            self.check_active()?;
            self.state.hooks.save(&updated)?;
            Ok(updated)
        };
        match renew.await {
            Ok(updated) => {
                *previous_error = None;
                *self.state.credentials.write().await = (revision + 1, updated.clone());
                Ok(updated)
            }
            Err(error @ (Error::Network | Error::Storage)) => {
                *previous_error = Some((Instant::now(), error.clone()));
                Err(error)
            }
            Err(_) => {
                self.state.expired.store(true, Ordering::Release);
                Err(Error::Expired)
            }
        }
    }

    async fn with_token<T, F, Fut>(&self, mut operation: F) -> Result<T>
    where
        F: FnMut(Credentials) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        self.check_active()?;
        let (revision, current) = self.state.credentials.read().await.clone();
        let result = match operation(current).await {
            Err(Error::Expired) => {
                // Keep the shared renewal alive when a player stops an HTTP range request.
                let session = self.clone();
                let updated = tokio::spawn(async move { session.refresh(revision).await })
                    .await
                    .map_err(|_| Error::Network)??;
                let result = operation(updated).await;
                if matches!(result, Err(Error::Expired)) {
                    self.state.expired.store(true, Ordering::Release);
                }
                result
            }
            other => other,
        };
        self.check_active()?;
        result
    }

    pub async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Result<Option<T>> {
        self.with_token(|credentials| {
            let mut url = endpoint(&credentials.base, path);
            if !query.is_empty() {
                url.query_pairs_mut().extend_pairs(query);
            }
            let method = method.clone();
            let body = body.clone();
            async move {
                self.state
                    .api
                    .json(method, url, body, Some(&credentials.token))
                    .await
            }
        })
        .await
    }

    pub async fn validate(&self) -> Result<()> {
        self.json::<serde_json::Value>(Method::GET, "/api/v1/user/me", &[], None)
            .await?;
        Ok(())
    }

    pub async fn cover(&self, cover_id: &str, size: &str) -> Result<Vec<u8>> {
        let response = self
            .resource(
                Method::GET,
                "/api/v1/static/cover",
                &[("coverId", cover_id), ("size", size)],
                None,
            )
            .await?;
        if !response.status().is_success() {
            return Err(Error::Http(response.status()));
        }
        limited_body(response, 12 * 1024 * 1024).await
    }

    pub async fn stream(
        &self,
        method: Method,
        id: &str,
        quality: Option<&str>,
        range: Option<HeaderValue>,
    ) -> Result<Response> {
        let mut query = vec![("guid", id)];
        if let Some(quality) = quality {
            query.push(("quality", quality));
        }
        self.resource(method, "/api/v1/track/stream", &query, range)
            .await
    }

    async fn resource(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        range: Option<HeaderValue>,
    ) -> Result<Response> {
        self.with_token(|credentials| {
            let mut url = endpoint(&credentials.base, path);
            url.query_pairs_mut().extend_pairs(query);
            let method = method.clone();
            let range = range.clone();
            async move {
                let head = method == Method::HEAD;
                let response = self
                    .state
                    .api
                    .request(method, url, None, Some(&credentials.token), range)
                    .await?;
                if response.status() == StatusCode::UNAUTHORIZED {
                    return Err(Error::Expired);
                }
                if response.status().is_success()
                    && !head
                    && response
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.starts_with("application/json"))
                {
                    let status = response.status();
                    let bytes = limited_body(response, JSON_LIMIT).await?;
                    if let Ok(envelope) =
                        serde_json::from_slice::<Envelope<serde_json::Value>>(&bytes)
                    {
                        return match envelope.code {
                            99999 => Err(Error::Expired),
                            code => Err(Error::Rejected(code)),
                        };
                    }
                    return Err(Error::Http(status));
                }
                Ok(response)
            }
        })
        .await
    }

    pub async fn logout(&self) {
        self.state.closed.store(true, Ordering::Release);
        let _guard = self.state.renewal.lock().await;
        let (_, current) = &*self.state.credentials.read().await;
        let logout = self.state.api.json::<serde_json::Value>(
            Method::POST,
            endpoint(&current.base, "/api/v1/user/logout"),
            Some(b"{}".to_vec()),
            Some(&current.token),
        );
        let _ = tokio::time::timeout(Duration::from_secs(3), logout).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_address_handles_proxy_prefix_and_ipv6() {
        assert_eq!(
            normalize_base(" [::1]:5666/ ").unwrap().as_str(),
            "http://[::1]:5666/music"
        );
        assert_eq!(
            normalize_base("HTTPS://NAS.example/proxy/music/")
                .unwrap()
                .as_str(),
            "https://nas.example/proxy/music"
        );
        assert_eq!(
            normalize_base("http://nas:5666/login").unwrap().as_str(),
            "http://nas:5666/music"
        );
        for invalid in [
            "",
            "http://a/?secret=1",
            "file:///x",
            "http://user:secret@nas/",
        ] {
            assert!(normalize_base(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn signature_matches_upstream_vectors() {
        let url =
            Url::parse("http://nas:5666/music/api/v1/album/list?sort=name,asc&page=1&size=500")
                .unwrap();
        assert_eq!(
            authx(&Method::GET, &url, &[], 123456, 1700000000000),
            "nonce=123456&timestamp=1700000000000&sign=740518b05883cab5a1208ed659ae2086"
        );
        let url = Url::parse("http://nas:5666/music/api/v1/user/password-login").unwrap();
        assert_eq!(
            authx(
                &Method::POST,
                &url,
                br#"{"username":"demo"}"#,
                654321,
                1700000000001
            ),
            "nonce=654321&timestamp=1700000000001&sign=8a6cd92357bffbd92bdf63bf8edb8217"
        );
    }

    #[test]
    fn callback_must_match_origin_and_path() {
        let auth = Authorization {
            sign_in: Url::parse("https://login.nas/signin").unwrap(),
            callback: Url::parse("https://nas/music/oauth/result").unwrap(),
        };
        let code = |url| auth.callback_code(&Url::parse(url).unwrap());
        assert_eq!(
            code("https://nas/music/oauth/result?code=a%2Bb")
                .unwrap()
                .unwrap(),
            "a+b"
        );
        assert!(
            code("https://nas/music/oauth/result?code=x&code=y")
                .unwrap()
                .is_err()
        );
        assert!(
            code("https://nas/music/oauth/result?error=denied")
                .unwrap()
                .is_err()
        );
        assert!(code("https://nas/music/oauth/result-spoof?code=x").is_none());
        assert!(code("https://nas.attacker/music/oauth/result?code=x").is_none());
        assert!(code("https://nas:444/music/oauth/result?code=x").is_none());
    }
}
