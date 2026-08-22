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
    /// route it, nothing here pins a region. Built on first use by
    /// [`AwsClient::org`].
    org: std::sync::OnceLock<OrgClient>,
    /// Cost Explorer client. Global service — [`cost_explorer_client`]
    /// endpoints it in the operator's own partition (see
    /// [`global_service_region`]).
    ///
    /// Built on first use by [`AwsClient::cost`]: only `:cost on`
    /// reaches it, and `list_environments_in_region` builds a whole
    /// `AwsClient` per region per refresh tick.
    cost: std::sync::OnceLock<CostExplorerClient>,
    /// IAM client used by `:explain` to call
    /// `iam:SimulatePrincipalPolicy`. Global service — [`iam_client`]
    /// endpoints it in the operator's own partition. Built on first
    /// use by [`AwsClient::iam`].
    iam: std::sync::OnceLock<IamClient>,
    /// Secrets Manager client. Region-scoped (unlike IAM and Cost
    /// Explorer, which are endpointed per partition) — operators read
    /// secrets from the same region as the env they're configuring.
    /// Built on first use by [`AwsClient::secrets`].
    secrets: std::sync::OnceLock<SecretsClient>,
    /// ACM client. Region-scoped — `:listener-edit` lists the region's
    /// certificates for the SSL-cert picker. Built on first use by
    /// [`AwsClient::acm`].
    acm: std::sync::OnceLock<AcmClient>,
    /// SSM client. Region-scoped — `:ssm-run` sends a shell command to
    /// the env's instances and aggregates the per-instance results.
    /// Built on first use by [`AwsClient::ssm`].
    ///
    /// Private like the rest: the mock-AWS tests that seed this cell
    /// live in `aws::tests`, a descendant of this module, so they
    /// reach it without `pub(crate)`.
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
    // `std::sync::Arc<AwsClient>` across spawned tasks, so it has to stay `Sync`,
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
        // A bare config is fine here: the six positional sub-clients are
        // owned by the caller, and the six lazy ones only read
        // `self.config` if a test actually touches them without seeding
        // the cell first — at which point it would fail at the network,
        // which is the signal we want.
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

/// The outcome of a paginated walk: what was collected, and whether the
/// runaway cap cut it short.
///
/// `truncated` is not decoration. A short result is only acceptable
/// where the caller renders it as a list and nothing more. Wherever the
/// caller *searches* it — `list_alarms_for_env` and a filtered
/// `list_secrets` match client-side; `list_environments`,
/// `list_application_versions` and `list_instances` are `.find()`-ed by
/// name; the VPC, certificate and account listings feed pickers — a
/// cut-short walk is indistinguishable from "not there". Those call
/// [`Paged::complete`]. An unfiltered `list_secrets` browse takes
/// [`Paged::items`], because there a shorter list is just shorter.
#[derive(Debug)]
#[must_use = "a paginated walk reports whether it was cut short — take \
              `.items()` to accept a possibly-short list, or `.complete()` \
              to refuse one"]
pub(crate) struct Paged<T> {
    /// Private: reaching past the accessors is how a caller ends up
    /// silently discarding `truncated`, which is the whole point of
    /// this type. `items()` and `complete()` are the two honest
    /// choices.
    items: Vec<T>,
    pub(crate) truncated: bool,
}

impl<T> Paged<T> {
    /// Wrap a hand-rolled walk's result so it has to be unwrapped
    /// through the same two honest choices. `simulate_principal_policy`
    /// is the one listing that can't use [`paginate`] — IAM's marker
    /// pagination is driven by an `is_truncated` flag rather than by
    /// the presence of a token — but that's no reason for its callers
    /// to get a bare `Vec` that hides the cap.
    pub(crate) fn new(items: Vec<T>, truncated: bool) -> Self {
        Self { items, truncated }
    }

    /// The items, accepting that the walk may have been cut short.
    pub(crate) fn items(self) -> Vec<T> {
        self.items
    }

    /// The items, or an error if the walk was cut short.
    ///
    /// For listings that filter after collecting: returning a subset
    /// there doesn't produce a shorter answer, it produces a wrong one.
    /// An operator told "the scan was too large" can act on that; one
    /// shown "no alarms" concludes alarms aren't the problem.
    pub(crate) fn complete(self, what: &str) -> Result<Vec<T>> {
        if self.truncated {
            return Err(eyre!(
                "{what}: the scan hit its page budget — refusing to report a \
                 partial result, because this listing is filtered after \
                 collection and a partial scan looks identical to no match"
            ));
        }
        Ok(self.items)
    }
}

