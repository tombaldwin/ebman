//! IAM: `SimulatePrincipalPolicy` for `:explain`, and resolving an
//! environment's instance-profile role.
//!
//! Global service — one endpoint per partition, resolved by
//! [`super::global_service_region`]. Built on first use, because only
//! `:explain` reaches it.

use super::*;

/// Build an IAM client endpointed in the operator's partition.
///
/// IAM is global: one endpoint per partition. This was hardcoded to
/// `us-east-1`, which is correct for the commercial partition and
/// cannot resolve in GovCloud, China or the ISO partitions.
/// (Organizations is *not* pinned — it is built from the operator's own
/// config and routed by the SDK's endpoint rules.)
pub(super) fn iam_client(base: &SdkConfig) -> IamClient {
    let region = base.region().map(|r| r.to_string()).unwrap_or_default();
    let cfg = base
        .to_builder()
        .region(Region::new(super::global_service_region(&region)))
        .build();
    IamClient::new(&cfg)
}

/// One row of the `:explain` IAM diagnosis result. Carries the
/// per-action decision + the matched statements (so the operator
/// can audit which policy granted / denied / failed-to-grant) +
/// SCP / permission-boundary blockers when present.
#[derive(Clone, Debug)]
pub struct IamSimResult {
    pub action: String,
    pub resource: String,
    /// `"allowed"`, `"explicitDeny"`, or `"implicitDeny"`. Verbatim
    /// from the SDK so the renderer can map to severity colours
    /// without re-parsing.
    pub decision: String,
    /// Matched statements — typically `(policy_source_arn, sid_or_index)`.
    /// Empty for `implicitDeny` (no statement matched).
    pub matched_statements: Vec<String>,
    /// Conditions in the matched statements that weren't satisfied
    /// (e.g. `aws:RequestTag/Environment` missing). Empty when
    /// no conditions are pending.
    pub missing_context: Vec<String>,
    /// `true` when an SCP at the Organizations level denied the
    /// action regardless of the role's own policies — diagnoses
    /// the "role looks fine but call fails" case.
    pub blocked_by_scp: bool,
    /// `true` when a permission boundary on the role denied the
    /// action (same shape as SCP at the role level).
    pub blocked_by_boundary: bool,
}

impl AwsClient {
    /// Resolve an EB `IamInstanceProfile` option value (bare name or
    /// full ARN) to the ARN of the ROLE inside the profile — the
    /// principal `simulate_principal_policy` needs (instance-profile
    /// ARNs aren't simulatable principals). Returns `Ok(None)` when
    /// the profile exists but carries no role. Used by the EBL020
    /// X-Ray lint probe. (SDK-compiled; call shape unverified against
    /// a live account — same status as the ACM listener fetch.)
    pub async fn instance_profile_role_arn(&self, profile: &str) -> Result<Option<String>> {
        // GetInstanceProfile wants the bare name; EB sometimes stores
        // the full ARN (`arn:aws:iam::123:instance-profile/name`).
        let name = profile.rsplit('/').next().unwrap_or(profile);
        let resp = self
            .iam()
            .get_instance_profile()
            .instance_profile_name(name)
            .send()
            .await
            .wrap_err("GetInstanceProfile failed")?;
        Ok(resp
            .instance_profile
            .and_then(|p| p.roles.into_iter().next())
            .map(|r| r.arn))
    }

