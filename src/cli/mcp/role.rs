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
            sanitize(&self.name, Shown::ASK),
            // Whole: `parse_role_arn` bounds it by IAM's limits, and a
            // value the operator approves is shown whole or refused.
            sanitize(&self.arn, Shown::Whole)
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
