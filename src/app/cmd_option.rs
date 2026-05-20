//! Per-option-settings commands — every arm in this family eventually
//! calls `spawn_option_settings_update(label, to_set, to_remove)`. The
//! arms vary only in (a) input validation / canonicalisation, (b) the
//! namespace + name pair, (c) the on-off-or-string shape of the value.
//!
//! These were ~200 lines of repetitive `match rest.first()` in
//! `execute_command`; lifting them into named methods makes each
//! arm's intent obvious and reduces the dispatch site to a column of
//! one-liners.
//!
//! Fourth slice of the `execute_command` split. Same parent-module
//! visibility pattern as `cmd_overlay` / `cmd_write` / `cmd_view`.

use super::App;

impl App {
    /// `:deployment-policy POLICY` — canonicalises caller-supplied
    /// aliases (lowercase / kebab) into the EB-API form before dispatch.
    pub(crate) fn cmd_deployment_policy(&mut self, rest: &[&str]) {
        let Some(raw) = rest.first().copied() else {
            self.error_message = Some(
                "usage: :deployment-policy POLICY  (POLICY: AllAtOnce | Rolling | RollingWithAdditionalBatch | Immutable | TrafficSplitting)".into(),
            );
            return;
        };
        let canonical = match raw {
            "AllAtOnce" | "all" | "all-at-once" => "AllAtOnce",
            "Rolling" | "rolling" => "Rolling",
            "RollingWithAdditionalBatch" | "rolling-batch" | "rolling-with-additional-batch" => {
                "RollingWithAdditionalBatch"
            }
            "Immutable" | "immutable" => "Immutable",
            "TrafficSplitting" | "traffic-split" | "traffic-splitting" => "TrafficSplitting",
            _ => {
                self.error_message = Some(format!(
                    "unknown deployment policy '{raw}'  (valid: AllAtOnce, Rolling, RollingWithAdditionalBatch, Immutable, TrafficSplitting)"
                ));
                return;
            }
        };
        let ns = "aws:elasticbeanstalk:command";
        self.spawn_option_settings_update(
            format!("deployment-policy {canonical}"),
            vec![(ns.into(), "DeploymentPolicy".into(), canonical.into())],
            vec![],
        );
    }

    pub(crate) fn cmd_rolling_update(&mut self, rest: &[&str]) {
        let ns = "aws:autoscaling:updatepolicy:rollingupdate";
        match rest.first().copied() {
            Some("on") | Some("true") | Some("enable") => {
                self.spawn_option_settings_update(
                    "rolling-update on".into(),
                    vec![(ns.into(), "RollingUpdateEnabled".into(), "true".into())],
                    vec![],
                );
            }
            Some("off") | Some("false") | Some("disable") => {
                self.spawn_option_settings_update(
                    "rolling-update off".into(),
                    vec![(ns.into(), "RollingUpdateEnabled".into(), "false".into())],
                    vec![],
                );
            }
            _ => {
                self.error_message = Some(
                    "usage: :rolling-update on|off  (configures the ASG rolling-update policy)"
                        .into(),
                );
            }
        }
    }

    pub(crate) fn cmd_health_check_url(&mut self, rest: &[&str]) {
        let Some(url) = rest.first().copied() else {
            self.error_message = Some(
                "usage: :health-check-url /path  (path probed for HTTP 200; default '/')".into(),
            );
            return;
        };
        let ns = "aws:elasticbeanstalk:application";
        self.spawn_option_settings_update(
            format!("health-check-url {url}"),
            vec![(
                ns.into(),
                "Application Healthcheck URL".into(),
                url.to_string(),
            )],
            vec![],
        );
    }

    pub(crate) fn cmd_keypair(&mut self, rest: &[&str]) {
        let Some(name) = rest.first().copied() else {
            self.error_message = Some(
                "usage: :keypair NAME  (existing EC2 key pair name; triggers rolling launch-config update)"
                    .into(),
            );
            return;
        };
        let ns = "aws:autoscaling:launchconfiguration";
        self.spawn_option_settings_update(
            format!("keypair {name}"),
            vec![(ns.into(), "EC2KeyName".into(), name.to_string())],
            vec![],
        );
    }

