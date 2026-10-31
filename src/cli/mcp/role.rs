//! What an MCP write's role is (assume-role,
//! `docs/design/assume-role-elevation.md`): the role a verb maps to, as
//! a plan carries it, and whether it can carry a write to this env.
//! Slice 3's session factory and session naming belong here too.

use super::writes::*;
use super::*;

impl Server {
    /// What the role config says of an MCP write of `verb`.
    pub(super) fn role_check(&self, verb: WriteVerb) -> crate::config::RoleCheck {
        let known = write_verb_names();
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        crate::config::mcp_role_check(&self.safety_cfg, verb.tool_name(), &known)
    }
}

/// The role a plan's verb maps to: assume-role slice 2
/// (`docs/design/assume-role-elevation.md`, D3). Named in the plan and
/// the confirmation, checked against the env's account, and NOT yet
/// assumed: until slice 3 the write goes out under the base
/// credentials, and both say so rather than name a credential the
/// write does not use.
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
    /// as one with the identity, the cut fell on "does not assume yet",
    /// the half that keeps the dialog honest.
    pub(super) fn line(&self) -> String {
        format!(
            "; the verb maps to role `{}` ({}), which this ebman does not assume yet, so \
             it goes out as that identity",
            sanitize_for_ask(&self.name),
            // Whole: `parse_role_arn` bounds it by IAM's limits, and a
            // value the operator approves is shown whole or refused.
            sanitize_whole(&self.arn)
        )
    }

    /// The plan's `role` field.
    pub(super) fn json(&self) -> String {
        format!(
            "{{\"config\":{},\"arn\":{},\"account\":{},\"name\":{},\"assumed\":false}}",
            util::json_string(&format!("mcp.role.{}", self.key)),
            util::json_string(&self.arn),
            util::json_string(&self.account),
            util::json_string(&self.name),
        )
    }
}

impl PlanRole {
    /// The role a [`RoleCheck::Mapped`](crate::config::RoleCheck) names;
    /// `None` for any other answer, which the gate has already acted on.
    pub(super) fn from_check(check: &crate::config::RoleCheck) -> Option<PlanRole> {
        match check {
            crate::config::RoleCheck::Mapped { role, arn, spec } => Some(PlanRole {
                key: role.clone(),
                arn: arn.arn.clone(),
                account: arn.account.clone(),
                name: arn.name.clone(),
                source_profile: spec.source_profile.clone(),
                external_id: spec.external_id.clone(),
            }),
            _ => None,
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
    /// The role is in another account than the env.
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
            // A verb has one role, so a role in another account cannot
            // be added beside it: writes in this account cannot use the
            // mapping at all.
            Self::WrongAccount { env_account } => format!(
                "remove the verb from mcp.role.NAME.verbs to write in account {env_account} as \
                 the base identity (refused under mcp.require_role)"
            ),
            Self::AccountUnknown => {
                "allow sts:GetCallerIdentity, or remove the verb from mcp.role.NAME.verbs".into()
            }
        }
    }

    pub(super) fn message(&self, verb: WriteVerb, env: &str, role: &PlanRole) -> String {
        match self {
            Self::WrongAccount { env_account } => format!(
                "{} on {env} was not planned: it maps to role `{}` ({}) in account {}, but \
                 {env} is in account {env_account}. A verb's role is in one account, so writes \
                 in account {env_account} cannot use this mapping.",
                verb.label(),
                role.name,
                role.arn,
                role.account,
            ),
            Self::AccountUnknown => format!(
                "{} on {env} was not planned: it maps to role `{}` ({}) in account {}, and \
                 ebman cannot tell which account {env} is in (no account in its ARN, and \
                 sts:GetCallerIdentity failed), so it cannot check the role against it.",
                verb.label(),
                role.name,
                role.arn,
                role.account,
            ),
        }
    }
}

/// The env's account, from its ARN, or else the identity's; then
/// whether `role` can carry the write.
pub(super) fn role_account_refusal(
    role: &PlanRole,
    env: &crate::aws::Environment,
    caller: &CallerIdentity,
) -> Option<RoleAccountRefusal> {
    let env_account = env.arn.as_deref().and_then(arn_account).or(match caller {
        CallerIdentity::Known { account, .. } => Some(account.as_str()),
        CallerIdentity::Unknown { .. } => None,
    });
    match env_account {
        None => Some(RoleAccountRefusal::AccountUnknown),
        Some(a) if a != role.account => Some(RoleAccountRefusal::WrongAccount {
            env_account: a.to_string(),
        }),
        Some(_) => None,
    }
}
