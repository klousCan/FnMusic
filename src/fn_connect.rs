//! FN Connect's public gateway resolves an FN ID to HTTPS relay origins.
//! Protocol reference: the JavaScript served by https://fnos.net/.

use sha2::{Digest, Sha256};
use url::Url;

use crate::api::{Error, Result, normalize_base};

pub(crate) const GATEWAY: &str = "https://fnos.net/api/v1/fn/con";
pub(crate) const API_KEY: &str = "zIGtkc3dqZnJpd29qZXJqa2w7c";

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 63
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id.as_bytes()[id.len() - 1].is_ascii_alphanumeric()
}

/// A bare FN ID is accepted only when the FN Connect connection mode is chosen.
/// That keeps single-label LAN hostnames working in the address connection mode.
pub fn parse_id(input: &str) -> Result<String> {
    let input = input.trim();
    let id = if valid_id(input) {
        input.to_owned()
    } else {
        id_from_link(input).ok_or_else(|| {
            Error::Invalid("请输入 FN ID 或 fnos.net 上的 FN Connect 链接".into())
        })??
    };
    if !valid_id(&id) {
        return Err(Error::Invalid("FN ID 格式不正确".into()));
    }
    Ok(id.to_ascii_lowercase())
}

/// Recognize official links without interpreting unrelated NAS addresses as IDs.
pub fn id_from_link(input: &str) -> Option<Result<String>> {
    let input = input.trim();
    let address = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = Url::parse(&address).ok()?;
    let host = url.host_str()?;
    let id = if matches!(
        host,
        "fnos.net" | "www.fnos.net" | "5ddd.com" | "www.5ddd.com"
    ) {
        url.path_segments()?.next().unwrap_or_default()
    } else if let Some(id) = host
        .strip_suffix(".fnos.net")
        .or_else(|| host.strip_suffix(".5ddd.com"))
    {
        id
    } else {
        return None;
    };
    Some(
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || !valid_id(id)
            || url.port().is_some_and(|port| port != 443 && port != 80)
        {
            Err(Error::Invalid("FN Connect 链接格式不正确".into()))
        } else {
            Ok(id.to_ascii_lowercase())
        },
    )
}

pub(crate) fn gateway_sign(id: &str, timestamp: u128) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("trim_connect`{id}`{timestamp}`anna"))
    )
}

pub(crate) fn relay_bases(addresses: &[String]) -> Result<Vec<Url>> {
    let mut bases = Vec::new();
    for address in addresses.iter().take(4) {
        let address = address.trim();
        let address = if address.contains("://") {
            address.to_owned()
        } else {
            format!("https://{address}")
        };
        let Ok(url) = Url::parse(&address) else {
            continue;
        };
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            continue;
        }
        let base = normalize_base(url.as_str())?;
        if !bases.contains(&base) {
            bases.push(base);
        }
    }
    if bases.is_empty() {
        return Err(Error::Invalid(
            "FN Connect 未返回 HTTPS 中继地址，请检查 NAS 的远程访问设置".into(),
        ));
    }
    Ok(bases)
}
