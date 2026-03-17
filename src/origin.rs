use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::IpAddr;
use std::sync::Arc;

const LOOPBACK_ORIGINS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

/// Origin validation policy.
#[derive(Clone)]
pub enum OriginPolicy {
    /// Allow any Origin. Used with `--allowed-origins '*'`.
    AllowAll,
    /// Allow only these hostnames. Requests with no Origin header always pass.
    AllowList(Arc<Vec<String>>),
}

impl OriginPolicy {
    /// Determine the default policy based on the bind address.
    /// Loopback addresses get a localhost allowlist.
    /// Everything else denies all Origins.
    pub fn default_for_addr(addr: IpAddr) -> Self {
        if addr.is_loopback() {
            OriginPolicy::AllowList(Arc::new(
                LOOPBACK_ORIGINS.iter().map(|s| (*s).to_string()).collect(),
            ))
        } else {
            OriginPolicy::AllowList(Arc::new(Vec::new()))
        }
    }

    /// Build a policy from an explicit `--allowed-origins` value.
    pub fn from_cli(origins: &[String]) -> Self {
        if origins.len() == 1 && origins[0] == "*" {
            tracing::warn!(
                "Origin validation disabled (--allowed-origins '*'). \
                 Ensure authentication is in place for production deployments."
            );
            OriginPolicy::AllowAll
        } else {
            OriginPolicy::AllowList(Arc::new(origins.to_vec()))
        }
    }
}

/// Validate the Origin header.
///
/// - Missing Origin header => allow
/// - Origin header with hostname in allow list => allow
/// - Origin header with hostname NOT in allow list => 403 Forbidden
pub async fn validate_origin(
    axum::extract::State(policy): axum::extract::State<OriginPolicy>,
    request: Request,
    next: Next,
) -> Response {
    let origin = request
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok());

    let Some(origin) = origin else {
        return next.run(request).await;
    };

    match &policy {
        OriginPolicy::AllowAll => next.run(request).await,
        OriginPolicy::AllowList(allowed) => {
            if let Some(hostname) = extract_hostname(origin)
                && allowed.iter().any(|a| a == &hostname)
            {
                return next.run(request).await;
            }
            tracing::warn!(origin, "Rejected request with disallowed Origin");
            (
                StatusCode::FORBIDDEN,
                format!("Forbidden: Origin not allowed: {origin}"),
            )
                .into_response()
        }
    }
}

fn extract_hostname(origin: &str) -> Option<String> {
    let after_scheme = origin.split("://").nth(1)?;
    let host_port = after_scheme.split('/').next()?;
    let hostname = if host_port.starts_with('[') {
        // IPv6: [::1]:port
        host_port.split(']').next().map(|h| format!("{h}]"))
    } else {
        Some(host_port.split(':').next().unwrap_or(host_port).to_string())
    };
    hostname.filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_hostname() {
        assert_eq!(
            extract_hostname("http://localhost:3000"),
            Some("localhost".to_string())
        );
        assert_eq!(
            extract_hostname("https://127.0.0.1:8080"),
            Some("127.0.0.1".to_string())
        );
        assert_eq!(
            extract_hostname("http://[::1]:3000"),
            Some("[::1]".to_string())
        );
        assert_eq!(
            extract_hostname("https://example.com"),
            Some("example.com".to_string())
        );
        assert_eq!(
            extract_hostname("https://example.com:443/path"),
            Some("example.com".to_string())
        );
        assert_eq!(extract_hostname("not-a-url"), None);
    }

    #[test]
    fn test_default_policy_loopback() {
        let policy = OriginPolicy::default_for_addr("127.0.0.1".parse().unwrap());
        match policy {
            OriginPolicy::AllowList(list) => {
                assert!(list.contains(&"localhost".to_string()));
                assert!(list.contains(&"127.0.0.1".to_string()));
                assert!(list.contains(&"[::1]".to_string()));
            }
            _ => panic!("expected AllowList"),
        }
    }

    #[test]
    fn test_default_policy_non_loopback() {
        let policy = OriginPolicy::default_for_addr("0.0.0.0".parse().unwrap());
        match policy {
            OriginPolicy::AllowList(list) => assert!(list.is_empty()),
            _ => panic!("expected empty AllowList"),
        }
    }

    #[test]
    fn test_wildcard_policy() {
        let policy = OriginPolicy::from_cli(&["*".to_string()]);
        assert!(matches!(policy, OriginPolicy::AllowAll));
    }
}
