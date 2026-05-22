//! Structured per-env write commands — `:tag`, `:untag`, `:env`,
//! `:capacity`, `:notify`, `:managed-window`, `:logs-stream`. Each
//! manipulates the env's option-settings / tags via the existing
//! `spawn_*` helpers; `:capacity` opens a modal form rather than
//! invoking the AWS path directly.
//!
//! Seventh slice of the `execute_command` split. Same parent-module
//! visibility pattern as the other `cmd_*` sub-modules.

use super::{flatten_err, format_env_vars, parse_named_arg, parse_tag_args, App, AppMsg};

impl App {
    pub(crate) fn cmd_tag(&mut self, rest: &[&str]) {
        match parse_tag_args(rest) {
            None => {
                self.error_message = Some(
                    "usage: :tag KEY VALUE  (value tokens joined with single spaces; no shell quoting — use a separate call to set values with literal multi-spaces)"
                        .into(),
                );
            }
            Some((key, value)) => {
                self.spawn_tag_update(vec![(key, value)], vec![]);
            }
        }
    }

    pub(crate) fn cmd_untag(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            None => self.error_message = Some("usage: :untag KEY".into()),
            Some(key) => self.spawn_tag_update(vec![], vec![key.to_string()]),
        }
    }

    /// `:env list` / `:env set KEY VAL...` / `:env unset KEY` — single
    /// CLI surface for the `aws:elasticbeanstalk:application:environment`
    /// namespace. Triggers an app-server restart per EB (the operator
    /// sees that via the Updating status pill blink + classified label
    /// on the Health tab).
    pub(crate) fn cmd_env(&mut self, rest: &[&str]) {
        let ns = "aws:elasticbeanstalk:application:environment";
        match rest.first().copied() {
            Some("list") | Some("ls") | None => {
                let Some(env) = self.selected_env().cloned() else {
                    self.error_message = Some("no env selected".into());
                    return;
                };
                let app_name = env.application.clone();
                let env_name = env.name.clone();
                let aws = self.aws.clone();
                let tx = self.msg_tx.clone();
                let gen = self.generation;
                let title = format!("env vars — {env_name}");
                self.status_message = Some(format!("fetching env vars for {env_name}…"));
                tokio::spawn(async move {
                    let body = match aws.fetch_env_vars(&app_name, &env_name).await {
                        Ok(vars) if vars.is_empty() => "(no env vars set)".to_string(),
                        Ok(vars) => format_env_vars(&vars),
                        Err(e) => format!("error: {}", flatten_err("fetch_env_vars", e)),
                    };
                    let _ = tx.send(AppMsg::TextOverlay { gen, title, body });
                });
            }
            Some("set") => match (rest.get(1).copied(), rest.get(2).copied()) {
                (Some(key), Some(_)) => {
                    let value = rest[2..].join(" ");
                    self.spawn_option_settings_update(
                        format!("env set {key}"),
                        vec![(ns.into(), key.to_string(), value)],
                        vec![],
                    );
                }
                _ => {
                    self.error_message = Some(
                        "usage: :env set KEY VALUE  (VALUE tokens joined with single spaces; triggers app-server restart)"
                            .into(),
                    );
                }
            },
            Some("unset") | Some("rm") | Some("delete") => match rest.get(1).copied() {
                None => self.error_message = Some("usage: :env unset KEY".into()),
                Some(key) => {
                    self.spawn_option_settings_update(
                        format!("env unset {key}"),
                        vec![],
                        vec![(ns.into(), key.to_string())],
                    );
                }
            },
            Some(other) => {
                self.error_message = Some(format!(
                    "unknown subcommand '{other}'  (use: list | set KEY VAL | unset KEY)"
                ));
            }
        }
    }

    /// `:capacity` — modal form to edit MinSize / MaxSize / InstanceType
    /// / Cooldown in one shot. Pre-fills from
    /// `DescribeConfigurationSettings` via the existing form-loader
    /// path; submit routes through `OptionSettings` mappings.
    pub(crate) fn cmd_capacity(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        let fields = vec![
            crate::form::FormField::integer(
                "min",
                "Min size",
                Some("Minimum ASG size (≥ 1)"),
                Some(1),
                Some(10_000),
                false,
            ),
            crate::form::FormField::integer(
                "max",
                "Max size",
                Some("Maximum ASG size (≥ min)"),
                Some(1),
                Some(10_000),
                false,
            ),
            crate::form::FormField::text(
                "instance_type",
                "Instance type",
                Some("e.g. t3.medium, m6g.large"),
            ),
            crate::form::FormField::integer(
                "cooldown",
                "Cooldown (s)",
                Some("Scaling cooldown in seconds (blank = leave as-is)"),
                Some(0),
                Some(86_400),
                true,
            ),
        ];
        let form = crate::form::Form::loading(
            format!("capacity — {}", env.name),
            env.name.clone(),
            "capacity update".to_string(),
            fields,
            crate::form::FormSubmit::OptionSettings {
                mappings: vec![
                    ("min".into(), "aws:autoscaling:asg".into(), "MinSize".into()),
                    ("max".into(), "aws:autoscaling:asg".into(), "MaxSize".into()),
                    (
                        "instance_type".into(),
                        "aws:autoscaling:launchconfiguration".into(),
                        "InstanceType".into(),
                    ),
                    (
                        "cooldown".into(),
                        "aws:autoscaling:asg".into(),
                        "Cooldown".into(),
                    ),
                ],
            },
        );
        self.open_form(form);
    }

    /// `:scaling-triggers` — modal form for the env's metric-based
    /// autoscaling trigger (`aws:autoscaling:trigger`): which CloudWatch
    /// metric to watch, the statistic / unit / period, the breach
    /// duration, the lower + upper thresholds, and how many instances to
    /// add or remove on a breach. Pre-fills with the env's current
    /// trigger; a field left blank is left unchanged (see
    /// `Form::to_option_settings`).
    pub(crate) fn cmd_scaling_triggers(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        let ns = "aws:autoscaling:trigger";
        let measures = vec![
            "CPUUtilization".to_string(),
            "NetworkOut".to_string(),
            "NetworkIn".to_string(),
            "Latency".to_string(),
            "RequestCount".to_string(),
            "HealthyHostCount".to_string(),
            "UnhealthyHostCount".to_string(),
            "DiskReadBytes".to_string(),
            "DiskWriteBytes".to_string(),
            "DiskReadOps".to_string(),
            "DiskWriteOps".to_string(),
        ];
        let stats = vec![
            "Average".to_string(),
            "Maximum".to_string(),
            "Minimum".to_string(),
            "Sum".to_string(),
        ];
        let fields = vec![
            crate::form::FormField::select(
                "measure",
                "Metric",
                measures,
                Some("CloudWatch metric the trigger watches"),
            ),
            crate::form::FormField::select(
                "statistic",
                "Statistic",
                stats,
                Some("how the metric is aggregated over the period"),
            ),
            crate::form::FormField::text(
                "unit",
                "Unit",
                Some("metric unit — e.g. Percent (CPU), Bytes (Network), Count"),
            ),
            crate::form::FormField::integer(
                "period",
                "Period (min)",
                Some("measurement period in minutes"),
                Some(1),
                Some(600),
                true,
            ),
            crate::form::FormField::integer(
                "breach",
                "Breach duration (min)",
                Some("minutes a threshold must be breached before scaling"),
                Some(1),
                Some(600),
                true,
            ),
            crate::form::FormField::integer(
                "lower_threshold",
                "Lower threshold",
                Some("scale down when the metric falls below this"),
                None,
                None,
                true,
            ),
            crate::form::FormField::integer(
                "upper_threshold",
                "Upper threshold",
                Some("scale up when the metric rises above this"),
                None,
                None,
                true,
            ),
            crate::form::FormField::integer(
                "lower_increment",
                "Lower scale increment",
                Some("instances to add/remove on a lower breach (e.g. -1)"),
                None,
                None,
                true,
            ),
            crate::form::FormField::integer(
                "upper_increment",
                "Upper scale increment",
                Some("instances to add/remove on an upper breach (e.g. 1)"),
                None,
                None,
                true,
            ),
        ];
        let form = crate::form::Form::loading(
            format!("scaling triggers — {}", env.name),
            env.name.clone(),
            "scaling-triggers update".to_string(),
            fields,
            crate::form::FormSubmit::OptionSettings {
                mappings: vec![
                    ("measure".into(), ns.into(), "MeasureName".into()),
                    ("statistic".into(), ns.into(), "Statistic".into()),
                    ("unit".into(), ns.into(), "Unit".into()),
                    ("period".into(), ns.into(), "Period".into()),
                    ("breach".into(), ns.into(), "BreachDuration".into()),
                    ("lower_threshold".into(), ns.into(), "LowerThreshold".into()),
                    ("upper_threshold".into(), ns.into(), "UpperThreshold".into()),
                    (
                        "lower_increment".into(),
                        ns.into(),
                        "LowerBreachScaleIncrement".into(),
                    ),
                    (
                        "upper_increment".into(),
                        ns.into(),
                        "UpperBreachScaleIncrement".into(),
                    ),
                ],
            },
        );
        self.open_form(form);
    }

    /// `:rds-attach` — modal form that couples an RDS instance to the env
    /// via the `aws:rds:dbinstance` namespace; EB provisions the database
    /// on the next environment update. If an RDS instance is already
    /// attached the form pre-fills with its current settings (DBPassword
    /// always blank — EB redacts it), so this doubles as an edit form: a
    /// field left blank is dropped from the update (see
    /// `Form::to_option_settings`), so editing the instance class without
    /// retyping the password is safe.
    /// `DBEngineVersion` and snapshot-restore are deliberately omitted —
    /// EB defaults the version, and `:set-option aws:rds:dbinstance …`
    /// covers the long tail.
    pub(crate) fn cmd_rds_attach(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        let ns = "aws:rds:dbinstance";
        let fields = vec![
            crate::form::FormField::text(
                "db_engine",
                "DB engine",
                Some("postgres / mysql / mariadb / sqlserver-ex / oracle-se2"),
            ),
            crate::form::FormField::text(
                "db_class",
                "Instance class",
                Some("e.g. db.t3.micro, db.t3.small, db.m6g.large"),
            ),
            crate::form::FormField::integer(
                "db_storage",
                "Allocated storage (GiB)",
                Some("5–65536; per-engine minimums apply"),
                Some(5),
                Some(65_536),
                false,
            ),
            crate::form::FormField::text("db_user", "Master username", Some("RDS master user")),
            crate::form::FormField::text(
                "db_password",
                "Master password",
                Some("8+ characters; stored as an EB option setting"),
            ),
            crate::form::FormField::select(
                "db_deletion",
                "Deletion policy",
                vec!["Snapshot".into(), "Delete".into()],
                Some("what happens to the DB when the env is terminated"),
            ),
            crate::form::FormField::boolean(
                "db_multiaz",
                "Multi-AZ",
                Some("provision a standby replica in a second AZ"),
            ),
        ];
        let form = crate::form::Form::loading(
            format!("rds-attach — {}", env.name),
            env.name.clone(),
            "rds attach".to_string(),
            fields,
            crate::form::FormSubmit::OptionSettings {
                mappings: vec![
                    ("db_engine".into(), ns.into(), "DBEngine".into()),
                    ("db_class".into(), ns.into(), "DBInstanceClass".into()),
                    ("db_storage".into(), ns.into(), "DBAllocatedStorage".into()),
                    ("db_user".into(), ns.into(), "DBUser".into()),
                    ("db_password".into(), ns.into(), "DBPassword".into()),
                    ("db_deletion".into(), ns.into(), "DBDeletionPolicy".into()),
                    ("db_multiaz".into(), ns.into(), "MultiAZDatabase".into()),
                ],
            },
        );
        self.open_form(form);
    }

    /// `:rds-detach ENV` — "safe-ify" the env's coupled RDS instance: sets
    /// `DBDeletionPolicy = Snapshot` so the database survives environment
    /// termination (EB takes a final snapshot then). The `ENV` argument
    /// must repeat the selected env's name — a typed-name confirm, since
    /// this changes the fate of a production database.
    ///
    /// This does *not* decouple the DB: Elastic Beanstalk has no detach
    /// operation — an EB-created RDS instance lives in the environment's
    /// CloudFormation stack, and true decoupling needs an environment
    /// rebuild. The command makes the data safe to keep; it doesn't move it.
    pub(crate) fn cmd_rds_detach(&mut self, rest: &[&str]) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        if rest.first().copied() != Some(env.name.as_str()) {
            self.error_message = Some(format!(
                "rds-detach changes a production database — confirm by repeating the env name: :rds-detach {}",
                env.name
            ));
            return;
        }
        self.spawn_option_settings_update(
            "rds-detach (DBDeletionPolicy → Snapshot)".into(),
            vec![(
                "aws:rds:dbinstance".into(),
                "DBDeletionPolicy".into(),
                "Snapshot".into(),
            )],
            vec![],
        );
        self.push_toast(
            crate::app::ToastKind::Info,
            format!(
                "{}: DB deletion policy → Snapshot — the database will survive env \
                 termination. This does not decouple the DB (EB has no detach op).",
                env.name
            ),
        );
    }

    pub(crate) fn cmd_logs_stream(&mut self, rest: &[&str]) {
        let on = match rest.first().copied() {
            Some("on") | Some("true") | Some("enable") => true,
            Some("off") | Some("false") | Some("disable") => false,
            _ => {
                self.error_message = Some(
                    "usage: :logs-stream on|off [--retention DAYS]  (defaults: retention=7 days, delete-on-terminate=false)"
                        .into(),
                );
                return;
            }
        };
        let ns = "aws:elasticbeanstalk:cloudwatch:logs";
        if on {
            let retention = parse_named_arg::<i32>(rest, "--retention").unwrap_or(7);
            self.spawn_option_settings_update(
                format!("logs-stream on (retention={retention}d)"),
                vec![
                    (ns.into(), "StreamLogs".into(), "true".into()),
                    (ns.into(), "DeleteOnTerminate".into(), "false".into()),
                    (ns.into(), "RetentionInDays".into(), retention.to_string()),
                ],
                vec![],
            );
        } else {
            self.spawn_option_settings_update(
                "logs-stream off".into(),
                vec![(ns.into(), "StreamLogs".into(), "false".into())],
                vec![],
            );
        }
    }

    pub(crate) fn cmd_notify(&mut self, rest: &[&str]) {
        let ns = "aws:elasticbeanstalk:sns:topics";
        match rest.first().copied() {
            None => {
                self.error_message = Some(
                    "usage: :notify EMAIL_OR_SNS_ARN | off  (EB creates a topic for emails; ARN attaches an existing topic)"
                        .into(),
                );
            }
            Some("off") => {
                self.spawn_option_settings_update(
                    "notify off".into(),
                    vec![],
                    vec![(ns.into(), "Notification Endpoint".into())],
                );
            }
            Some(endpoint) => {
                self.spawn_option_settings_update(
                    format!("notify {endpoint}"),
                    vec![(
                        ns.into(),
                        "Notification Endpoint".into(),
                        endpoint.to_string(),
                    )],
                    vec![],
                );
            }
        }
    }

    /// `:managed-window DAY HOUR | off` — EB uses cron-like
    /// `Mon:14:00` syntax for PreferredStartTime; this normalises
    /// day-of-week aliases and hour parsing before dispatch.
    pub(crate) fn cmd_managed_window(&mut self, rest: &[&str]) {
        let ns = "aws:elasticbeanstalk:managedactions";
        match (rest.first().copied(), rest.get(1).copied()) {
            (Some("off"), _) => {
                self.spawn_option_settings_update(
                    "managed-window off".into(),
                    vec![(ns.into(), "ManagedActionsEnabled".into(), "false".into())],
                    vec![],
                );
            }
            (Some(day), Some(hour)) => {
                let canonical_day = match day.to_lowercase().as_str() {
                    "mon" | "monday" => "Mon",
                    "tue" | "tuesday" => "Tue",
                    "wed" | "wednesday" => "Wed",
                    "thu" | "thursday" => "Thu",
                    "fri" | "friday" => "Fri",
                    "sat" | "saturday" => "Sat",
                    "sun" | "sunday" => "Sun",
                    _ => {
                        self.error_message = Some(format!(
                            "unknown day '{day}'  (use Mon/Tue/Wed/Thu/Fri/Sat/Sun)"
                        ));
                        return;
                    }
                };
                let Ok(hour_n) = hour.parse::<u8>() else {
                    self.error_message = Some(format!("hour '{hour}' is not 0-23"));
                    return;
                };
                if hour_n > 23 {
                    self.error_message = Some(format!("hour {hour_n} out of range (0-23)"));
                    return;
                }
                let start = format!("{canonical_day}:{hour_n:02}:00");
                self.spawn_option_settings_update(
                    format!("managed-window {start}"),
                    vec![
                        (ns.into(), "ManagedActionsEnabled".into(), "true".into()),
                        (ns.into(), "PreferredStartTime".into(), start),
                    ],
                    vec![],
                );
            }
            _ => {
                self.error_message = Some(
                    "usage: :managed-window DAY HOUR | off  (DAY: Mon/Tue/.../Sun; HOUR: 0-23)"
                        .into(),
                );
            }
        }
    }
}
