//! AWS Organizations: enumerating member accounts for the
//! multi-account overlays.
//!
//! The service is global, but the client is built from the operator's
//! own `SdkConfig` — unlike IAM and Cost Explorer, nothing here pins a
//! region; the SDK's endpoint rules route it.

use super::*;

/// One row in the `:accounts` overlay — an AWS Organizations child
/// account (or the management account itself). Sourced from
/// `organizations:ListAccounts`.
#[derive(Clone, Debug)]
pub struct OrgAccount {
    /// 12-digit account ID.
    pub id: String,
    /// Friendly name set when the account joined the org.
    pub name: String,
    /// Root user's email address (often the only way to spot ownership
    /// when account names are terse).
    pub email: Option<String>,
    /// `ACTIVE` / `SUSPENDED` / `PENDING_CLOSURE` — capitalised verbatim
    /// from the API.
    pub status: String,
}

impl AwsClient {
    /// `organizations:ListAccounts`, paginated. Returns every active +
    /// suspended account the active credentials can see (i.e. the
    /// caller is in the mgmt account or a delegated administrator).
    /// Surfaces the API's `AccessDenied` cleanly so the `:accounts`
    /// overlay can render a "no org access" hint rather than an opaque
    /// stack trace.
    pub async fn list_org_accounts(&self) -> Result<Vec<OrgAccount>> {
        let this = self;
        let raw = super::paginate("organizations:ListAccounts", move |token| async move {
            let mut req = this.org().list_accounts();
            if let Some(t) = token {
                req = req.next_token(t);
            }
            let resp = req
                .send()
                .await
                .wrap_err("organizations:ListAccounts failed")?;
            Ok((resp.accounts.unwrap_or_default(), resp.next_token))
        })
        .await?;
        let mut out: Vec<OrgAccount> = raw
            .into_iter()
            .map(|a| OrgAccount {
                id: a.id.unwrap_or_default(),
                name: a.name.unwrap_or_default(),
                email: a.email,
                status: a.status.map(|s| s.as_str().to_string()).unwrap_or_default(),
            })
            .collect();
        // Stable display order: status (Active first), then name.
        out.sort_by(|a, b| {
            let sa = (a.status != "ACTIVE", a.name.to_lowercase());
            let sb = (b.status != "ACTIVE", b.name.to_lowercase());
            sa.cmp(&sb)
        });
        Ok(out)
    }
}
