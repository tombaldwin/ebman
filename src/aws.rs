//! The AWS boundary: one `AwsClient` holding a per-service SDK client,
//! and every call the rest of the crate makes.
//!
//! This module owns construction (profile / region resolution, `AssumeRole`,
//! the STS identity check) and the plain Rust types the SDK shapes get mapped
//! into. Nothing above it — `app`, `ui`, `cli` — imports an `aws_sdk_*` crate
//! directly, so an SDK upgrade stops here.
//!
//! # Where the Elastic Beanstalk part is
//!
//! Of the fourteen AWS services ebman talks to, only one is the domain. The
//! sub-modules are split by service precisely so that boundary is visible:
//!
//! - `eb` is Elastic Beanstalk — environments and their health, the
//!   application/version model, option settings, saved-configuration
//!   templates, platform upgrades. This is the part that makes ebman
//!   specifically an EB tool, and it is the part a sibling tool for another
//!   service would replace wholesale.
//! - everything else — `cloudwatch`, `logs`, `sqs`, `s3`, `ec2`, `ssm`,
//!   `cost`, `secrets`, `iam`, `org`, `acm`, `waf` — is generic AWS surface
//!   any operator TUI would want, and is EB-agnostic apart from taking an
//!   environment name as a lookup key.
//!
//! Each sub-module holds its own types *and* the calls that produce them, and
//! is glob-re-exported here, so `crate::aws::Environment` and
//! `crate::aws::QueueMessage` resolve the same as when this was one file.
//!
//! The split is by *SDK client*, which is not always the same as by subject:
//! `describe_worker_queues` lives in `eb` because that's whose client it
//! calls, even though what it returns is a queue depth, and it reaches SQS
//! through `sqs::queue_stats`. Where a type's producer and its subject
//! disagree, the type follows the producer.
//!
//! # Conventions
//!
//! Errors use `wrap_err` rather than `eyre!("...: {e}")` so the SDK error
//! survives as a source in the chain — `app::flatten_err_to_string`
//! peeks at it to recognise throttling, and flattening early would disarm the
//! refresh back-off.

use aws_config::{Region, SdkConfig};
use aws_sdk_acm::Client as AcmClient;
use aws_sdk_cloudwatch::Client as CwClient;
use aws_sdk_cloudwatchlogs::Client as CwLogsClient;
use aws_sdk_costexplorer::Client as CostExplorerClient;
use aws_sdk_ec2::Client as Ec2Client;
use aws_sdk_elasticbeanstalk::Client;
use aws_sdk_iam::Client as IamClient;
use aws_sdk_organizations::Client as OrgClient;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_secretsmanager::Client as SecretsClient;
use aws_sdk_sqs::Client as SqsClient;
use aws_sdk_ssm::Client as SsmClient;
use aws_sdk_sts::Client as StsClient;
use chrono::{DateTime, Utc};
use color_eyre::eyre::{eyre, Result, WrapErr};

mod acm;
mod cloudwatch;
mod cost;
// ---------------------------------------------------------------------------
// Per-service sub-modules. Each contributes `impl AwsClient { ... }` for one
// AWS service, so the boundary between "the Elastic Beanstalk domain" and
// "generic AWS surface any operator TUI would want" is visible in the file
// list rather than buried in a 6k-line module. They open with `use super::*`
// and are glob-re-exported below, so every `crate::aws::foo` path resolves as
// it did when this was one file.
// ---------------------------------------------------------------------------
mod eb; // Elastic Beanstalk — the domain
mod ec2;
mod iam;
mod logs;
mod org;
mod s3;
mod secrets;
mod sqs;
mod ssm;
mod waf;

pub use acm::*;
pub use cloudwatch::*;
pub use cost::*;
pub use eb::*;
pub use ec2::*;
pub use iam::*;
pub use logs::*;
pub use org::*;
pub use s3::*;
pub use secrets::*;
pub use sqs::*;
pub use ssm::*;
// `waf` contributes only an `impl AwsClient` method — no types to re-export.