    /// Call `iam:SimulatePrincipalPolicy` for a role + action list.
    /// Returns the per-action decision (allowed / explicitDeny /
    /// implicitDeny), matched statements, and SCP / permission-
    /// boundary blocker flags. Powers `:explain`.
    ///
    /// `resource_arns` defaults to `["*"]` when empty — most EB
    /// AccessDenied errors don't carry a resource ARN that would
    /// affect the decision, and the unscoped check still surfaces
    /// "the role doesn't have this action at all" cases which is
    /// what the operator usually wants. Pass real ARNs when you
    /// want to evaluate resource-scoped policies.
    ///
    /// Errors out of the SimulatePrincipalPolicy itself usually
    /// mean the caller lacks `iam:SimulatePrincipalPolicy` on the
    /// target role — common with assumed-role sessions that don't
    /// have IAM perms. The renderer surfaces that as a clear hint.
    pub async fn simulate_principal_policy(
        &self,
        principal_arn: &str,
        action_names: &[String],
        resource_arns: &[String],
    ) -> Result<Vec<IamSimResult>> {
        if action_names.is_empty() {
            return Ok(Vec::new());
        }
        let resources: Vec<String> = if resource_arns.is_empty() {
            vec!["*".to_string()]
        } else {
            resource_arns.to_vec()
        };
        // Follow `marker` while `is_truncated`. IAM can truncate below
        // MaxItems, and `:explain` is a surface where an action's
        // *absence* from the table reads as "not the problem" — so a
        // silently dropped page turns a denied action into a
        // non-finding. The cap is a runaway guard; hitting it warns.
        // Named distinctly: `aws::MAX_PAGES` is the shared runaway
        // guard, and a same-named local silently shadowed it here.
        const SIMULATE_MAX_PAGES: usize = 10;
        let mut raw = Vec::new();
        let mut marker: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut req = self
                .iam()
                .simulate_principal_policy()
                .policy_source_arn(principal_arn);
            for a in action_names {
                req = req.action_names(a);
            }
            for r in &resources {
                req = req.resource_arns(r);
            }
            if let Some(m) = marker.take() {
                req = req.marker(m);
            }
            let resp = req
                .send()
                .await
                .wrap_err("SimulatePrincipalPolicy failed")?;
            raw.extend(resp.evaluation_results.unwrap_or_default());
            pages += 1;
            match resp.marker {
                Some(m) if resp.is_truncated && !m.is_empty() && pages < SIMULATE_MAX_PAGES => {
                    marker = Some(m);
                }
                Some(m) if resp.is_truncated && !m.is_empty() => {
                    tracing::warn!(
                        target: "ebman::aws",
                        pages,
                        collected = raw.len(),
                        "SimulatePrincipalPolicy page cap reached — some action \
                         decisions were not fetched"
                    );
                    break;
                }
                _ => break,
            }
        }
        let mut out: Vec<IamSimResult> = Vec::new();
        for r in raw {
            let action = r.eval_action_name;
            let resource = r.eval_resource_name.unwrap_or_default();
            let decision = r.eval_decision.as_str().to_string();
            let matched_statements: Vec<String> = r
                .matched_statements
                .unwrap_or_default()
                .into_iter()
                .filter_map(|s| {
                    let policy = s.source_policy_id?;
                    // SDK returns start_position as `Option<Position>`
                    // with `line` + `column` already as `i32` (not
                    // Option). Format defensively in case the
                    // position is missing for an inline-eval result.
                    let sid = s
                        .start_position
                        .as_ref()
                        .map(|p| format!("{}:{}", p.line, p.column))
                        .unwrap_or_else(|| "0:0".into());
                    Some(format!("{policy} @ {sid}"))
                })
                .collect();
            let missing_context: Vec<String> = r.missing_context_values.unwrap_or_default();
            // SCP / boundary blockers — only populated when the
            // top-level decision was overridden by an org-level
            // policy or the role's permission boundary. Both fields
            // carry an `EvalDecisionDetail` we just need the
            // `allowed_by_organizations` / `allowed_by_permissions_boundary`
            // flag for.
            let blocked_by_scp = r
                .organizations_decision_detail
                .as_ref()
                .is_some_and(|d| !d.allowed_by_organizations);
            let blocked_by_boundary = r
                .permissions_boundary_decision_detail
                .as_ref()
                .is_some_and(|d| !d.allowed_by_permissions_boundary);
            out.push(IamSimResult {
                action,
                resource,
                decision,
                matched_statements,
                missing_context,
                blocked_by_scp,
                blocked_by_boundary,
            });
        }
        Ok(out)
    }
}