/// Far above any real result set — a runaway guard, not a limit.
/// Listings that legitimately need bounding (the log and event tails,
/// Cost Explorer) carry their own, tighter cap.
const MAX_PAGES: usize = 100;

/// Page budget for listings that filter *after* collecting.
///
/// `list_alarms_for_env` and `list_secrets` scan an account and match
/// client-side, so their walk is bounded by the size of the account,
/// not by the size of the answer. At 100 records per page this covers
/// 50,000 alarms or secrets — well past CloudWatch's default 5,000
/// alarms-per-region quota even when raised. The point is that
/// [`Paged::complete`] should be a genuine impossibility rather than a
/// wall a large account hits during an outage.
const SCAN_PAGES: usize = 500;

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
pub(crate) async fn paginate<T, F, Fut>(what: &'static str, page: F) -> Result<Paged<T>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<(Vec<T>, Option<String>)>>,
{
    paginate_capped(what, MAX_PAGES, page).await
}

/// [`paginate`] with an explicit page budget — for the scan-then-filter
/// listings, which need [`SCAN_PAGES`] rather than the runaway guard.
pub(crate) async fn paginate_capped<T, F, Fut>(
    what: &'static str,
    max_pages: usize,
    mut page: F,
) -> Result<Paged<T>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<(Vec<T>, Option<String>)>>,
{
    let mut items = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..max_pages {
        let (batch, next) = page(token.take()).await?;
        items.extend(batch);
        match next {
            Some(t) if !t.is_empty() => token = Some(t),
            _ => {
                return Ok(Paged {
                    items,
                    truncated: false,
                })
            }
        }
    }
    tracing::warn!(
        target: "ebman::aws",
        operation = what,
        pages = max_pages,
        collected = items.len(),
        "pagination cap reached — result may be incomplete"
    );
    Ok(Paged {
        items,
        truncated: true,
    })
}

/// Process-wide cache of profile+region clients.
///
/// `list_environments_in_region` is called once per region on every
/// refresh tick, and each call used to build a whole `AwsClient` —
/// twelve SDK clients *and* `aws_config::load()`, which re-reads
/// `~/.aws/config` and `~/.aws/credentials` from disk and rebuilds the
/// credential-provider chain. With four extra regions that is four disk
/// round-trips and forty-eight client constructions every tick, forever.
/// Making six of the twelve lazy (see [`AwsClient::cost`] and friends)
/// only addressed the CPU-bound half, and none of those deferred cells
/// ever survived to a second use on this path.
///
/// Caching the client is also what the SDK expects: its credential
/// providers cache and refresh internally, so a long-lived client picks
/// up a renewed SSO token on its own — building a fresh one per call
/// throws that away too.
///
/// Only the profile path is cached. `assume_role` clients carry a
/// session with a hard 1-hour cap and must not be reused past it.
/// `(profile, region)` → when it was built, and the client.
type ClientCache = std::sync::Mutex<
    std::collections::HashMap<
        (Option<String>, String),
        (std::time::Instant, std::sync::Arc<AwsClient>),
    >,
>;

static CLIENT_CACHE: std::sync::OnceLock<ClientCache> = std::sync::OnceLock::new();

/// Bumped by [`clear_client_cache`]. A build that started before a
/// clear must not install its result afterwards.
static CACHE_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long a cached client is reused before being rebuilt.
///
/// Not arbitrary: the memoisation removed the only thing that re-read
/// `~/.aws` during a session. The SDK's *own* caching refreshes SSO and
/// `credential_process` credentials, but static profile credentials —
/// the ones an operator pastes from the Identity Center panel, or that
/// aws-vault writes — carry no expiry, so the provider never
/// re-resolves them. Without a TTL, pasting fresh credentials into
/// `~/.aws/credentials` did nothing until an explicit context switch or
/// a restart, with nothing on screen to suggest either.
///
/// Five minutes: short enough that a paste self-heals without the
/// operator knowing why it was broken, long enough that the 15-second
/// refresh tick still gets ~20 free reuses per region.
const CLIENT_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn client_cache() -> &'static ClientCache {
    CLIENT_CACHE.get_or_init(Default::default)
}

