use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hub_client::CasJWTInfo as HubCasJwtInfo;
use once_cell::sync::Lazy;
use reqwest::header::{HeaderMap, CONTENT_LENGTH, CONTENT_RANGE, ETAG, LINK, RANGE};
use reqwest::{redirect::Policy, Client};

use crate::{CasJwtInfo, XetError, USER_AGENT};

const HEADER_X_REPO_COMMIT: &str = "x-repo-commit";
const HEADER_X_XET_HASH: &str = "x-xet-hash";
const HEADER_X_XET_REFRESH_ROUTE: &str = "x-xet-refresh-route";
const HEADER_X_XET_ENDPOINT: &str = "x-xet-cas-url";
const HEADER_X_XET_ACCESS_TOKEN: &str = "x-xet-access-token";
const HEADER_X_XET_EXPIRATION: &str = "x-xet-token-expiration";
const HEADER_X_LINKED_SIZE: &str = "x-linked-size";
const HEADER_X_LINKED_ETAG: &str = "x-linked-etag";
const HF_ENDPOINT: &str = "https://huggingface.co";
const TOKEN_CACHE_SAFETY_WINDOW: Duration = Duration::from_secs(60);

static TOKEN_CACHE: Lazy<std::sync::Mutex<HashMap<String, CachedToken>>> =
    Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
pub struct XetFileData {
    pub file_hash: String,
    pub refresh_route: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct FileResolveMetadata {
    pub download_url: String,
    pub etag: String,
    pub commit_hash: String,
    pub size: u64,
    pub xet_file_data: Option<XetFileData>,
}

#[derive(Clone)]
struct CachedToken {
    value: Arc<CasJwtInfo>,
    expires_at: Instant,
}

impl CachedToken {
    fn is_valid(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

fn canonical_repo_prefix(repo_type_plural: &str) -> &'static str {
    match repo_type_plural {
        "models" => "",
        "datasets" => "datasets/",
        "spaces" => "spaces/",
        _ => "",
    }
}

pub async fn fetch_file_metadata(
    endpoint: &str,
    repo_type_plural: &str,
    repo_full_name: &str,
    path: &str,
    revision: &str,
    token: Option<&String>,
) -> Result<FileResolveMetadata, XetError> {
    let metadata_client = Client::builder()
        .user_agent(USER_AGENT)
        .redirect(Policy::none())
        .build()
        .map_err(|e| XetError::NetworkError {
            message: format!("Failed to create metadata client: {}", e),
        })?;
    let endpoint = endpoint.trim_end_matches('/');
    let encoded_path = urlencoding::encode(path);
    let encoded_rev = urlencoding::encode(revision);

    let canonical_prefix = canonical_repo_prefix(repo_type_plural);

    // Try multiple URL formats to match the behavior of hf_transfer / hf_hub_url
    let candidate_urls = vec![
        // Canonical URL (no /api prefix, repo type only for datasets/spaces)
        format!(
            "{endpoint}/{canonical_prefix}{repo_full_name}/resolve/{encoded_rev}/{encoded_path}"
        ),
        // API URL with revision in path
        format!(
            "{endpoint}/api/{repo_type_plural}/{repo_full_name}/resolve/{encoded_rev}/{encoded_path}"
        ),
        // Format 2: resolve with path first, revision as query
        format!(
            "{endpoint}/api/{repo_type_plural}/{repo_full_name}/resolve/{encoded_path}?revision={encoded_rev}"
        ),
        // Format 3: direct resolve endpoint (without /api prefix) - legacy fallback
        format!(
            "{endpoint}/{repo_full_name}/resolve/{encoded_rev}/{encoded_path}"
        ),
    ];

    let mut last_error: Option<String> = None;

    for url in candidate_urls {
        // Try HEAD first (more efficient)
        let mut head_request = metadata_client.head(&url);
        if let Some(token) = token {
            head_request = head_request.bearer_auth(token);
        }

        match head_request.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() || status.is_redirection() {
                    match parse_metadata_from_response(resp, endpoint) {
                        Ok(metadata) => return Ok(metadata),
                        Err(err) => {
                            last_error = Some(err.to_string());
                            continue;
                        }
                    }
                } else if status.is_client_error() {
                    last_error = Some(format!("HEAD request failed with status: {}", status));
                    continue;
                } else {
                    last_error =
                        Some(format!("HEAD request received unexpected status: {}", status));
                    continue;
                }
            }
            Err(_) => {
                // HEAD failed, try GET
            }
        }

