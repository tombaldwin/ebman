//! What an MCP write's role is (assume-role,
//! `docs/design/assume-role-elevation.md`): the role a verb maps to, as
//! a plan carries it, and whether it can carry a write to this env.
//! Slice 3: the role assumed at confirm, after the yes, and the session
//! it is named for (D4, D5).

use super::writes::*;
use super::*;

/// A role session for one approved write (D4): the client built from
/// it, and the role and session name its audit lines carry. `Clone`
/// because a `dlq_delete`'s undo buffer entry keeps it, and the undo
/// goes out under the same session.
#[derive(Clone)]
pub(super) struct RoleSession {
    pub client: std::sync::Arc<crate::aws::AwsClient>,
    pub arn: String,
    pub name: String,
}

/// By hand: `AwsClient` has no `Debug`, and the role and session name
/// are what identify it anyway.
impl std::fmt::Debug for RoleSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleSession")
            .field("arn", &self.arn)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl RoleSession {
    /// `role=` and `session=`, for each of the write's audit lines, so
    /// ebman's log joins CloudTrail on the session name (D5).
    pub(super) fn extras(&self) -> [(&'static str, String); 2] {
        [("role", self.arn.clone()), ("session", self.name.clone())]
    }
}

/// `ebman-mcp-<client>-<plan>` (D5): STS allows 2–64 characters of
/// `[\w+=,.@-]`. The client's declared name, mapped to that alphabet and
/// cut to 24 characters; and the first 12 hex digits of the plan token's
/// SHA-256, never the token, which is a live credential until used.
pub(super) fn session_name(client: &str, token: &str) -> String {
    use sha2::Digest;
    let client: String = client
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_+=,.@-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .take(24)
        .collect();
    let client = if client.is_empty() {
        "unknown".into()
    } else {
        client
    };
    let digest = sha2::Sha256::digest(token.as_bytes());
    let plan: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("ebman-mcp-{client}-{plan}")
}

impl Server {
    /// Assume the role the plan picked, for this write only (D3, D4):
    /// after the operator's yes, never at plan time, so every AssumeRole
    /// with an ebman session name is an approved write. `Ok(None)` for a
    /// verb no role lists, and on the demo backend, which assumes
    /// nothing (D8). A failure, or a session in another account than
    /// the role's, is a refusal (D7): nothing is dispatched, and the
    /// base credentials are never used instead.
    pub(super) async fn assume_role_for(
        &self,
        p: &PendingWrite,
        client_name: &str,
    ) -> Result<Option<RoleSession>, Audited> {
        let Some(role) = p.role() else {
            return Ok(None);
        };
        if matches!(self.backend, Backend::Demo) {
            return Ok(None);
        }
        let name = session_name(client_name, &p.token);
        let spec = crate::config::AccountSpec {
            role_arn: role.arn.clone(),
            // With no source profile, from the write's own credentials.
            source_profile: role.source_profile.clone().or_else(|| p.profile.clone()),
            external_id: role.external_id.clone(),
            region: p.region.clone(),
        };
        let why = match self.assume(&spec, &name).await {
            Ok(client) => match client.context.account_id.as_deref() {
                Some(a) if a == role.account => {
                    return Ok(Some(RoleSession {
                        client: std::sync::Arc::new(client),
                        arn: role.arn.clone(),
                        name,
                    }));
                }
                Some(a) => format!(
                    "the session is in account {a}, not the role's {}",
                    role.account
                ),
                None => "STS did not say which account the session is in".to_string(),
            },
            Err(e) => e,
        };
        let audit_profile = p.profile.clone().or_else(|| self.ambient_profile.clone());
        let region = self.audit_region(&p.profile, p.region.as_deref()).await;
        Err(Audited::record(
            audit_profile.as_deref(),
            &region,
            p.verb.label(),
            &p.env,
            crate::audit::RefusalRule::RoleUnassumable,
            &format!(
                "let {} assume {}, or remove the verb from mcp.role.{}",
                name_of_base(&spec),
                role.arn,
                role.key
            ),
            format!(
                "{} on {} was approved but not dispatched: role `{}` ({}) could not be \
                 assumed: {why}. Nothing ran, and it was not sent as the base identity.",
                p.verb.label(),
                p.env,
                role.name,
                role.arn,
            ),
        ))
    }

    /// One AssumeRole for 900 seconds, STS's floor (D4). Under test, the
    /// sessions `TestSeams` was seeded with.
    async fn assume(
        &self,
        spec: &crate::config::AccountSpec,
        session: &str,
    ) -> Result<crate::aws::AwsClient, String> {
        #[cfg(test)]
        {
            if let Ok(mut asked) = self.seams.assumed.lock() {
                asked.push((spec.clone(), session.to_string()));
            }
            self.seams
                .role_sessions
                .lock()
                .ok()
                .and_then(|mut q| q.pop_front())
                .unwrap_or_else(|| Err("no role session seeded".into()))
        }
        #[cfg(not(test))]
        {
            crate::aws::AwsClient::assume_role_with(spec, session, Some(900))
                .await
                .map_err(|e| format!("{e:#}"))
        }
    }

    /// What the role config says of an MCP write of `verb`.
    pub(super) fn role_check(&self, verb: WriteVerb) -> crate::config::RoleCheck {
        let known = write_verb_names();
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        crate::config::mcp_role_check(&self.safety_cfg, verb.tool_name(), &known)
    }
}

/// The role a plan's verb maps to (`docs/design/assume-role-elevation.md`,
/// D3): named in the plan and the confirmation, picked for the env's
/// account, and assumed at confirm for this write alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlanRole {
    /// The `mcp.role.KEY` that lists the verb.
    pub key: String,
    pub arn: String,
    pub account: String,
    /// The role's own name, from the ARN.
    pub name: String,
    /// Whose credentials the role is assumed from, and the external id
    /// its trust policy asks for: carried so slice 3 assumes what the
    /// operator approved, never a lookup at confirm.
    pub source_profile: Option<String>,
    pub external_id: Option<String>,
}