#[derive(Clone, Debug)]
pub struct AwsContext {
    pub region: String,
    pub profile: Option<String>,
    pub account_id: Option<String>,
    pub caller_arn: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub account_id: Option<String>,
    pub caller_arn: Option<String>,
}

pub struct AwsClient {
    client: Client,
    sqs: SqsClient,
    cw: CwClient,
    cw_logs: CwLogsClient,
    s3: S3Client,
    ec2: Ec2Client,
    /// Organizations client. The service is global, but this is built
    /// from the operator's own `SdkConfig` — the SDK's endpoint rules
    /// route it, nothing here pins a region.
    org: std::sync::OnceLock<OrgClient>,
    /// Cost Explorer client, pinned to `us-east-1` by
    /// [`cost_explorer_client`] — Cost Explorer is global and only
    /// endpoints there.
    ///
    /// Built eagerly in every constructor, despite what an earlier
    /// comment here claimed: operators who never run `:cost on` still
    /// pay for it, and it is the most expensive client to construct.
    /// Making it genuinely lazy is tracked in `BACKLOG.md`.
    cost: std::sync::OnceLock<CostExplorerClient>,
    /// IAM client used by `:explain` to call
    /// `iam:SimulatePrincipalPolicy`. IAM is global, and
    /// [`iam_client`] pins it to `us-east-1`.
    iam: std::sync::OnceLock<IamClient>,
    /// Secrets Manager client. Region-scoped (unlike IAM and Cost
    /// Explorer, which this crate pins to `us-east-1`) — operators
    /// read secrets from the same region as the env they're
    /// configuring.
    secrets: std::sync::OnceLock<SecretsClient>,
    /// ACM client. Region-scoped — `:listener-edit` lists the region's
    /// certificates for the SSL-cert picker.
    acm: std::sync::OnceLock<AcmClient>,
    /// SSM client. Region-scoped — `:ssm-run` sends a shell command to
    /// the env's instances and aggregates the per-instance results.
    ///
    /// Private like the rest: the mock-AWS tests that overwrite this
    /// post-construction live in `aws::tests`, a descendant of this
    /// module, so they reach it without `pub(crate)`. (It was
    /// `pub(crate)` on the assumption they needed it; they don't.)
    ssm: std::sync::OnceLock<SsmClient>,
    config: SdkConfig,
    pub context: AwsContext,
}

impl AwsClient {
    // ---- lazily-built clients ----------------------------------------
    //
    // Six of the twelve sub-clients are only reachable from an explicit
    // operator action — `:cost on`, `:explain`, the accounts overlay,
    // `:secrets`, `:listener-edit`, `:ssm-run`. Building them eagerly
    // cost every session, and `list_environments_in_region` constructs a
    // whole `AwsClient` per region on every refresh tick, so a
    // multi-region fan-out paid for all six per region per tick.
    //
    // Measured on this machine: each SDK client costs ~0.6 ms to
    // construct, near-identically across services, so all twelve came to
    // ~7.3 ms. Deferring these six roughly halves that. (An earlier note
    // in this file claimed Cost Explorer alone dominated; it doesn't —
    // the cost is uniform, there are just twelve of them.)
    //
    // `OnceLock` rather than a plain `Option`: `AwsClient` is shared as
    // `Arc<AwsClient>` across spawned tasks, so it has to stay `Sync`,
    // and the accessors take `&self`.

    fn cost(&self) -> &CostExplorerClient {
        self.cost.get_or_init(|| cost_explorer_client(&self.config))
    }

    fn iam(&self) -> &IamClient {
        self.iam.get_or_init(|| iam_client(&self.config))
    }

    fn org(&self) -> &OrgClient {
        self.org.get_or_init(|| OrgClient::new(&self.config))
    }

    fn secrets(&self) -> &SecretsClient {
        self.secrets
            .get_or_init(|| SecretsClient::new(&self.config))
    }