        // Fallback to GET request (reqwest automatically follows redirects)
        // We'll read headers only, not the body
        let mut get_request = metadata_client.get(&url).header(RANGE, "bytes=0-0");
        if let Some(token) = token {
            get_request = get_request.bearer_auth(token);
        }

        match get_request.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() || status.is_redirection() {
                    match parse_metadata_from_response(resp, endpoint) {
                        Ok(metadata) => return Ok(metadata),
                        Err(err) => {
                            last_error = Some(err.to_string());
                            continue;
                        }
                    }
                } else if status.is_client_error() {
                    last_error = Some(format!("GET request failed with status: {}", status));
                } else {
                    last_error =
                        Some(format!("GET request received unexpected status: {}", status));
                }
            }
            Err(err) => {
                last_error = Some(format!("GET request failed: {}", err));
            }
        }
    }

    Err(XetError::NetworkError {
        message: format!(
            "Failed to retrieve HEAD metadata: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        ),
    })
}

pub async fn get_cached_cas_jwt(
    client: &Client,
    refresh_route: &str,
    token: Option<&String>,
) -> Result<Arc<CasJwtInfo>, XetError> {
    if let Some(cached) = get_cached_token(refresh_route) {
        if cached.is_valid() {
            return Ok(cached.value.clone());
        }
    }

    let mut request = client.get(refresh_route);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }

    let response = request
        .send()
        .await
        .map_err(|e| XetError::NetworkError {
            message: format!("Failed to fetch CAS JWT: {}", e),
        })?
        .error_for_status()
        .map_err(|e| XetError::NetworkError {
            message: format!("Failed to fetch CAS JWT: {}", e),
        })?;

    let headers = response.headers();
    let endpoint =
        header_to_string(headers, HEADER_X_XET_ENDPOINT).ok_or_else(|| XetError::NetworkError {
            message: "CAS endpoint header missing".to_string(),
        })?;
    let access_token = header_to_string(headers, HEADER_X_XET_ACCESS_TOKEN).ok_or_else(|| {
        XetError::NetworkError {
            message: "CAS access token header missing".to_string(),
        }
    })?;
    let expiration = header_to_string(headers, HEADER_X_XET_EXPIRATION)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| XetError::NetworkError {
            message: "CAS expiration header missing".to_string(),
        })?;

    let cas_jwt = Arc::new(CasJwtInfo::from(HubCasJwtInfo {
        cas_url: endpoint.clone(),
        exp: expiration,
        access_token: access_token.clone(),
    }));

    cache_token(refresh_route.to_string(), cas_jwt.clone());
    Ok(cas_jwt)
}

fn parse_metadata_from_response(
    response: reqwest::Response,
    endpoint: &str,
) -> Result<FileResolveMetadata, XetError> {
    let headers = response.headers().clone();

    let commit_hash =
        header_to_string(&headers, HEADER_X_REPO_COMMIT).ok_or_else(|| XetError::NetworkError {
            message: "Missing X-Repo-Commit header".to_string(),
        })?;

    let etag = header_to_string(&headers, HEADER_X_LINKED_ETAG)
        .or_else(|| header_to_string(&headers, ETAG.as_str()))
        .ok_or_else(|| XetError::NetworkError {
            message: "Missing ETag header".to_string(),
        })?;

    let size = parse_file_size(&headers)?;

    let xet_file_data = parse_xet_file_data(&headers, endpoint);

    Ok(FileResolveMetadata {
        download_url: response.url().to_string(),
        etag,
        commit_hash,
        size,
        xet_file_data,
    })
}

fn parse_xet_file_data(headers: &HeaderMap, endpoint: &str) -> Option<XetFileData> {
    let hash = header_to_string(headers, HEADER_X_XET_HASH)?;
    let refresh_route = extract_refresh_route(headers, endpoint).or_else(|| {
        header_to_string(headers, HEADER_X_XET_REFRESH_ROUTE)
            .map(|route| rewrite_refresh_route(&route, endpoint))
    })?;

    Some(XetFileData {
        file_hash: hash,
        refresh_route,
    })
}

fn extract_refresh_route(headers: &HeaderMap, endpoint: &str) -> Option<String> {
    let link_value = headers.get(LINK)?.to_str().ok()?.to_string();
    for fragment in link_value.split(',') {
        if fragment.to_ascii_lowercase().contains("rel=\"xet-auth\"") {
            if let Some(url_start) = fragment.find('<') {
                if let Some(url_end) = fragment[url_start + 1..].find('>') {
                    let raw = &fragment[url_start + 1..url_start + 1 + url_end];
                    return Some(rewrite_refresh_route(raw.trim(), endpoint));
                }
            }
        }
    }
    None
}