/// A client for this profile+region, built once and reused for
/// `CLIENT_CACHE_TTL`.
///
/// Two callers racing on a cold key both build one; the loser's is
/// dropped. That is cheaper than holding the lock across the `await`,
/// and both clients work.
pub async fn cached_client(
    profile: Option<String>,
    region: String,
) -> Result<std::sync::Arc<AwsClient>> {
    use std::sync::atomic::Ordering;
    let key = (profile.clone(), region.clone());
    let fresh = client_cache().lock().ok().and_then(|c| {
        c.get(&key)
            .filter(|(built, _)| built.elapsed() < CLIENT_CACHE_TTL)
            .map(|(_, client)| client.clone())
    });
    if let Some(found) = fresh {
        return Ok(found);
    }
    // Capture the epoch BEFORE the await. A `clear_client_cache` landing
    // while this builds means the operator changed something on disk;
    // installing a client resolved before that would quietly undo the
    // clear. The generation guard on `AppMsg` protects results, not
    // cache writes, so this needs its own.
    let epoch = CACHE_EPOCH.load(Ordering::SeqCst);
    let built = std::sync::Arc::new(AwsClient::with(profile, Some(region)).await?);
    install_if_current(key, epoch, built.clone());
    Ok(built)
}

/// Install a freshly built client, unless a clear happened while it was
/// building.
///
/// The epoch check and the insert happen under the SAME lock. Reading
/// the epoch outside it left the window open: a clear could complete
/// end-to-end between the check passing and the lock being taken, and
/// the stranded builder would then repopulate the map the operator's
/// profile switch had just emptied — serving a pre-switch client for
/// the whole TTL while the header showed the new context.
///
/// `clear_client_cache` bumps the epoch *before* it locks, so under the
/// lock the value is always current. Returns whether it installed,
/// which is what makes the guard testable.
fn install_if_current(
    key: (Option<String>, String),
    epoch: u64,
    client: std::sync::Arc<AwsClient>,
) -> bool {
    use std::sync::atomic::Ordering;
    let Ok(mut cache) = client_cache().lock() else {
        return false;
    };
    if CACHE_EPOCH.load(Ordering::SeqCst) != epoch {
        return false;
    }
    cache.insert(key, (std::time::Instant::now(), client));
    true
}

/// Serialises tests that touch the process-global client cache.
///
/// Not confined to `aws::tests`: `App::apply_rebuild` calls
/// `clear_client_cache()`, so the app-side tests that drive it race the
/// cache tests from another file entirely — an intermittent failure
/// whose cause isn't visible from where it fires.
#[cfg(test)]
pub(crate) static CACHE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The guarded install, for tests that need to drive the race
/// deterministically rather than hope to hit it.
#[cfg(test)]
pub(crate) fn install_if_current_for_tests(
    key: (Option<String>, String),
    epoch: u64,
    client: std::sync::Arc<AwsClient>,
) -> bool {
    install_if_current(key, epoch, client)
}

/// Is this key currently cached?
#[cfg(test)]
pub(crate) fn is_cached_for_tests(key: &(Option<String>, String)) -> bool {
    client_cache()
        .lock()
        .map(|c| c.contains_key(key))
        .unwrap_or(false)
}

/// The cache epoch, for tests that need to observe a clear.
#[cfg(test)]
pub(crate) fn cache_epoch_for_tests() -> u64 {
    CACHE_EPOCH.load(std::sync::atomic::Ordering::SeqCst)
}

/// Drop every cached client.
///
/// Called when the operator signals that the world changed — a profile
/// or account switch — since that is also when they may have re-run
/// `aws sso login` or edited `~/.aws/config`.
pub fn clear_client_cache() {
    // Bumped BEFORE the lock, so any builder that acquires the lock
    // after this point sees the new value and declines to install.
    CACHE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if let Ok(mut cache) = client_cache().lock() {
        cache.clear();
    }
}

/// The region to endpoint a *global* AWS service in, given the region the
/// operator is working in.
///
/// IAM and Cost Explorer are global: they have one endpoint per partition,
/// not per region. Both clients used to hardcode `us-east-1`, which is
/// correct for the commercial partition and simply cannot work anywhere
/// else — a GovCloud, China or ISO operator got a cross-partition
/// endpoint, so `:explain` and `:cost on` failed outright.
///
/// The property that matters is staying inside the operator's partition.
/// Within the right partition a wrong region still resolves; across
/// partitions nothing does. The table lives in [`crate::util`], shared
/// with ARN parsing and console URLs.
fn global_service_region(operator_region: &str) -> &'static str {
    crate::util::partition_for_region(operator_region).global_region
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
///
/// Note what "recoverable" means here, because an earlier version of this
/// doc overstated it: nothing re-assumes automatically. `assume_role` has
/// exactly one caller, `spawn_assume_role_switch`, reached only from an
/// explicit `:account <name>`. The refresh tick reuses the existing
/// client. So after the 1-hour STS cap every call fails with ExpiredToken
/// until the operator re-runs `:account` — see `BACKLOG.md`.
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
