//! Secrets Manager: listing an account's secrets and reading a value
//! (redacted by default at the render layer, not here).

use super::*;

/// One row in the `:secrets` listing — metadata only, no values.
/// Value retrieval happens via `fetch_secret_value` on demand
/// because every `GetSecretValue` call is a separate audit-loggable
/// AWS event the operator should opt into explicitly.
#[derive(Clone, Debug)]
pub struct SecretSummary {
    pub name: String,
    pub arn: String,
    pub description: Option<String>,
    pub last_changed: Option<DateTime<Utc>>,
    pub last_rotated: Option<DateTime<Utc>>,
    pub kms_key_id: Option<String>,
}

impl AwsClient {
    /// List Secrets Manager secrets in the active region.
    /// `name_filter` is an optional substring match against the
    /// secret name (case-sensitive — Secrets Manager's
    /// `Filters.Key=name` does prefix matching only, so we
    /// post-filter for substring instead).
    ///
    /// Paginates internally. Returns the metadata rows; no
    /// secret *values* are fetched here — see [`AwsClient::fetch_secret_value`].
    pub async fn list_secrets(&self, name_filter: Option<&str>) -> Result<Vec<SecretSummary>> {
        let mut out: Vec<SecretSummary> = Vec::new();
        let mut next: Option<String> = None;
        loop {
            let mut req = self.secrets.list_secrets();
            if let Some(t) = next.take() {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("ListSecrets failed")?;
            for s in resp.secret_list.unwrap_or_default() {
                let name = match s.name {
                    Some(n) if !n.is_empty() => n,
                    _ => continue,
                };
                if let Some(needle) = name_filter {
                    if !name.contains(needle) {
                        continue;
                    }
                }
                out.push(SecretSummary {
                    name,
                    arn: s.arn.unwrap_or_default(),
                    description: s.description.filter(|d| !d.is_empty()),
                    last_changed: s
                        .last_changed_date
                        .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
                    last_rotated: s
                        .last_rotated_date
                        .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
                    kms_key_id: s.kms_key_id.filter(|k| !k.is_empty()),
                });
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next = Some(t),
                _ => break,
            }
        }
        // Stable order — most-recently-changed first so freshly
        // rotated secrets float to the top of the picker.
        out.sort_by_key(|r| std::cmp::Reverse(r.last_changed));
        Ok(out)
    }

    /// `GetSecretValue` for one secret. Returns the value verbatim —
    /// caller decides whether to display, redact, or yank.
    /// Audit-loggable on the AWS side (CloudTrail logs every
    /// GetSecretValue); ebman additionally writes its own audit
    /// line via the caller path.
    pub async fn fetch_secret_value(&self, secret_id: &str) -> Result<String> {
        let resp = self
            .secrets
            .get_secret_value()
            .secret_id(secret_id)
            .send()
            .await
            .wrap_err("GetSecretValue failed")?;
        // Secrets Manager returns either SecretString (UTF-8 text,
        // including JSON for k/v secrets) or SecretBinary (base64
        // blob). Prefer the string; fall back to noting the binary
        // length so the operator doesn't try to inspect.
        if let Some(s) = resp.secret_string {
            return Ok(s);
        }
        if let Some(b) = resp.secret_binary {
            return Ok(format!("(binary, {} bytes — not shown)", b.as_ref().len()));
        }
        Ok(String::new())
    }
}