fn rewrite_refresh_route(route: &str, endpoint: &str) -> String {
    if route.starts_with(HF_ENDPOINT) && !endpoint.is_empty() {
        let suffix = route.trim_start_matches(HF_ENDPOINT);
        let normalized_endpoint = endpoint.trim_end_matches('/');
        format!("{normalized_endpoint}{suffix}")
    } else {
        route.to_string()
    }
}

fn header_to_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_matches('"').trim().to_string())
}

fn parse_file_size(headers: &HeaderMap) -> Result<u64, XetError> {
    if let Some(linked_size) = header_to_string(headers, HEADER_X_LINKED_SIZE) {
        if let Ok(size) = linked_size.parse::<u64>() {
            return Ok(size);
        }
    }

    if let Some(content_range) = header_to_string(headers, CONTENT_RANGE.as_str()) {
        if let Some(total) = parse_total_from_content_range(&content_range) {
            return Ok(total);
        }
    }

    if let Some(content_length) = header_to_string(headers, CONTENT_LENGTH.as_str()) {
        if let Ok(length) = content_length.parse::<u64>() {
            return Ok(length);
        }
    }

    Err(XetError::NetworkError {
        message: "Missing file size headers in response".to_string(),
    })
}

fn parse_total_from_content_range(value: &str) -> Option<u64> {
    let parts: Vec<&str> = value.split('/').collect();
    parts.last()?.parse::<u64>().ok()
}

fn get_cached_token(key: &str) -> Option<CachedToken> {
    TOKEN_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(key).cloned())
}

fn cache_token(key: String, token: Arc<CasJwtInfo>) {
    if let Ok(mut cache) = TOKEN_CACHE.lock() {
        let expiry = compute_cache_expiry(token.exp());
        cache.insert(
            key,
            CachedToken {
                value: token,
                expires_at: expiry,
            },
        );
    }
}

fn compute_cache_expiry(exp: u64) -> Instant {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    let ttl_secs = exp.saturating_sub(now_unix);
    let ttl = Duration::from_secs(ttl_secs);
    Instant::now()
        .checked_add(ttl)
        .and_then(|instant| instant.checked_sub(TOKEN_CACHE_SAFETY_WINDOW))
        .unwrap_or_else(Instant::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CasJwtInfo;
    use reqwest::header::HeaderValue;
    use std::sync::Arc;

    #[test]
    fn rewrite_route_replaces_default_endpoint() {
        let rewritten = rewrite_refresh_route(
            "https://huggingface.co/api/models/test/xet-read-token/main",
            "https://example.com",
        );
        assert_eq!(
            rewritten,
            "https://example.com/api/models/test/xet-read-token/main"
        );
    }

    #[test]
    fn rewrite_route_preserves_custom_endpoint() {
        let rewritten = rewrite_refresh_route(
            "https://alt.huggingface.co/api/models/test/xet-read-token/main",
            "https://example.com",
        );
        assert_eq!(
            rewritten,
            "https://alt.huggingface.co/api/models/test/xet-read-token/main"
        );
    }

    #[test]
    fn extract_refresh_route_reads_link_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            LINK,
            HeaderValue::from_static(
                r#"<https://huggingface.co/api/models/foo/xet-read-token/main>; rel="xet-auth""#,
            ),
        );

        let refresh_route = extract_refresh_route(&headers, "https://example.com");
        assert_eq!(
            refresh_route.unwrap(),
            "https://example.com/api/models/foo/xet-read-token/main"
        );
    }

    #[test]
    fn parse_xet_file_data_uses_header_route() {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_X_XET_HASH, HeaderValue::from_static("sha256:abc"));
        headers.insert(
            HEADER_X_XET_REFRESH_ROUTE,
            HeaderValue::from_static("https://huggingface.co/api/models/foo/xet-read-token/main"),
        );

        let result = parse_xet_file_data(&headers, "https://example.com").unwrap();
        assert_eq!(result.file_hash, "sha256:abc");
        assert_eq!(
            result.refresh_route,
            "https://example.com/api/models/foo/xet-read-token/main"
        );
    }

    #[test]
    fn token_cache_round_trip() {
        let token = Arc::new(CasJwtInfo::from(HubCasJwtInfo {
            cas_url: "https://cas.example.com".to_string(),
            exp: u64::MAX / 2,
            access_token: "secret".to_string(),
        }));

        cache_token("test".to_string(), token.clone());
        let cached = get_cached_token("test").expect("token should be cached");
        assert!(cached.is_valid());
        assert_eq!(cached.value.access_token(), token.access_token());

        if let Ok(mut cache) = TOKEN_CACHE.lock() {
            cache.remove("test");
        }
    }
}
