use std::path::PathBuf;

use chrono::{DateTime, Utc};

/// Where the AWS CLI / SDK stash SSO session tokens. Each file is a JSON blob
/// with at least `accessToken` and `expiresAt` (ISO-8601 UTC). We don't care
/// which token belongs to which profile — for the "session expiring in N
/// minutes" header chip we just take the longest still-valid one and assume
/// that's roughly the one in use.
fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join(".aws").join("sso").join("cache")
}

/// Latest still-valid `expiresAt` across the SSO cache, or `None` if there are
/// no unexpired tokens (or the cache directory doesn't exist).
pub fn latest_session_expiry() -> Option<DateTime<Utc>> {
    let dir = cache_dir();
    let entries = std::fs::read_dir(&dir).ok()?;
    let now = Utc::now();
    let mut best: Option<DateTime<Utc>> = None;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(at) = extract_expires_at(&text) else {
            continue;
        };
        if at <= now {
            continue;
        }
        if best.is_none_or(|b| at > b) {
            best = Some(at);
        }
    }
    best
}

/// Minimal JSON-string extractor for `"expiresAt": "<RFC3339>"`. Avoids pulling
/// in a JSON dep for this one field; the SSO cache file is always a flat object.
fn extract_expires_at(text: &str) -> Option<DateTime<Utc>> {
    let key = "\"expiresAt\"";
    let i = text.find(key)?;
    let after = &text[i + key.len()..];
    let colon = after.find(':')?;
    let after = &after[colon + 1..];
    let q1 = after.find('"')?;
    let rest = &after[q1 + 1..];
    let q2 = rest.find('"')?;
    let value = &rest[..q2];
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::extract_expires_at;

    #[test]
    fn extracts_iso_string() {
        let s =
            r#"{"startUrl":"https://x","accessToken":"abc","expiresAt":"2030-01-02T03:04:05Z"}"#;
        let at = extract_expires_at(s).unwrap();
        assert_eq!(at.to_rfc3339(), "2030-01-02T03:04:05+00:00");
    }

    #[test]
    fn missing_field_is_none() {
        assert!(extract_expires_at(r#"{"foo":"bar"}"#).is_none());
    }

    #[test]
    fn malformed_value_is_none() {
        // Right key, but the value is unparseable as RFC 3339.
        let s = r#"{"expiresAt":"not-a-date"}"#;
        assert!(extract_expires_at(s).is_none());
    }

    #[test]
    fn handles_whitespace_around_colon() {
        let s = r#"{ "expiresAt" : "2030-01-02T03:04:05+00:00" }"#;
        assert!(extract_expires_at(s).is_some());
    }
}