    fn acm(&self) -> &AcmClient {
        self.acm.get_or_init(|| AcmClient::new(&self.config))
    }

    fn ssm(&self) -> &SsmClient {
        self.ssm.get_or_init(|| SsmClient::new(&self.config))
    }
    /// Build the SDK client without making any network calls.
    pub async fn with(profile: Option<String>, region: Option<String>) -> Result<Self> {
        let mut builder = aws_config::defaults(aws_config::BehaviorVersion::latest());
        if let Some(p) = profile.clone() {
            builder = builder.profile_name(p);
        }
        if let Some(r) = region.clone() {
            builder = builder.region(Region::new(r));
        }
        let config = builder.load().await;

        let resolved_region = config
            .region()
            .map(|r| r.as_ref().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        if region.as_deref().is_some_and(|r| r != resolved_region) {
            // SDK silently fell back to its chain. Most likely cause:
            // `.region(Region::new(r))` failed to bind because `r` is
            // empty or whitespace, leaving the env / profile chain to
            // pick. Make it loud so we can see it in `ebman.log`.
            tracing::warn!(
                target: "ebman::aws",
                requested = ?region,
                resolved = %resolved_region,
                env_aws_region = ?std::env::var("AWS_REGION").ok(),
                env_aws_default_region = ?std::env::var("AWS_DEFAULT_REGION").ok(),
                "AwsClient::with region mismatch — explicit override was ignored by SDK"
            );
        }
        let region = resolved_region;
        let profile = profile.or_else(|| std::env::var("AWS_PROFILE").ok());
        let client = Client::new(&config);
        let sqs = SqsClient::new(&config);
        let cw = CwClient::new(&config);
        let cw_logs = CwLogsClient::new(&config);
        let s3 = S3Client::new(&config);
        let ec2 = Ec2Client::new(&config);

        Ok(Self {
            client,
            sqs,
            cw,
            cw_logs,
            s3,
            ec2,
            org: std::sync::OnceLock::new(),
            cost: std::sync::OnceLock::new(),
            iam: std::sync::OnceLock::new(),
            secrets: std::sync::OnceLock::new(),
            acm: std::sync::OnceLock::new(),
            ssm: std::sync::OnceLock::new(),
            config,
            context: AwsContext {
                region,
                profile,
                account_id: None,
                caller_arn: None,
            },
        })
    }

    /// Build an `AwsClient` by `sts:AssumeRole`-ing into a target role
    /// using `source_profile`'s creds as the base identity. Pinned to
    /// `target_region` when supplied (falls back to the source profile's
    /// region / env default). Returned client carries the assumed
    /// session's caller_arn / account_id once `verify_identity` runs.
    ///
    /// Session lifetime defaults to AWS's 1h cap; the caller is
    /// expected to swap clients again before expiry. We don't implement
    /// background refresh here — the operator's refresh tick will
    /// re-invoke this when the session dies.
    pub async fn assume_role(target_name: &str, spec: &crate::config::AccountSpec) -> Result<Self> {
        // Stage 1: load the source-profile creds + region.
        let mut builder = aws_config::defaults(aws_config::BehaviorVersion::latest());
        if let Some(p) = spec.source_profile.as_ref() {
            builder = builder.profile_name(p.clone());
        }
        if let Some(r) = spec.region.clone() {
            builder = builder.region(Region::new(r));
        }
        let base_config = builder.load().await;

        // Stage 2: STS:AssumeRole against the configured role.
        let sts = StsClient::new(&base_config);
        let session_name = format!("ebman-{target_name}");
        let mut req = sts
            .assume_role()
            .role_arn(spec.role_arn.clone())
            .role_session_name(session_name);
        if let Some(eid) = spec.external_id.as_ref() {
            req = req.external_id(eid.clone());
        }
        let resp = req.send().await.wrap_err("sts:AssumeRole failed")?;
        let creds = resp
            .credentials
            .ok_or_else(|| eyre!("sts:AssumeRole returned no credentials"))?;
        let access_key = creds.access_key_id;
        let secret_key = creds.secret_access_key;
        let session_token = creds.session_token;
        let aws_creds = aws_credential_types::Credentials::new(
            access_key,
            secret_key,
            Some(session_token),
            Some(sts_expiry_to_system_time(creds.expiration.secs())?),
            "ebman-assume-role",
        );

        // Stage 3: build the final SdkConfig with the assumed creds.
        // We rebuild from scratch (rather than mutating base_config) so
        // the resulting config carries ONLY the assumed-role identity —
        // no leaked source-profile creds, no cross-region surprises.
        let mut builder = aws_config::defaults(aws_config::BehaviorVersion::latest());
        builder = builder.credentials_provider(aws_creds);
        if let Some(r) = spec.region.clone() {
            builder = builder.region(Region::new(r));
        } else if let Some(r) = base_config.region().cloned() {
            builder = builder.region(r);
        }
        let config = builder.load().await;
        let region = config
            .region()
            .map(|r| r.as_ref().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        Ok(Self {
            client: Client::new(&config),
            sqs: SqsClient::new(&config),
            cw: CwClient::new(&config),
            cw_logs: CwLogsClient::new(&config),
            s3: S3Client::new(&config),
            ec2: Ec2Client::new(&config),
            org: std::sync::OnceLock::new(),
            cost: std::sync::OnceLock::new(),
            iam: std::sync::OnceLock::new(),
            secrets: std::sync::OnceLock::new(),
            acm: std::sync::OnceLock::new(),
            ssm: std::sync::OnceLock::new(),
            config,
            context: AwsContext {
                region,
                // Track the friendly account name as the "profile"
                // breadcrumb so the header reads `account=prod` rather
                // than the source profile name (which is just the
                // launchpad, not the destination).
                profile: Some(target_name.to_string()),
                account_id: None,
                caller_arn: None,
            },
        })
    }

    /// Build an `AwsClient` with default (un-mocked) sub-clients. For
    /// tests that exercise non-AWS code paths (keyboard flow, render,
    /// pure-helper composition) and don't care which AWS surface is
    /// reachable. Any AWS call against the returned client will fail
    /// loudly, which is the desired signal for "test accidentally hit
    /// the network". Pair with `App::for_tests` to drive `handle_event`
    /// without spinning up a real session.
    ///
    /// Also the underlying client used by `App::new_demo` — `--demo`
    /// mode wants the no-network behaviour without the cfg gate.
    /// `pub(crate)` because every caller (test code + `App::new_demo`)
    /// lives in this crate; no need to expose to the bin or downstream.
    pub(crate) fn stub() -> Self {
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        Self::for_tests(
            Client::new(&cfg),
            SqsClient::new(&cfg),
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            Ec2Client::new(&cfg),
        )
    }

    /// Build a fully-mocked `AwsClient` for unit tests. The caller supplies
    /// pre-built (typically `mock_client!`-backed) sub-clients; any client
    /// not exercised by the test can stay as a plain SDK-default instance.
    /// Tests should not assume any of the sub-clients can talk to a real
    /// endpoint — the default ones will fail if a non-mocked code path is
    /// reached, which is exactly the signal we want.
    pub(crate) fn for_tests(
        client: Client,
        sqs: SqsClient,
        cw: CwClient,
        cw_logs: CwLogsClient,
        s3: S3Client,
        ec2: Ec2Client,
    ) -> Self {
        // A bare config is fine here — every sub-client is owned by the
        // caller, so the only consumer of `self.config` is the lazy STS
        // client in `verify_identity`, which our tests don't call.
        let config = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        // Org + Cost Explorer clients use default config because no
        // existing test exercises them; mocked variants can use a
        // dedicated helper if added.
        Self {
            client,
            sqs,
            cw,
            cw_logs,
            s3,
            ec2,
            org: std::sync::OnceLock::new(),
            cost: std::sync::OnceLock::new(),
            iam: std::sync::OnceLock::new(),
            secrets: std::sync::OnceLock::new(),
            acm: std::sync::OnceLock::new(),
            ssm: std::sync::OnceLock::new(),
            config,
            context: AwsContext {
                region: "us-east-1".to_string(),
                profile: None,
                account_id: None,
                caller_arn: None,
            },
        }
    }

    /// Verify credentials work and fetch the caller identity. Used at startup to
    /// detect invalid persisted profiles, and as a background task after rebuild.
    pub async fn verify_identity(&self) -> Result<Identity> {
        let ident = StsClient::new(&self.config)
            .get_caller_identity()
            .send()
            .await
            .wrap_err("sts get-caller-identity failed")?;
        Ok(Identity {
            account_id: ident.account,
            caller_arn: ident.arn,
        })
    }

    /// Fetch the body of a pre-signed S3 URL. Shells out to `curl` so we don't
    /// pull in an HTTP-client dep; pre-signed URLs are plain HTTPS GETs with
    /// no auth headers, which curl handles trivially. 15 s cap per fetch.
    ///
    /// `url` is `EnvironmentInfoDescription.message` passed through verbatim,
    /// so it is only as trustworthy as the account's EB API responses. Two
    /// guards, because curl is generous about what it accepts:
    ///
    /// - `--` stops option parsing, so a value beginning with `-` is treated
    ///   as a URL rather than as a flag. Without it, `-o/tmp/x` would make
    ///   curl write to a local file and return an empty body.
    /// - `--proto =https` restricts the transfer to HTTPS, so a `file://` or
    ///   `ftp://` value can't be fetched and rendered into the log overlay.
    pub async fn fetch_url_text(url: &str) -> Result<String> {
        use tokio::process::Command;
        let out = Command::new("curl")
            .args([
                "-s",
                "-S",
                "--fail-with-body",
                "--proto",
                "=https",
                "--max-time",
                "15",
                "--no-buffer",
                "--",
            ])
            .arg(url)
            .output()
            .await
            .wrap_err("could not invoke curl (is it installed?)")?;
        if !out.status.success() {
            return Err(eyre!(
                "curl exit {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Drive a token-paginated AWS listing to completion.
///
/// Eleven listings across `aws/` hand-rolled the same loop — build the
/// request, attach the token if there is one, send, extend, look for the
/// next token — and none of them bounded it. A server that keeps handing
/// back a token (a bug, a proxy, a hostile endpoint) would spin that task
/// forever, leaving the operation permanently "loading" with no error.
///
/// `page` is called once per page with the token from the previous one
/// (`None` first) and returns that page's items plus the next token.
/// Capture `&self` and any borrowed arguments into the async block by
/// copy — shared references are `Copy`, so the returned future borrows
/// from them rather than from the closure, which is what lets a single
/// `Fut` type work here without async closures.
pub(crate) async fn paginate<T, F, Fut>(what: &'static str, mut page: F) -> Result<Vec<T>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<(Vec<T>, Option<String>)>>,
{
    /// Far above any real result set — this is a runaway guard, not a
    /// limit. Listings that legitimately need bounding (the log and
    /// event tails, Cost Explorer) carry their own, tighter cap.
    const MAX_PAGES: usize = 100;
    let mut items = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let (batch, next) = page(token.take()).await?;
        items.extend(batch);
        match next {
            Some(t) if !t.is_empty() => token = Some(t),
            _ => return Ok(items),
        }
    }
    tracing::warn!(
        target: "ebman::aws",
        operation = what,
        pages = MAX_PAGES,
        collected = items.len(),
        "pagination cap reached — result may be incomplete"
    );
    Ok(items)
}

/// The region to endpoint a *global* AWS service in, given the region the
/// operator is working in.
///
/// IAM and Cost Explorer are global: they have one endpoint per partition,
/// not per region. Both clients used to hardcode `us-east-1`, which is
/// correct for the commercial partition and simply cannot work anywhere
/// else — a GovCloud or China operator got a cross-partition endpoint, so
/// `:explain` and `:cost on` failed outright.
///
/// The property that matters here is staying inside the operator's
/// partition. Within the right partition a wrong region still resolves;
/// across partitions nothing does.
fn global_service_region(operator_region: &str) -> &'static str {
    if operator_region.starts_with("us-gov-") {
        "us-gov-west-1"
    } else if operator_region.starts_with("cn-") {
        "cn-north-1"
    } else {
        "us-east-1"
    }
}

/// Convert an STS credential expiry (seconds since the epoch, `i64`) into a
/// `SystemTime`, refusing anything that can't be represented.
///
/// The refusal is the point. This used to be `secs() as u64` fed straight
/// into `SystemTime::checked_add`, and the failure mode was silent and
/// backwards: a non-positive `secs` — clock skew, a malformed response, a
/// mocked endpoint — wraps under `as` to ~1.8e19, `checked_add` overflows and
/// returns `None`, and `Credentials::new(..., None, ...)` means *never
/// expires*. The session was then treated as permanently valid, so the
/// refresh tick never re-assumed the role and every later call failed with
/// ExpiredToken until the operator restarted.
///
/// Of the two readings of an expiry we can't parse, "never expires" is the
/// dangerous one; failing the assume-role outright is recoverable and says
/// what happened.
fn sts_expiry_to_system_time(secs: i64) -> Result<std::time::SystemTime> {
    u64::try_from(secs)
        .ok()
        .and_then(|s| {
            std::time::SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(s))
        })
        .ok_or_else(|| {
            eyre!(
                "sts:AssumeRole returned an unusable credential expiry \
                 ({secs}s since the epoch) — refusing rather than treating \
                 the session as never-expiring"
            )
        })
}

/// The two actionable rewrites of [`rewrite_credential_error`],
/// split so each caller can append its own surface-appropriate
/// follow-up hint (TUI: Ctrl-R / `p`; MCP: nothing to press).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialHint {
    Expired(String),
    Invalid(String),
}

/// Pure: rewrite an AWS-layer error into the actionable credentials
/// command when it matches a known signal, `None` for everything
/// else. Shared by the TUI's `format_aws_error` and the MCP server's
/// tool-error path so the signal lists live once.
///
/// **ORDER MATTERS**: the SSO arm MUST run before the invalid-creds
/// arm. The SSO signal list includes generic "unable to load
/// credentials" / "no credentials in the property bag" tokens that
/// AWS sometimes emits ALONGSIDE InvalidClientTokenId (e.g. SSO
/// refresh failure with a stale token in the chain). Routing those
/// to `aws configure --profile X` would misdirect the operator —
/// `aws sso login` is the correct first remediation. Do NOT reorder
/// for "alphabetical tidiness". 0.17.4 review caught this as a
/// latent risk.
pub fn rewrite_credential_error(profile: &str, msg: &str) -> Option<CredentialHint> {
    let lower = msg.to_lowercase();
    let sso_signals = [
        "expiredtoken",
        "expired token",
        "token has expired",
        "the security token included in the request is expired",
        "unable to load credentials",
        "no credentials in the property bag",
        "sso session has expired",
    ];
    if sso_signals.iter().any(|s| lower.contains(s)) {
        return Some(CredentialHint::Expired(format!(
            "credentials expired — run: aws sso login --profile {profile}"
        )));
    }
    let invalid_creds_signals = [
        "invalidclienttokenid",
        "the security token included in the request is invalid",
        "signaturedoesnotmatch",
        "the request signature we calculated does not match",
    ];
    if invalid_creds_signals.iter().any(|s| lower.contains(s)) {
        return Some(CredentialHint::Invalid(format!(
            "credentials invalid for profile '{profile}' — run: aws configure --profile {profile}"
        )));
    }
    None
}

#[cfg(test)]
mod tests;