    pub(crate) fn cmd_service_role(&mut self, rest: &[&str]) {
        let Some(role) = rest.first().copied() else {
            self.error_message =
                Some("usage: :service-role ARN_OR_NAME  (IAM role EB itself assumes)".into());
            return;
        };
        let ns = "aws:elasticbeanstalk:environment";
        self.spawn_option_settings_update(
            format!("service-role {role}"),
            vec![(ns.into(), "ServiceRole".into(), role.to_string())],
            vec![],
        );
    }

    pub(crate) fn cmd_instance_profile(&mut self, rest: &[&str]) {
        let Some(name) = rest.first().copied() else {
            self.error_message = Some(
                "usage: :instance-profile NAME  (IAM instance profile attached to EC2 instances)"
                    .into(),
            );
            return;
        };
        let ns = "aws:autoscaling:launchconfiguration";
        self.spawn_option_settings_update(
            format!("instance-profile {name}"),
            vec![(ns.into(), "IamInstanceProfile".into(), name.to_string())],
            vec![],
        );
    }

    pub(crate) fn cmd_public_ip(&mut self, rest: &[&str]) {
        let ns = "aws:ec2:vpc";
        match rest.first().copied() {
            Some("on") | Some("true") | Some("enable") => {
                self.spawn_option_settings_update(
                    "public-ip on".into(),
                    vec![(ns.into(), "AssociatePublicIpAddress".into(), "true".into())],
                    vec![],
                );
            }
            Some("off") | Some("false") | Some("disable") => {
                self.spawn_option_settings_update(
                    "public-ip off".into(),
                    vec![(ns.into(), "AssociatePublicIpAddress".into(), "false".into())],
                    vec![],
                );
            }
            _ => self.error_message = Some("usage: :public-ip on|off".into()),
        }
    }

    pub(crate) fn cmd_elb_scheme(&mut self, rest: &[&str]) {
        let ns = "aws:ec2:vpc";
        match rest.first().copied() {
            Some(s @ ("public" | "internal")) => {
                self.spawn_option_settings_update(
                    format!("elb-scheme {s}"),
                    vec![(ns.into(), "ELBScheme".into(), s.into())],
                    vec![],
                );
            }
            _ => {
                self.error_message = Some(
                    "usage: :elb-scheme public|internal  (internal = VPC-only, public = internet-facing)"
                        .into(),
                );
            }
        }
    }

    /// `:set-option NAMESPACE OPTION VALUE...` — generic escape hatch
    /// for option settings we don't have a friendly named command for.
    /// VALUE tokens joined with single spaces (matches `:tag`).
    pub(crate) fn cmd_set_option(&mut self, rest: &[&str]) {
        match (
            rest.first().copied(),
            rest.get(1).copied(),
            rest.get(2).copied(),
        ) {
            (Some(ns), Some(opt), Some(_)) => {
                let value = rest[2..].join(" ");
                self.spawn_option_settings_update(
                    format!("set-option {ns}.{opt}"),
                    vec![(ns.to_string(), opt.to_string(), value)],
                    vec![],
                );
            }
            _ => {
                self.error_message = Some(
                    "usage: :set-option NAMESPACE OPTION VALUE  (generic escape hatch; VALUE tokens joined with single spaces)"
                        .into(),
                );
            }
        }
    }

    pub(crate) fn cmd_unset_option(&mut self, rest: &[&str]) {
        match (rest.first().copied(), rest.get(1).copied()) {
            (Some(ns), Some(opt)) => {
                self.spawn_option_settings_update(
                    format!("unset-option {ns}.{opt}"),
                    vec![],
                    vec![(ns.to_string(), opt.to_string())],
                );
            }
            _ => self.error_message = Some("usage: :unset-option NAMESPACE OPTION".into()),
        }
    }

    pub(crate) fn cmd_instance_type(&mut self, rest: &[&str]) {
        let Some(t) = rest.first().copied() else {
            self.error_message = Some(
                "usage: :instance-type TYPE  (e.g. t3.medium; triggers rolling launch-config replacement)"
                    .into(),
            );
            return;
        };
        let ns = "aws:autoscaling:launchconfiguration";
        self.spawn_option_settings_update(
            format!("instance-type {t}"),
            vec![(ns.into(), "InstanceType".into(), t.to_string())],
            vec![],
        );
    }
}