impl PlanRole {
    /// What the confirmation adds after the identity. Each field is
    /// capped on its own and the sentence around them never is: capped
    /// as one with the identity, the cut fell on the half that says
    /// which credential the write uses.
    pub(super) fn line(&self) -> String {
        let from = match &self.source_profile {
            Some(p) => format!("profile `{}`", sanitize(p, Shown::ASK)),
            None => "that identity".to_string(),
        };
        format!(
            "; it goes out as role `{}` ({}), assumed for this write alone from {from}",
            sanitize(&self.name, Shown::ASK),
            // Whole: `parse_role_arn` bounds it by IAM's limits, and a
            // value the operator approves is shown whole or refused.
            sanitize(&self.arn, Shown::Whole)
        )
    }

    /// The plan's `role` field.
    pub(super) fn json(&self) -> String {
        format!(
            "{{\"config\":{},\"arn\":{},\"account\":{},\"name\":{},\"assumed\":true}}",
            util::json_string(&format!("mcp.role.{}", self.key)),
            util::json_string(&self.arn),
            util::json_string(&self.account),
            util::json_string(&self.name),
        )
    }
}

impl PlanRole {
    /// The plan's copy of one role that lists the verb.
    pub(super) fn from_mapped(m: &crate::config::MappedRole) -> PlanRole {
        PlanRole {
            key: m.role.clone(),
            arn: m.arn.arn.clone(),
            account: m.arn.account.clone(),
            name: m.arn.name.clone(),
            source_profile: m.spec.source_profile.clone(),
            external_id: m.spec.external_id.clone(),
        }
    }
}

/// The account in an AWS ARN (`arn:PARTITION:SERVICE:REGION:ACCOUNT:…`),
/// when it is twelve digits.
fn arn_account(arn: &str) -> Option<&str> {
    arn.split(':')
        .nth(4)
        .filter(|a| crate::config::is_account_id(a))
}

/// Why a mapped role cannot carry this write (D3), decided locally
/// before anyone is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RoleAccountRefusal {
    /// No role that lists the verb is in the env's account.
    WrongAccount { env_account: String },
    /// Neither the env's ARN nor the identity says which account the
    /// env is in, so a role cannot be checked against it. Closed: with
    /// slice 3 a cross-account trust would send the write to the role's
    /// account, whatever account the env was found in.
    AccountUnknown,
}

impl RoleAccountRefusal {
    pub(super) fn rule(&self) -> crate::audit::RefusalRule {
        match self {
            Self::WrongAccount { .. } => crate::audit::RefusalRule::RoleWrongAccount,
            Self::AccountUnknown => crate::audit::RefusalRule::RoleAccountUnknown,
        }
    }

    pub(super) fn remedy(&self) -> String {
        match self {
            Self::WrongAccount { env_account } => format!(
                "add a role in account {env_account} that lists the verb (mcp.role.NAME.role_arn \
                 and .verbs)"
            ),
            Self::AccountUnknown => {
                "allow sts:GetCallerIdentity, or remove the verb from mcp.role.NAME.verbs".into()
            }
        }
    }

    pub(super) fn message(
        &self,
        verb: WriteVerb,
        env: &str,
        roles: &[crate::config::MappedRole],
    ) -> String {
        let mapped = roles
            .iter()
            .map(|r| {
                format!(
                    "`{}` ({}) in account {}",
                    r.arn.name, r.arn.arn, r.arn.account
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        match self {
            Self::WrongAccount { env_account } => format!(
                "{} on {env} was not planned: it maps to role {mapped}, but {env} is in account \
                 {env_account}, where no role lists it.",
                verb.label(),
            ),
            Self::AccountUnknown => format!(
                "{} on {env} was not planned: it maps to role {mapped}, and ebman cannot tell \
                 which account {env} is in (no account in its ARN, and sts:GetCallerIdentity \
                 failed), so it cannot pick the role for it.",
                verb.label(),
            ),
        }
    }
}

/// The role among `roles` in the env's account: the env's account from
/// its ARN, or else the identity's. Refused when neither says, or when no
/// role is in that account.
pub(super) fn role_for_env(
    roles: &[crate::config::MappedRole],
    env: &crate::aws::Environment,
    caller: &CallerIdentity,
) -> Result<PlanRole, RoleAccountRefusal> {
    let env_account = env.arn.as_deref().and_then(arn_account).or(match caller {
        CallerIdentity::Known { account, .. } => Some(account.as_str()),
        CallerIdentity::Unknown { .. } => None,
    });
    let Some(account) = env_account else {
        return Err(RoleAccountRefusal::AccountUnknown);
    };
    roles
        .iter()
        .find(|r| r.arn.account == account)
        .map(PlanRole::from_mapped)
        .ok_or_else(|| RoleAccountRefusal::WrongAccount {
            env_account: account.to_string(),
        })
}

/// Whose credentials the role is assumed from, for a remedy.
fn name_of_base(spec: &crate::config::AccountSpec) -> String {
    match &spec.source_profile {
        Some(p) => format!("profile `{p}`"),
        None => "the base identity".into(),
    }
}
