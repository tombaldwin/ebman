use aws_config::{Region, SdkConfig};
use aws_sdk_cloudwatch::Client as CwClient;
use aws_sdk_cloudwatchlogs::Client as CwLogsClient;
use aws_sdk_ec2::Client as Ec2Client;
use aws_sdk_elasticbeanstalk::Client;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_sqs::Client as SqsClient;
use aws_sdk_sts::Client as StsClient;
use chrono::{DateTime, Utc};
use color_eyre::eyre::{eyre, Result};

#[derive(Clone, Debug)]
pub struct Event {
    pub at: Option<DateTime<Utc>>,
    pub env: String,
    pub application: String,
    pub message: String,
    pub severity: String,
}

#[derive(Clone, Debug)]
pub struct CwAlarm {
    pub name: String,
    pub state: String, // OK / ALARM / INSUFFICIENT_DATA
    pub state_reason: String,
    pub metric_name: String,
    pub namespace: String,
}

#[derive(Clone, Debug, Default)]
pub struct MetricSeries {
    pub id: String,    // stable, e.g. "health"
    pub label: String, // CloudWatch label
    pub points: Vec<(DateTime<Utc>, f64)>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkerQueues {
    pub main_url: Option<String>,
    pub dlq_url: Option<String>,
    pub main_stats: Option<QueueStats>,
    pub dlq_stats: Option<QueueStats>,
}

#[derive(Clone, Debug, Default)]
pub struct QueueStats {
    pub visible: i64,
    pub in_flight: i64,
    pub delayed: i64,
}

#[derive(Clone, Debug)]
pub struct QueueMessage {
    pub id: String,
    pub receipt_handle: String,
    pub body: String,
    pub receive_count: i64,
    pub sent_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct Instance {
    pub id: String,
    pub health: String, // Ok / Warning / Degraded / Severe / Info / NoData / Unknown / Pending
    pub color: String,  // Green / Yellow / Red / Grey
    pub causes: Vec<String>,
    pub instance_type: String,
    pub availability_zone: String,
    pub launched_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct Application {
    pub name: String,
    pub description: String,
    pub date_created: Option<DateTime<Utc>>,
    pub date_updated: Option<DateTime<Utc>>,
    pub version_count: usize,
    pub templates: Vec<String>,
}

/// Result of `fetch_env_vpc_context` — the env's VPC plus the option-
/// settings selections the `:subnets` / `:elb-subnets` / `:security-groups`
/// pickers need for their pre-fill. Each field is `None` / empty when the
/// env doesn't override that option (EB uses its account-default in that
/// case).
#[derive(Clone, Debug, Default)]
pub struct EnvVpcContext {
    pub vpc_id: Option<String>,
    pub subnets: Vec<String>,
    /// ELB subnets (`aws:ec2:vpc.ELBSubnets`). Web-tier envs typically
    /// attach the ELB to a separate subnet set than the instance subnets;
    /// worker envs leave this empty.
    pub elb_subnets: Vec<String>,
    pub security_groups: Vec<String>,
}

/// One subnet in a VPC. Used by `:subnets` to populate the picker.
#[derive(Clone, Debug)]
pub struct SubnetInfo {
    pub id: String,
    pub availability_zone: String,
    pub cidr_block: String,
    /// Friendly name from the `Name` tag, if any.
    pub name_tag: Option<String>,
}

/// One security group in a VPC. Used by `:security-groups`.
#[derive(Clone, Debug)]
pub struct SecurityGroupInfo {
    pub id: String,
    pub group_name: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct CustomPlatform {
    pub arn: String,
    pub branch: String,
    pub version: String,
    pub status: String,
    pub lifecycle: String,
}

#[derive(Clone, Debug)]
pub struct AppVersion {
    pub label: String,
    pub description: String,
    pub created: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct Environment {
    pub name: String,
    pub application: String,
    pub status: String,
    pub health: String,
    pub platform: String, // family + version, e.g. "Java 17"
    pub tier: String,     // "Web" / "Worker" / "?"
    pub cname: String,
    pub version_label: String,
    pub arn: Option<String>,
    pub updated: Option<DateTime<Utc>>,
    /// Internal EB environment ID (e.g. `e-abcdef1234`). Required by APIs
    /// that snapshot config from a live env (CreateConfigurationTemplate).
    pub id: Option<String>,
    /// Region the env was discovered in, when results were fanned out across
    /// multiple regions. `None` in single-region mode.
    pub region: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AwsContext {
    pub region: String,
    pub profile: Option<String>,
    pub account_id: Option<String>,
    pub caller_arn: Option<String>,
}

/// One row passed to `fetch_custom_env_metrics`. The shape is wide enough
/// that clippy complains if used inline (`type_complexity` lint), so this
/// alias keeps call-sites tidy.
pub type CustomMetricQuery = (String, String, String, String, Vec<(String, String)>);

/// One event from a CloudWatch Logs stream — server-side timestamp + the
/// stream it came from + the raw message. `:logs-tail` builds these from
/// FilterLogEvents and renders them in chronological order.
#[derive(Clone, Debug)]
pub struct LogEvent {
    pub timestamp_ms: i64,
    pub stream: String,
    pub message: String,
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
    config: SdkConfig,
    pub context: AwsContext,
}

impl AwsClient {
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

        let region = config
            .region()
            .map(|r| r.as_ref().to_string())
            .unwrap_or_else(|| "unknown".to_string());
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
            config,
            context: AwsContext {
                region,
                profile,
                account_id: None,
                caller_arn: None,
            },
        })
    }

    /// Build a fully-mocked `AwsClient` for unit tests. The caller supplies
    /// pre-built (typically `mock_client!`-backed) sub-clients; any client
    /// not exercised by the test can stay as a plain SDK-default instance.
    /// Tests should not assume any of the sub-clients can talk to a real
    /// endpoint — the default ones will fail if a non-mocked code path is
    /// reached, which is exactly the signal we want.
    #[cfg(test)]
    pub fn for_tests(
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
        Self {
            client,
            sqs,
            cw,
            cw_logs,
            s3,
            ec2,
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
            .map_err(|e| eyre!("sts get-caller-identity failed: {e}"))?;
        Ok(Identity {
            account_id: ident.account,
            caller_arn: ident.arn,
        })
    }

    pub async fn list_events(&self, max: i32) -> Result<Vec<Event>> {
        self.list_events_inner(None, max).await
    }

    pub async fn list_events_for_env(&self, env_name: &str, max: i32) -> Result<Vec<Event>> {
        self.list_events_inner(Some(env_name.to_string()), max)
            .await
    }

    async fn list_events_inner(&self, env_name: Option<String>, max: i32) -> Result<Vec<Event>> {
        let mut req = self.client.describe_events().max_records(max);
        if let Some(n) = env_name {
            req = req.environment_name(n);
        }
        let resp = req.send().await?;
        let events = resp
            .events
            .unwrap_or_default()
            .into_iter()
            .map(|e| Event {
                at: e
                    .event_date
                    .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
                env: e.environment_name.unwrap_or_default(),
                application: e.application_name.unwrap_or_default(),
                message: e.message.unwrap_or_default(),
                severity: e
                    .severity
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_else(|| "INFO".to_string()),
            })
            .collect();
        Ok(events)
    }

    /// Full `DescribeEnvironmentResources` dump for an env, formatted as a
    /// human-readable string suitable for an overlay. Covers ASGs,
    /// instances, launch configurations, launch templates, load balancers,
    /// trigger names, and SQS queues — i.e. every infra resource EB
    /// manages for the env. Useful for "what's actually under this env?".
    pub async fn describe_env_resources(&self, env_name: &str) -> Result<String> {
        let resp = self
            .client
            .describe_environment_resources()
            .environment_name(env_name)
            .send()
            .await
            .map_err(|e| eyre!("DescribeEnvironmentResources failed: {e}"))?;
        let res = resp
            .environment_resources
            .ok_or_else(|| eyre!("no environment_resources in response"))?;
        let mut out = String::new();
        out.push_str(&format!("Resources for {env_name}\n"));
        out.push_str("───────────────────────────────────────\n\n");
        let asgs = res.auto_scaling_groups.unwrap_or_default();
        out.push_str(&format!("Auto-scaling groups ({})\n", asgs.len()));
        for a in &asgs {
            out.push_str(&format!("  ▸ {}\n", a.name.as_deref().unwrap_or("?")));
        }
        let instances = res.instances.unwrap_or_default();
        out.push_str(&format!("\nInstances ({})\n", instances.len()));
        for i in &instances {
            out.push_str(&format!("  ▸ {}\n", i.id.as_deref().unwrap_or("?")));
        }
        let lcs = res.launch_configurations.unwrap_or_default();
        if !lcs.is_empty() {
            out.push_str(&format!("\nLaunch configurations ({})\n", lcs.len()));
            for l in &lcs {
                out.push_str(&format!("  ▸ {}\n", l.name.as_deref().unwrap_or("?")));
            }
        }
        let lts = res.launch_templates.unwrap_or_default();
        if !lts.is_empty() {
            out.push_str(&format!("\nLaunch templates ({})\n", lts.len()));
            for l in &lts {
                out.push_str(&format!("  ▸ {}\n", l.id.as_deref().unwrap_or("?")));
            }
        }
        let lbs = res.load_balancers.unwrap_or_default();
        out.push_str(&format!("\nLoad balancers ({})\n", lbs.len()));
        for l in &lbs {
            out.push_str(&format!("  ▸ {}\n", l.name.as_deref().unwrap_or("?")));
        }
        let triggers = res.triggers.unwrap_or_default();
        if !triggers.is_empty() {
            out.push_str(&format!("\nTriggers ({})\n", triggers.len()));
            for t in &triggers {
                out.push_str(&format!("  ▸ {}\n", t.name.as_deref().unwrap_or("?")));
            }
        }
        let queues = res.queues.unwrap_or_default();
        if !queues.is_empty() {
            out.push_str(&format!("\nQueues ({})\n", queues.len()));
            for q in &queues {
                out.push_str(&format!(
                    "  ▸ {}\n      {}\n",
                    q.name.as_deref().unwrap_or("?"),
                    q.url.as_deref().unwrap_or("?")
                ));
            }
        }
        out.push_str("\nesc / q to close");
        Ok(out)
    }

    /// Resolve the worker queue URL (and DLQ URL) for an env. EB autocreates
    /// queues when the user doesn't override `WorkerQueueURL`, and in that
    /// (common) case the option value comes back empty — so we ask
    /// `DescribeEnvironmentResources` first, which exposes the actual queue
    /// URLs under named entries (`WorkerQueue`, `WorkerDeadLetterQueue`).
    /// Falls back to the option-settings path for users who override the
    /// URL explicitly.
    pub async fn describe_worker_queues(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<WorkerQueues> {
        let mut main_url: Option<String> = None;
        let mut dlq_url: Option<String> = None;

        // Primary path: ask EB for the env's resources. Includes the URLs of
        // the queues EB created automatically when WorkerQueueURL is empty.
        if let Ok(resp) = self
            .client
            .describe_environment_resources()
            .environment_name(env_name)
            .send()
            .await
        {
            if let Some(res) = resp.environment_resources {
                for q in res.queues.unwrap_or_default() {
                    let name = q.name.unwrap_or_default();
                    let url = q.url.unwrap_or_default();
                    if url.is_empty() {
                        continue;
                    }
                    match name.as_str() {
                        "WorkerQueue" => main_url = Some(url),
                        "WorkerDeadLetterQueue" => dlq_url = Some(url),
                        _ => {}
                    }
                }
            }
        }

        // Fallback / override: look at user-supplied option settings in case
        // the env explicitly points at a queue the user manages outside EB.
        if main_url.is_none() || dlq_url.is_none() {
            if let Ok(resp) = self
                .client
                .describe_configuration_settings()
                .application_name(application_name)
                .environment_name(env_name)
                .send()
                .await
            {
                for setting in resp.configuration_settings.unwrap_or_default() {
                    for opt in setting.option_settings.unwrap_or_default() {
                        let ns = opt.namespace.unwrap_or_default();
                        let name = opt.option_name.unwrap_or_default();
                        if ns != "aws:elasticbeanstalk:sqsd" {
                            continue;
                        }
                        match name.as_str() {
                            "WorkerQueueURL" => {
                                let v = opt.value.unwrap_or_default();
                                if !v.is_empty() && main_url.is_none() {
                                    main_url = Some(v);
                                }
                            }
                            "DeadLetterQueueURL" => {
                                let v = opt.value.unwrap_or_default();
                                if !v.is_empty() && dlq_url.is_none() {
                                    dlq_url = Some(v);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // If we still have a main queue but no DLQ URL, derive one by SQS naming convention.
        if let (Some(main), None) = (&main_url, &dlq_url) {
            dlq_url = derive_dlq_url(main);
        }

        let main_stats = if let Some(u) = &main_url {
            self.queue_stats(u).await.ok()
        } else {
            None
        };
        let dlq_stats = if let Some(u) = &dlq_url {
            self.queue_stats(u).await.ok()
        } else {
            None
        };

        Ok(WorkerQueues {
            main_url,
            dlq_url,
            main_stats,
            dlq_stats,
        })
    }

    pub async fn queue_stats(&self, queue_url: &str) -> Result<QueueStats> {
        use aws_sdk_sqs::types::QueueAttributeName as Q;
        let resp = self
            .sqs
            .get_queue_attributes()
            .queue_url(queue_url)
            .attribute_names(Q::ApproximateNumberOfMessages)
            .attribute_names(Q::ApproximateNumberOfMessagesNotVisible)
            .attribute_names(Q::ApproximateNumberOfMessagesDelayed)
            .send()
            .await?;
        let attrs = resp.attributes.unwrap_or_default();
        let parse = |k: Q| -> i64 {
            attrs
                .get(&k)
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0)
        };
        Ok(QueueStats {
            visible: parse(Q::ApproximateNumberOfMessages),
            in_flight: parse(Q::ApproximateNumberOfMessagesNotVisible),
            delayed: parse(Q::ApproximateNumberOfMessagesDelayed),
        })
    }

    /// Peek up to `max` messages from `queue_url` with a short visibility
    /// timeout (so we don't disrupt real consumers). SQS `ReceiveMessage`
    /// returns at most 10 per call AND, because the queue is partitioned, a
    /// single call commonly returns fewer than requested even with a deep
    /// queue. We therefore loop with a short long-poll, accumulating unique
    /// messages until we hit `max`, until two consecutive calls return zero,
    /// or until the per-call budget runs out. De-duplication is by message
    /// id — a partition can return the same message across calls within the
    /// visibility-timeout window if we're slow.
    pub async fn peek_messages(&self, queue_url: &str, max: i32) -> Result<Vec<QueueMessage>> {
        use aws_sdk_sqs::types::MessageSystemAttributeName as M;
        let target = max.clamp(1, 100) as usize;
        let mut out: Vec<QueueMessage> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut empty_in_a_row = 0;
        // Cap total iterations so a sparse queue can't spin forever.
        for _ in 0..((target / 10).max(1) + 4) {
            if out.len() >= target {
                break;
            }
            let resp = self
                .sqs
                .receive_message()
                .queue_url(queue_url)
                .max_number_of_messages(((target - out.len()).clamp(1, 10)) as i32)
                // Visibility timeout long enough to read + dedupe across the
                // loop without holding messages back from real consumers for
                // any noticeable time.
                .visibility_timeout(5)
                // Short long-poll: SQS will wait up to 1s for messages from
                // additional partitions before returning. Trades a little
                // latency for much better recall.
                .wait_time_seconds(1)
                .message_system_attribute_names(M::ApproximateReceiveCount)
                .message_system_attribute_names(M::SentTimestamp)
                .send()
                .await
                .map_err(|e| eyre!("ReceiveMessage failed: {e}"))?;
            let batch = resp.messages.unwrap_or_default();
            if batch.is_empty() {
                empty_in_a_row += 1;
                if empty_in_a_row >= 2 {
                    break;
                }
                continue;
            }
            empty_in_a_row = 0;
            for m in batch {
                let id = m.message_id.clone().unwrap_or_default();
                if !id.is_empty() && !seen.insert(id.clone()) {
                    continue;
                }
                let attrs = m.attributes.unwrap_or_default();
                let receive_count = attrs
                    .get(&M::ApproximateReceiveCount)
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0);
                let sent_at = attrs
                    .get(&M::SentTimestamp)
                    .and_then(|v| v.parse::<i64>().ok())
                    .and_then(DateTime::from_timestamp_millis);
                out.push(QueueMessage {
                    id,
                    receipt_handle: m.receipt_handle.unwrap_or_default(),
                    body: m.body.unwrap_or_default(),
                    receive_count,
                    sent_at,
                });
                if out.len() >= target {
                    break;
                }
            }
        }
        Ok(out)
    }

    pub async fn send_message(&self, queue_url: &str, body: &str) -> Result<()> {
        self.sqs
            .send_message()
            .queue_url(queue_url)
            .message_body(body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn delete_message(&self, queue_url: &str, receipt_handle: &str) -> Result<()> {
        self.sqs
            .delete_message()
            .queue_url(queue_url)
            .receipt_handle(receipt_handle)
            .send()
            .await?;
        Ok(())
    }

    /// Describe metric alarms whose first dimension references the given env.
    /// CloudWatch doesn't expose a server-side filter by dimension, so we pull
    /// alarms in the AWS/ElasticBeanstalk namespace and filter client-side.
    pub async fn list_alarms_for_env(&self, env_name: &str) -> Result<Vec<CwAlarm>> {
        let mut out = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut req = self.cw.describe_alarms();
            if let Some(t) = next_token.take() {
                req = req.next_token(t);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| eyre!("DescribeAlarms failed: {e}"))?;
            for a in resp.metric_alarms.unwrap_or_default() {
                let dims = a.dimensions.clone().unwrap_or_default();
                let touches = dims.iter().any(|d| d.value.as_deref() == Some(env_name));
                if !touches {
                    continue;
                }
                out.push(CwAlarm {
                    name: a.alarm_name.unwrap_or_default(),
                    state: a
                        .state_value
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_default(),
                    state_reason: a.state_reason.unwrap_or_default(),
                    metric_name: a.metric_name.unwrap_or_default(),
                    namespace: a.namespace.unwrap_or_default(),
                });
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Create or update a CloudWatch metric alarm in the
    /// `AWS/ElasticBeanstalk` namespace, dimensioned by `EnvironmentName`.
    /// `metric_name` should be one of the env-scoped metrics already in our
    /// Metrics tab (EnvironmentHealth / ApplicationRequests4xx /
    /// ApplicationRequests5xx / ApplicationLatencyP90) — anything else and
    /// the alarm will be created with no datapoints. No alarm actions are
    /// attached; operators can wire SNS via the console or CLI later.
    #[allow(clippy::too_many_arguments)]
    pub async fn put_env_metric_alarm(
        &self,
        alarm_name: &str,
        env_name: &str,
        metric_name: &str,
        threshold: f64,
        comparison_operator: &str,
        period_secs: i32,
        evaluation_periods: i32,
        statistic: &str,
    ) -> Result<()> {
        use aws_sdk_cloudwatch::types::{ComparisonOperator, Dimension, Statistic};
        // The smithy enums round-trip "unknown" inputs through their Unknown
        // variant; checking `as_str()` against the original input is the
        // documented way to detect that case without matching on the
        // deprecated variant.
        let op = ComparisonOperator::from(comparison_operator);
        if op.as_str() != comparison_operator {
            return Err(eyre!(
                "unknown comparison operator '{comparison_operator}' \
                 (valid: GreaterThanThreshold, GreaterThanOrEqualToThreshold, \
                 LessThanThreshold, LessThanOrEqualToThreshold)"
            ));
        }
        let stat = Statistic::from(statistic);
        if stat.as_str() != statistic {
            return Err(eyre!(
                "unknown statistic '{statistic}' (valid: Average, Sum, Maximum, Minimum, SampleCount)"
            ));
        }
        let dim = Dimension::builder()
            .name("EnvironmentName")
            .value(env_name)
            .build();
        self.cw
            .put_metric_alarm()
            .alarm_name(alarm_name)
            .alarm_description(format!("ebman: {metric_name} alarm on {env_name}"))
            .namespace("AWS/ElasticBeanstalk")
            .metric_name(metric_name)
            .dimensions(dim)
            .comparison_operator(op)
            .threshold(threshold)
            .period(period_secs)
            .evaluation_periods(evaluation_periods)
            .statistic(stat)
            .treat_missing_data("notBreaching")
            .send()
            .await
            .map_err(|e| eyre!("PutMetricAlarm failed: {e}"))?;
        Ok(())
    }

    /// Fetch the current env vars for an environment from
    /// `DescribeConfigurationSettings` filtered to the
    /// `aws:elasticbeanstalk:application:environment` namespace. Returns
    /// sorted `(KEY, VALUE)` pairs.
    /// Fetch every option setting for a live env. Used by the modal-form
    /// pre-fill: callers filter the result down to the `(namespace, option_name)`
    /// pairs their form cares about. Returns `(namespace, option_name, value)`
    /// triples.
    pub async fn fetch_env_option_settings(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .environment_name(env_name)
            .send()
            .await
            .map_err(|e| eyre!("DescribeConfigurationSettings(env) failed: {e}"))?;
        let out = resp
            .configuration_settings
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.option_settings.unwrap_or_default())
            .map(|o| {
                (
                    o.namespace.unwrap_or_default(),
                    o.option_name.unwrap_or_default(),
                    o.value.unwrap_or_default(),
                )
            })
            .collect();
        Ok(out)
    }

    /// Pull the env's VPC id plus the currently-selected subnet and
    /// security-group IDs from EB option settings in a single round-trip.
    /// `:subnets` and `:security-groups` both call this — VPC id drives
    /// the subsequent EC2 list call, the existing selections drive the
    /// MultiSelect pre-fill.
    pub async fn fetch_env_vpc_context(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<EnvVpcContext> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .environment_name(env_name)
            .send()
            .await
            .map_err(|e| eyre!("DescribeConfigurationSettings(env) failed: {e}"))?;
        let mut ctx = EnvVpcContext::default();
        for setting in resp.configuration_settings.unwrap_or_default() {
            for opt in setting.option_settings.unwrap_or_default() {
                let ns = opt.namespace.unwrap_or_default();
                let name = opt.option_name.unwrap_or_default();
                let value = opt.value.unwrap_or_default();
                match (ns.as_str(), name.as_str()) {
                    ("aws:ec2:vpc", "VPCId") if !value.is_empty() => {
                        ctx.vpc_id = Some(value);
                    }
                    ("aws:ec2:vpc", "Subnets") if !value.is_empty() => {
                        ctx.subnets = split_csv(&value);
                    }
                    ("aws:ec2:vpc", "ELBSubnets") if !value.is_empty() => {
                        ctx.elb_subnets = split_csv(&value);
                    }
                    ("aws:autoscaling:launchconfiguration", "SecurityGroups")
                        if !value.is_empty() =>
                    {
                        ctx.security_groups = split_csv(&value);
                    }
                    _ => {}
                }
            }
        }
        Ok(ctx)
    }

    /// List subnets in a VPC, ordered by AZ then CIDR for stable picker
    /// rows. Returns the wide rows the `:subnets` picker needs (id + AZ
    /// + CIDR + Name tag) so callers don't need a second round-trip.
    pub async fn list_subnets_in_vpc(&self, vpc_id: &str) -> Result<Vec<SubnetInfo>> {
        use aws_sdk_ec2::types::Filter;
        let resp = self
            .ec2
            .describe_subnets()
            .filters(
                Filter::builder()
                    .name("vpc-id")
                    .values(vpc_id.to_string())
                    .build(),
            )
            .send()
            .await
            .map_err(|e| eyre!("DescribeSubnets failed: {e}"))?;
        let mut out: Vec<SubnetInfo> = resp
            .subnets
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                let name_tag = s.tags.as_ref().and_then(|tags| {
                    tags.iter()
                        .find(|t| t.key.as_deref() == Some("Name"))
                        .and_then(|t| t.value.clone())
                });
                SubnetInfo {
                    id: s.subnet_id.unwrap_or_default(),
                    availability_zone: s.availability_zone.unwrap_or_default(),
                    cidr_block: s.cidr_block.unwrap_or_default(),
                    name_tag,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            a.availability_zone
                .cmp(&b.availability_zone)
                .then(a.cidr_block.cmp(&b.cidr_block))
        });
        Ok(out)
    }

    /// List security groups in a VPC, ordered by name for stable picker
    /// rows.
    pub async fn list_security_groups_in_vpc(
        &self,
        vpc_id: &str,
    ) -> Result<Vec<SecurityGroupInfo>> {
        use aws_sdk_ec2::types::Filter;
        let resp = self
            .ec2
            .describe_security_groups()
            .filters(
                Filter::builder()
                    .name("vpc-id")
                    .values(vpc_id.to_string())
                    .build(),
            )
            .send()
            .await
            .map_err(|e| eyre!("DescribeSecurityGroups failed: {e}"))?;
        let mut out: Vec<SecurityGroupInfo> = resp
            .security_groups
            .unwrap_or_default()
            .into_iter()
            .map(|g| SecurityGroupInfo {
                id: g.group_id.unwrap_or_default(),
                group_name: g.group_name.unwrap_or_default(),
                description: g.description.unwrap_or_default(),
            })
            .collect();
        out.sort_by(|a, b| a.group_name.cmp(&b.group_name));
        Ok(out)
    }

    pub async fn fetch_env_vars(
        &self,
        application_name: &str,
        env_name: &str,
    ) -> Result<Vec<(String, String)>> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .environment_name(env_name)
            .send()
            .await
            .map_err(|e| eyre!("DescribeConfigurationSettings(env) failed: {e}"))?;
        let mut out: Vec<(String, String)> = resp
            .configuration_settings
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.option_settings.unwrap_or_default())
            .filter(|o| {
                o.namespace.as_deref() == Some("aws:elasticbeanstalk:application:environment")
            })
            .map(|o| {
                (
                    o.option_name.unwrap_or_default(),
                    o.value.unwrap_or_default(),
                )
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Update an env's option settings — `to_set` is `(namespace, option_name,
    /// value)` triples to add or overwrite; `to_remove` is `(namespace,
    /// option_name)` pairs to clear back to defaults. EB applies the change
    /// as a rolling update (or instantly for non-disruptive options).
    pub async fn update_env_option_settings(
        &self,
        env_name: &str,
        to_set: &[(String, String, String)],
        to_remove: &[(String, String)],
    ) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::{ConfigurationOptionSetting, OptionSpecification};
        if to_set.is_empty() && to_remove.is_empty() {
            return Err(eyre!("update_env_option_settings: nothing to do"));
        }
        let mut req = self.client.update_environment().environment_name(env_name);
        for (ns, name, value) in to_set {
            req = req.option_settings(
                ConfigurationOptionSetting::builder()
                    .namespace(ns)
                    .option_name(name)
                    .value(value)
                    .build(),
            );
        }
        for (ns, name) in to_remove {
            req = req.options_to_remove(
                OptionSpecification::builder()
                    .namespace(ns)
                    .option_name(name)
                    .build(),
            );
        }
        req.send()
            .await
            .map_err(|e| eyre!("UpdateEnvironment(option_settings) failed: {e}"))?;
        Ok(())
    }

    /// Discover the CloudWatch Logs groups an EB env streams to. EB names
    /// them under the prefix `/aws/elasticbeanstalk/{env}/...` so we
    /// `DescribeLogGroups` with that prefix. Returns sorted group names;
    /// empty if `:logs-stream on` hasn't been issued for the env.
    pub async fn discover_env_log_groups(&self, env_name: &str) -> Result<Vec<String>> {
        let prefix = format!("/aws/elasticbeanstalk/{env_name}/");
        let mut out: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut req = self
                .cw_logs
                .describe_log_groups()
                .log_group_name_prefix(&prefix);
            if let Some(t) = next_token.take() {
                req = req.next_token(t);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| eyre!("DescribeLogGroups failed: {e}"))?;
            for g in resp.log_groups.unwrap_or_default() {
                if let Some(name) = g.log_group_name {
                    out.push(name);
                }
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        out.sort();
        Ok(out)
    }

    /// Fetch events from one CW Logs group since `since_ms` (Unix
    /// milliseconds). Uses `FilterLogEvents` so the result spans all log
    /// streams in the group in chronological order — that's how an EB-tier
    /// log group works (one stream per instance). The returned tuple is
    /// `(events, next_since_ms)` where `next_since_ms` is the highest
    /// timestamp + 1 we saw, suitable to pass back on the next call.
    pub async fn fetch_recent_log_events(
        &self,
        log_group: &str,
        since_ms: i64,
        limit: i32,
    ) -> Result<(Vec<LogEvent>, i64)> {
        let resp = self
            .cw_logs
            .filter_log_events()
            .log_group_name(log_group)
            .start_time(since_ms)
            .limit(limit)
            .send()
            .await
            .map_err(|e| eyre!("FilterLogEvents failed: {e}"))?;
        let mut out: Vec<LogEvent> = Vec::new();
        let mut max_ts = since_ms;
        for e in resp.events.unwrap_or_default() {
            let ts = e.timestamp.unwrap_or(since_ms);
            if ts > max_ts {
                max_ts = ts;
            }
            out.push(LogEvent {
                timestamp_ms: ts,
                stream: e.log_stream_name.unwrap_or_default(),
                message: e.message.unwrap_or_default(),
            });
        }
        // Move the cursor past the last event we saw so the next poll
        // doesn't return it again.
        let next_since = if max_ts > since_ms {
            max_ts + 1
        } else {
            since_ms
        };
        Ok((out, next_since))
    }

    /// Delete one or more CloudWatch alarms by name.
    pub async fn delete_alarms(&self, names: &[String]) -> Result<()> {
        if names.is_empty() {
            return Ok(());
        }
        let mut req = self.cw.delete_alarms();
        for n in names {
            req = req.alarm_names(n);
        }
        req.send()
            .await
            .map_err(|e| eyre!("DeleteAlarms failed: {e}"))?;
        Ok(())
    }

    /// Pull a handful of useful EB metrics for one env, from CloudWatch.
    /// Returns an empty Vec for queries the API filtered out.
    pub async fn fetch_env_metrics(
        &self,
        env_name: &str,
        range_secs: i64,
    ) -> Result<Vec<MetricSeries>> {
        use aws_sdk_cloudwatch::types::{Dimension, Metric, MetricDataQuery, MetricStat};

        let end = Utc::now();
        let start = end - chrono::Duration::seconds(range_secs);

        let dim = Dimension::builder()
            .name("EnvironmentName")
            .value(env_name)
            .build();

        let make_query = |id: &str, name: &str, stat: &str| -> MetricDataQuery {
            let metric = Metric::builder()
                .namespace("AWS/ElasticBeanstalk")
                .metric_name(name)
                .dimensions(dim.clone())
                .build();
            let ms = MetricStat::builder()
                .metric(metric)
                .period(60)
                .stat(stat)
                .build();
            MetricDataQuery::builder().id(id).metric_stat(ms).build()
        };

        let resp = self
            .cw
            .get_metric_data()
            .start_time(to_smithy(start))
            .end_time(to_smithy(end))
            .metric_data_queries(make_query("health", "EnvironmentHealth", "Maximum"))
            .metric_data_queries(make_query("req4xx", "ApplicationRequests4xx", "Sum"))
            .metric_data_queries(make_query("req5xx", "ApplicationRequests5xx", "Sum"))
            .metric_data_queries(make_query("p90", "ApplicationLatencyP90", "Average"))
            .send()
            .await?;

        let order = ["health", "req4xx", "req5xx", "p90"];
        let labels: std::collections::HashMap<&str, (&str, &str)> = [
            ("health", ("Env Health (0–25)", "score")),
            ("req4xx", ("4xx Requests / min", "count")),
            ("req5xx", ("5xx Requests / min", "count")),
            ("p90", ("Latency P90", "s")),
        ]
        .into_iter()
        .collect();

        let mut by_id: std::collections::HashMap<String, MetricSeries> =
            std::collections::HashMap::new();
        for r in resp.metric_data_results.unwrap_or_default() {
            let id = r.id.unwrap_or_default();
            let display = labels
                .get(id.as_str())
                .copied()
                .map(|(d, _)| d.to_string())
                .unwrap_or_else(|| id.clone());
            let timestamps = r.timestamps.unwrap_or_default();
            let values = r.values.unwrap_or_default();
            let mut points: Vec<(DateTime<Utc>, f64)> = timestamps
                .iter()
                .zip(values.iter())
                .filter_map(|(ts, v)| {
                    DateTime::<Utc>::from_timestamp(ts.secs(), ts.subsec_nanos()).map(|t| (t, *v))
                })
                .collect();
            points.sort_by_key(|(t, _)| *t);
            by_id.insert(
                id.clone(),
                MetricSeries {
                    id,
                    label: display,
                    points,
                },
            );
        }

        Ok(order.iter().filter_map(|id| by_id.remove(*id)).collect())
    }

    /// Fetch user-defined metric series for one env. Each spec is
    /// `(label, namespace, name, stat, dimensions)` — `dimensions` are
    /// explicit overrides; when empty the call falls back to the env-scoped
    /// `EnvironmentName=env_name` dimension (the common case for
    /// `AWS/ElasticBeanstalk` metrics). Returns the series in the same
    /// order as `specs` so operators see their additions in add-order.
    pub async fn fetch_custom_env_metrics(
        &self,
        env_name: &str,
        range_secs: i64,
        specs: &[CustomMetricQuery],
    ) -> Result<Vec<MetricSeries>> {
        use aws_sdk_cloudwatch::types::{Dimension, Metric, MetricDataQuery, MetricStat};
        if specs.is_empty() {
            return Ok(Vec::new());
        }
        let end = Utc::now();
        let start = end - chrono::Duration::seconds(range_secs);

        let mut req = self
            .cw
            .get_metric_data()
            .start_time(to_smithy(start))
            .end_time(to_smithy(end));
        // CloudWatch's GetMetricData requires the `id` field to be a valid
        // metric reference (lowercase alpha + numeric + underscore, starts
        // with a letter). We use `m{i}` to dodge label-vs-id concerns.
        let mut id_to_label: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (i, (label, namespace, name, stat, dims)) in specs.iter().enumerate() {
            let id = format!("m{i}");
            let mut metric_builder = Metric::builder().namespace(namespace).metric_name(name);
            if dims.is_empty() {
                metric_builder = metric_builder.dimensions(
                    Dimension::builder()
                        .name("EnvironmentName")
                        .value(env_name)
                        .build(),
                );
            } else {
                for (k, v) in dims {
                    metric_builder =
                        metric_builder.dimensions(Dimension::builder().name(k).value(v).build());
                }
            }
            let ms = MetricStat::builder()
                .metric(metric_builder.build())
                .period(60)
                .stat(stat)
                .build();
            id_to_label.insert(id.clone(), label.clone());
            req =
                req.metric_data_queries(MetricDataQuery::builder().id(id).metric_stat(ms).build());
        }

        let resp = req.send().await?;
        let mut by_id: std::collections::HashMap<String, MetricSeries> =
            std::collections::HashMap::new();
        for r in resp.metric_data_results.unwrap_or_default() {
            let id = r.id.unwrap_or_default();
            let label = id_to_label.get(&id).cloned().unwrap_or_else(|| id.clone());
            let timestamps = r.timestamps.unwrap_or_default();
            let values = r.values.unwrap_or_default();
            let mut points: Vec<(DateTime<Utc>, f64)> = timestamps
                .iter()
                .zip(values.iter())
                .filter_map(|(ts, v)| {
                    DateTime::<Utc>::from_timestamp(ts.secs(), ts.subsec_nanos()).map(|t| (t, *v))
                })
                .collect();
            points.sort_by_key(|(t, _)| *t);
            by_id.insert(id.clone(), MetricSeries { id, label, points });
        }
        // Return in the spec order so operators see the charts in the order
        // they added them.
        Ok((0..specs.len())
            .filter_map(|i| by_id.remove(&format!("m{i}")))
            .collect())
    }

    pub async fn purge_queue(&self, queue_url: &str) -> Result<()> {
        self.sqs.purge_queue().queue_url(queue_url).send().await?;
        Ok(())
    }

    pub async fn list_tags(&self, resource_arn: &str) -> Result<Vec<(String, String)>> {
        let resp = self
            .client
            .list_tags_for_resource()
            .resource_arn(resource_arn)
            .send()
            .await?;
        let tags = resp
            .resource_tags
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| match (t.key, t.value) {
                (Some(k), Some(v)) => Some((k, v)),
                _ => None,
            })
            .collect();
        Ok(tags)
    }

    /// UpdateTagsForResource — add/update tags listed in `to_add` and remove
    /// keys listed in `to_remove`. Empty lists are allowed but at least one
    /// side must be non-empty (the API rejects no-op calls).
    pub async fn update_tags(
        &self,
        resource_arn: &str,
        to_add: &[(String, String)],
        to_remove: &[String],
    ) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::Tag;
        let mut req = self
            .client
            .update_tags_for_resource()
            .resource_arn(resource_arn);
        for (k, v) in to_add {
            req = req.tags_to_add(Tag::builder().key(k).value(v).build());
        }
        for k in to_remove {
            req = req.tags_to_remove(k);
        }
        req.send().await?;
        Ok(())
    }

    pub async fn rebuild_env(&self, env_name: &str) -> Result<()> {
        self.client
            .rebuild_environment()
            .environment_name(env_name)
            .send()
            .await?;
        Ok(())
    }

    pub async fn restart_app_server(&self, env_name: &str) -> Result<()> {
        self.client
            .restart_app_server()
            .environment_name(env_name)
            .send()
            .await?;
        Ok(())
    }

    pub async fn swap_cnames(&self, source: &str, dest: &str) -> Result<()> {
        self.client
            .swap_environment_cnames()
            .source_environment_name(source)
            .destination_environment_name(dest)
            .send()
            .await?;
        Ok(())
    }

    /// Snapshot an env's current configuration as a named template under the
    /// same application. Idempotent for the user — if a template with the
    /// same name already exists, the API returns an error which we surface.
    pub async fn create_config_template(
        &self,
        application_name: &str,
        template_name: &str,
        source_env_name: &str,
    ) -> Result<()> {
        self.client
            .create_configuration_template()
            .application_name(application_name)
            .template_name(template_name)
            .environment_id(source_env_name)
            .send()
            .await
            .map_err(|e| eyre!("CreateConfigurationTemplate failed: {e}"))?;
        Ok(())
    }

    /// Delete a configuration template by name. AWS will refuse if the
    /// template is currently in use; we pass the error back unchanged.
    pub async fn delete_config_template(
        &self,
        application_name: &str,
        template_name: &str,
    ) -> Result<()> {
        self.client
            .delete_configuration_template()
            .application_name(application_name)
            .template_name(template_name)
            .send()
            .await
            .map_err(|e| eyre!("DeleteConfigurationTemplate failed: {e}"))?;
        Ok(())
    }

    /// List the newer platform versions in the same branch family as the
    /// env's current platform. Filtered server-side to `Ready` platforms;
    /// branch matching is best-effort using the current ARN's branch suffix
    /// (e.g. `Tomcat 9 with Corretto 17`). Sorted newest version first.
    pub async fn list_compatible_platforms(&self, env_name: &str) -> Result<Vec<CustomPlatform>> {
        use aws_sdk_elasticbeanstalk::types::{PlatformFilter, PlatformStatus};
        // Read the env's current platform ARN.
        let desc = self
            .client
            .describe_environments()
            .environment_names(env_name)
            .send()
            .await
            .map_err(|e| eyre!("DescribeEnvironments failed: {e}"))?;
        let env = desc
            .environments
            .unwrap_or_default()
            .into_iter()
            .next()
            .ok_or_else(|| eyre!("env '{env_name}' not found"))?;
        let current_arn = env.platform_arn.clone().unwrap_or_default();
        let stack_or_arn = env
            .solution_stack_name
            .clone()
            .unwrap_or_else(|| current_arn.clone());
        let branch = platform_branch_from(&stack_or_arn);
        let owner_filter = PlatformFilter::builder()
            .r#type("PlatformStatus")
            .operator("=")
            .values(PlatformStatus::Ready.as_str())
            .build();
        let mut filters = vec![owner_filter];
        if !branch.is_empty() {
            filters.push(
                PlatformFilter::builder()
                    .r#type("PlatformBranchName")
                    .operator("=")
                    .values(branch.clone())
                    .build(),
            );
        }
        let mut next_token: Option<String> = None;
        let mut out: Vec<CustomPlatform> = Vec::new();
        loop {
            let mut req = self.client.list_platform_versions();
            for f in &filters {
                req = req.filters(f.clone());
            }
            if let Some(t) = next_token.clone() {
                req = req.next_token(t);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| eyre!("ListPlatformVersions failed: {e}"))?;
            for p in resp.platform_summary_list.unwrap_or_default() {
                out.push(CustomPlatform {
                    arn: p.platform_arn.unwrap_or_default(),
                    branch: p.platform_branch_name.unwrap_or_default(),
                    version: p.platform_version.unwrap_or_default(),
                    status: p
                        .platform_status
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_default(),
                    lifecycle: p.platform_lifecycle_state.unwrap_or_default(),
                });
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        // Sort newest-first by semver-ish version.
        out.sort_by(|a, b| compare_versions(&b.version, &a.version));
        Ok(out)
    }

    /// Migrate the env to a new platform ARN via UpdateEnvironment. EB
    /// performs this as a rolling update; the API returns immediately and
    /// the event log carries progress.
    pub async fn upgrade_platform(&self, env_name: &str, platform_arn: &str) -> Result<()> {
        self.client
            .update_environment()
            .environment_name(env_name)
            .platform_arn(platform_arn)
            .send()
            .await
            .map_err(|e| eyre!("UpdateEnvironment(platform_arn) failed: {e}"))?;
        Ok(())
    }

    /// Clone an env: snapshot the source's settings into a transient
    /// configuration template, spin up a new env from it, then clean the
    /// template up. The new env starts the usual EB launch process — the
    /// caller can monitor via DescribeEvents.
    pub async fn clone_env(&self, source_env_name: &str, target_env_name: &str) -> Result<()> {
        // Snapshot the source env's application + ID.
        let desc = self
            .client
            .describe_environments()
            .environment_names(source_env_name)
            .send()
            .await
            .map_err(|e| eyre!("DescribeEnvironments failed: {e}"))?;
        let env = desc
            .environments
            .unwrap_or_default()
            .into_iter()
            .next()
            .ok_or_else(|| eyre!("source env '{source_env_name}' not found"))?;
        let application = env
            .application_name
            .ok_or_else(|| eyre!("source env has no application_name"))?;
        let env_id = env
            .environment_id
            .ok_or_else(|| eyre!("source env has no environment_id"))?;
        // Use a transient template name so we can clean it up even if the
        // create fails partway.
        let template = format!(
            "__ebman-clone-{}-{}",
            target_env_name,
            chrono::Utc::now().timestamp()
        );
        self.client
            .create_configuration_template()
            .application_name(&application)
            .template_name(&template)
            .environment_id(&env_id)
            .send()
            .await
            .map_err(|e| eyre!("CreateConfigurationTemplate failed: {e}"))?;
        // Best-effort cleanup even if create_environment fails — we don't
        // want to leave debris.
        let create_result = self
            .client
            .create_environment()
            .application_name(&application)
            .environment_name(target_env_name)
            .template_name(&template)
            .send()
            .await;
        let _ = self
            .client
            .delete_configuration_template()
            .application_name(&application)
            .template_name(&template)
            .send()
            .await;
        create_result.map_err(|e| eyre!("CreateEnvironment failed: {e}"))?;
        Ok(())
    }

    /// Set the env's `aws:autoscaling:asg:{MinSize,MaxSize}` so the ASG
    /// reaches `count` instances. Passing `Some(0)` is the "stop" pattern
    /// (no instances, env keeps its config). The API returns immediately;
    /// EB performs the scale as a rolling change.
    pub async fn scale_env(&self, env_name: &str, min: i32, max: i32) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::ConfigurationOptionSetting;
        let opts = vec![
            ConfigurationOptionSetting::builder()
                .namespace("aws:autoscaling:asg")
                .option_name("MinSize")
                .value(min.to_string())
                .build(),
            ConfigurationOptionSetting::builder()
                .namespace("aws:autoscaling:asg")
                .option_name("MaxSize")
                .value(max.to_string())
                .build(),
        ];
        self.client
            .update_environment()
            .environment_name(env_name)
            .set_option_settings(Some(opts))
            .send()
            .await
            .map_err(|e| eyre!("UpdateEnvironment(asg) failed: {e}"))?;
        Ok(())
    }

    /// Terminate a single EC2 instance by ID. ASG (created by EB) re-launches
    /// a replacement automatically. The API returns immediately; the
    /// instance enters `shutting-down` and EB's events panel will surface
    /// the replacement within ~30 s.
    pub async fn terminate_instance(&self, instance_id: &str) -> Result<()> {
        self.ec2
            .terminate_instances()
            .instance_ids(instance_id)
            .send()
            .await
            .map_err(|e| eyre!("ec2:TerminateInstances failed: {e}"))?;
        Ok(())
    }

    /// Stop an in-flight environment update. Useful to bail out of a hung
    /// deploy. No-op if EB sees no operation in progress.
    pub async fn abort_environment_update(&self, env_name: &str) -> Result<()> {
        self.client
            .abort_environment_update()
            .environment_name(env_name)
            .send()
            .await
            .map_err(|e| eyre!("AbortEnvironmentUpdate failed: {e}"))?;
        Ok(())
    }

    /// List custom EB platforms in this account. Filters server-side via
    /// `PlatformOwner=self` so we only show platforms the caller built, not
    /// the AWS-managed ones. Returns the ARN, platform branch name, and
    /// lifecycle state per entry.
    pub async fn list_custom_platforms(&self) -> Result<Vec<CustomPlatform>> {
        use aws_sdk_elasticbeanstalk::types::PlatformFilter;
        let filter = PlatformFilter::builder()
            .r#type("PlatformOwner")
            .operator("=")
            .values("self")
            .build();
        let mut next_token: Option<String> = None;
        let mut out: Vec<CustomPlatform> = Vec::new();
        loop {
            let mut req = self.client.list_platform_versions().filters(filter.clone());
            if let Some(t) = next_token.clone() {
                req = req.next_token(t);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| eyre!("ListPlatformVersions failed: {e}"))?;
            for p in resp.platform_summary_list.unwrap_or_default() {
                out.push(CustomPlatform {
                    arn: p.platform_arn.unwrap_or_default(),
                    branch: p.platform_branch_name.unwrap_or_default(),
                    version: p.platform_version.unwrap_or_default(),
                    status: p
                        .platform_status
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_default(),
                    lifecycle: p.platform_lifecycle_state.unwrap_or_default(),
                });
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Delete a custom platform by ARN. EB returns success immediately even
    /// though the underlying AMI / EBS cleanup runs async. Will fail if any
    /// envs are still using the platform.
    pub async fn delete_custom_platform(&self, platform_arn: &str) -> Result<()> {
        self.client
            .delete_platform_version()
            .platform_arn(platform_arn)
            .send()
            .await
            .map_err(|e| eyre!("DeletePlatformVersion failed: {e}"))?;
        Ok(())
    }

    /// List application versions for `application_name`, sorted newest-first
    /// by `date_created`. Each entry carries the version label and the
    /// optional description text shown in the EB console.
    pub async fn list_application_versions(
        &self,
        application_name: &str,
    ) -> Result<Vec<AppVersion>> {
        let resp = self
            .client
            .describe_application_versions()
            .application_name(application_name)
            .send()
            .await
            .map_err(|e| eyre!("DescribeApplicationVersions failed: {e}"))?;
        let mut out: Vec<AppVersion> = resp
            .application_versions
            .unwrap_or_default()
            .into_iter()
            .map(|v| AppVersion {
                label: v.version_label.unwrap_or_default(),
                description: v.description.unwrap_or_default(),
                created: v
                    .date_created
                    .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
            })
            .collect();
        out.sort_by_key(|v| std::cmp::Reverse(v.created));
        Ok(out)
    }

    /// Delete an application version by label. `delete_source_bundle = true`
    /// also removes the underlying `.zip` from S3 so the storage cost goes
    /// away. EB rejects the call if the version is currently deployed to any
    /// env — surfaced as `SourceBundleDeletionException` /
    /// `OperationInProgressException` in the error chain.
    pub async fn delete_application_version(
        &self,
        application_name: &str,
        version_label: &str,
        delete_source_bundle: bool,
    ) -> Result<()> {
        self.client
            .delete_application_version()
            .application_name(application_name)
            .version_label(version_label)
            .delete_source_bundle(delete_source_bundle)
            .send()
            .await
            .map_err(|e| eyre!("DeleteApplicationVersion failed: {e}"))?;
        Ok(())
    }

    /// Deploy a specific application-version label to an existing env via
    /// Ask EB for its managed S3 bucket — same bucket EB uses for its own
    /// uploads. We push application bundles into a known prefix here so
    /// `CreateApplicationVersion` can reference an `S3Location`. EB
    /// auto-creates the bucket on first call; subsequent calls return the
    /// same name.
    pub async fn create_storage_location(&self) -> Result<String> {
        let resp = self
            .client
            .create_storage_location()
            .send()
            .await
            .map_err(|e| eyre!("CreateStorageLocation failed: {e}"))?;
        resp.s3_bucket
            .ok_or_else(|| eyre!("CreateStorageLocation returned no S3Bucket"))
    }

    /// Single-shot S3 PutObject for an application bundle. The 5 GiB API
    /// ceiling covers the vast majority of EB source bundles; bundles
    /// larger than that need multipart upload, which is a follow-on.
    /// `bytes` carries the whole file; caller is responsible for reading.
    pub async fn put_application_bundle(
        &self,
        bucket: &str,
        key: &str,
        bytes: Vec<u8>,
    ) -> Result<()> {
        use aws_sdk_s3::primitives::ByteStream;
        self.s3
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(ByteStream::from(bytes))
            .send()
            .await
            .map_err(|e| eyre!("S3 PutObject {bucket}/{key} failed: {e}"))?;
        Ok(())
    }

    /// Register a new application version pointing at an S3 source bundle.
    /// `auto_create_app` is `false` because we only create versions for
    /// existing applications; the env's application is the source of truth.
    pub async fn create_app_version(
        &self,
        application_name: &str,
        version_label: &str,
        description: Option<&str>,
        s3_bucket: &str,
        s3_key: &str,
    ) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::S3Location;
        let source = S3Location::builder()
            .s3_bucket(s3_bucket)
            .s3_key(s3_key)
            .build();
        let mut req = self
            .client
            .create_application_version()
            .application_name(application_name)
            .version_label(version_label)
            .source_bundle(source)
            .auto_create_application(false);
        if let Some(d) = description {
            req = req.description(d);
        }
        req.send()
            .await
            .map_err(|e| eyre!("CreateApplicationVersion failed: {e}"))?;
        Ok(())
    }

    /// `UpdateEnvironment(version_label)`. Returns immediately — the env
    /// will mutate in the background.
    pub async fn deploy_version(&self, env_name: &str, version_label: &str) -> Result<()> {
        self.client
            .update_environment()
            .environment_name(env_name)
            .version_label(version_label)
            .send()
            .await
            .map_err(|e| eyre!("UpdateEnvironment(version_label) failed: {e}"))?;
        Ok(())
    }

    /// Fetch the option settings stored in a saved configuration template.
    /// Returns a sorted `(namespace, option_name, value)` vector — sort makes
    /// the overlay output stable and diffable across runs. Empty values are
    /// preserved (operators sometimes care that a setting is explicitly
    /// empty vs. unset; the call only returns settings the template actually
    /// defines, so "missing" already means "use platform default").
    pub async fn describe_template_settings(
        &self,
        application_name: &str,
        template_name: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let resp = self
            .client
            .describe_configuration_settings()
            .application_name(application_name)
            .template_name(template_name)
            .send()
            .await
            .map_err(|e| eyre!("DescribeConfigurationSettings(template) failed: {e}"))?;
        let mut out: Vec<(String, String, String)> = resp
            .configuration_settings
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| c.option_settings.unwrap_or_default())
            .map(|o| {
                (
                    o.namespace.unwrap_or_default(),
                    o.option_name.unwrap_or_default(),
                    o.value.unwrap_or_default(),
                )
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Apply a saved configuration template to an existing env via
    /// `UpdateEnvironment(template_name)`. The env will start mutating in
    /// the background; surface the launch via the events panel.
    pub async fn apply_config_template(&self, env_name: &str, template_name: &str) -> Result<()> {
        self.client
            .update_environment()
            .environment_name(env_name)
            .template_name(template_name)
            .send()
            .await
            .map_err(|e| eyre!("UpdateEnvironment(template_name) failed: {e}"))?;
        Ok(())
    }

    pub async fn terminate_env(&self, env_name: &str) -> Result<()> {
        self.client
            .terminate_environment()
            .environment_name(env_name)
            .send()
            .await?;
        Ok(())
    }

    /// Ask EB to start collecting the tail log for an env. Per-instance log
    /// snapshots become available via `retrieve_env_info` once each instance
    /// has uploaded its sample to S3 (usually 5-15 seconds).
    pub async fn request_env_info_tail(&self, env_name: &str) -> Result<()> {
        use aws_sdk_elasticbeanstalk::types::EnvironmentInfoType;
        self.client
            .request_environment_info()
            .environment_name(env_name)
            .info_type(EnvironmentInfoType::Tail)
            .send()
            .await
            .map_err(|e| eyre!("RequestEnvironmentInfo failed: {e}"))?;
        Ok(())
    }

    /// Read whatever tail-log samples EB has on file for the env, mapped to
    /// pre-signed S3 URLs. Empty vec means no samples have been uploaded yet —
    /// poll again. Each entry is `(ec2_instance_id, pre_signed_url)`.
    pub async fn retrieve_env_info_tail(&self, env_name: &str) -> Result<Vec<(String, String)>> {
        use aws_sdk_elasticbeanstalk::types::EnvironmentInfoType;
        let resp = self
            .client
            .retrieve_environment_info()
            .environment_name(env_name)
            .info_type(EnvironmentInfoType::Tail)
            .send()
            .await
            .map_err(|e| eyre!("RetrieveEnvironmentInfo failed: {e}"))?;
        let mut out = Vec::new();
        for info in resp.environment_info.unwrap_or_default() {
            if let (Some(id), Some(url)) = (info.ec2_instance_id, info.message) {
                out.push((id, url));
            }
        }
        Ok(out)
    }

    /// Fetch the body of a pre-signed S3 URL. Shells out to `curl` so we don't
    /// pull in an HTTP-client dep; pre-signed URLs are plain HTTPS GETs with
    /// no auth headers, which curl handles trivially. 15 s cap per fetch.
    pub async fn fetch_url_text(url: &str) -> Result<String> {
        use tokio::process::Command;
        let out = Command::new("curl")
            .args([
                "-s",
                "-S",
                "--fail-with-body",
                "--max-time",
                "15",
                "--no-buffer",
            ])
            .arg(url)
            .output()
            .await
            .map_err(|e| eyre!("could not invoke curl (is it installed?): {e}"))?;
        if !out.status.success() {
            return Err(eyre!(
                "curl exit {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn list_instances(&self, env_name: &str) -> Result<Vec<Instance>> {
        let resp = self
            .client
            .describe_instances_health()
            .environment_name(env_name)
            .attribute_names(aws_sdk_elasticbeanstalk::types::InstancesHealthAttribute::All)
            .send()
            .await?;
        let instances = resp
            .instance_health_list
            .unwrap_or_default()
            .into_iter()
            .map(|i| Instance {
                id: i.instance_id.unwrap_or_default(),
                health: i.health_status.unwrap_or_default(),
                color: i.color.unwrap_or_default(),
                causes: i.causes.unwrap_or_default(),
                instance_type: i.instance_type.unwrap_or_default(),
                availability_zone: i.availability_zone.unwrap_or_default(),
                launched_at: i
                    .launched_at
                    .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
            })
            .collect();
        Ok(instances)
    }

    pub async fn list_applications(&self) -> Result<Vec<Application>> {
        let resp = self.client.describe_applications().send().await?;
        let apps = resp
            .applications
            .unwrap_or_default()
            .into_iter()
            .map(|a| Application {
                name: a.application_name.unwrap_or_default(),
                description: a.description.unwrap_or_default(),
                date_created: a
                    .date_created
                    .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
                date_updated: a
                    .date_updated
                    .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
                version_count: a.versions.map(|v| v.len()).unwrap_or(0),
                templates: a.configuration_templates.unwrap_or_default(),
            })
            .collect();
        Ok(apps)
    }

    pub async fn list_environments(&self) -> Result<Vec<Environment>> {
        let mut all = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut req = self.client.describe_environments().include_deleted(false);
            if let Some(t) = next_token.take() {
                req = req.next_token(t);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| eyre!("DescribeEnvironments failed: {e}"))?;
            if let Some(envs) = resp.environments {
                all.extend(envs.into_iter().map(map_env));
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        Ok(all)
    }
}

fn map_env(e: aws_sdk_elasticbeanstalk::types::EnvironmentDescription) -> Environment {
    let raw_platform = e
        .solution_stack_name
        .clone()
        .or(e.platform_arn.clone())
        .unwrap_or_default();
    let tier = e
        .tier
        .as_ref()
        .and_then(|t| t.name.as_deref())
        .map(normalize_tier)
        .unwrap_or_else(|| "?".into());
    Environment {
        name: e.environment_name.unwrap_or_default(),
        application: e.application_name.unwrap_or_default(),
        status: e
            .status
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| "-".into()),
        health: e
            .health
            .map(|h| h.as_str().to_string())
            .unwrap_or_else(|| "-".into()),
        platform: platform_family(&raw_platform),
        tier,
        cname: e.cname.unwrap_or_default(),
        version_label: e.version_label.unwrap_or_default(),
        arn: e.environment_arn,
        updated: e
            .date_updated
            .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos())),
        id: e.environment_id,
        region: None,
    }
}

/// Fan-out helper: build a transient `AwsClient` for `region` (sharing the
/// caller's profile) and pull `DescribeEnvironments` from there. Each
/// returned env has `region` stamped so the table can sort / group on it.
/// Best-effort extraction of the EB platform branch name from a solution
/// stack name or platform ARN. The names look like `64bit Amazon Linux 2023
/// v4.5.2 running Tomcat 9 Corretto 17` — we keep the "running …" tail and
/// strip any leading "running " marker. ARNs follow a separate scheme and
/// already carry the branch in their path.
fn platform_branch_from(stack_or_arn: &str) -> String {
    if let Some(rest) = stack_or_arn.split(" running ").nth(1) {
        return rest.trim().to_string();
    }
    if stack_or_arn.starts_with("arn:") {
        // Branch is the second-to-last path segment.
        let parts: Vec<&str> = stack_or_arn.split('/').collect();
        if parts.len() >= 2 {
            return parts[parts.len() - 2].to_string();
        }
    }
    String::new()
}

/// Compare two dotted version strings semver-ish. Numeric tokens compared
/// numerically; non-numeric tails fall back to string comparison. Returns
/// `Ordering` so this can drive `sort_by`.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let parse = |s: &str| {
        s.split('.')
            .map(|p| p.split('-').next().unwrap_or(p).parse::<u64>().ok())
            .collect::<Vec<_>>()
    };
    let av = parse(a);
    let bv = parse(b);
    for i in 0..av.len().max(bv.len()) {
        let aa = av.get(i).and_then(|x| *x);
        let bb = bv.get(i).and_then(|x| *x);
        match (aa, bb) {
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => continue,
                o => return o,
            },
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => break,
        }
    }
    a.cmp(b)
}

pub async fn list_environments_in_region(
    profile: Option<String>,
    region: String,
) -> Result<Vec<Environment>> {
    let client = AwsClient::with(profile, Some(region.clone())).await?;
    let mut envs = client.list_environments().await?;
    for e in &mut envs {
        e.region = Some(region.clone());
    }
    Ok(envs)
}

/// Pulls the family + version out of either a solution_stack_name like
/// "64bit Amazon Linux 2 v3.7.0 running Tomcat 9 Corretto 17"  → "Tomcat 9 Corretto 17"
/// or a platform_arn like
/// "arn:aws:elasticbeanstalk:us-east-1::platform/Java 17 running on 64bit Amazon Linux 2/3.5.0"
///   → "Java 17"
fn platform_family(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    // Platform ARN form: "...platform/Family X running on 64bit Amazon Linux/3.5.0"
    // The interesting segment lives between '/' separators and contains " running on ".
    if raw.contains(" running on ") {
        for seg in raw.split('/') {
            if let Some((family, _)) = seg.split_once(" running on ") {
                return family.trim().to_string();
            }
        }
    }
    // Solution-stack form: "...64bit Amazon Linux 2 v3.5.0 running Family X"
    if let Some((_, after)) = raw.rsplit_once(" running ") {
        return after.trim().to_string();
    }
    raw.to_string()
}

/// Convention-based DLQ derivation for EB-managed worker queues. EB names the
/// main queue `awseb-<env-id>-<random>` and the DLQ `awseb-<env-id>-<random>-dlq`.
/// If the main queue URL doesn't match the pattern, returns None and the caller
/// just shows no DLQ.
fn to_smithy(d: DateTime<Utc>) -> aws_sdk_cloudwatch::primitives::DateTime {
    aws_sdk_cloudwatch::primitives::DateTime::from_secs(d.timestamp())
}

/// Pure: split a comma-separated EB option-setting value into a clean
/// `Vec<String>`. Trims each entry and drops empties.
fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn derive_dlq_url(main: &str) -> Option<String> {
    let trimmed = main.trim_end_matches('/');
    if trimmed.ends_with("-dlq") {
        return None;
    }
    Some(format!("{trimmed}-dlq"))
}

fn normalize_tier(name: &str) -> String {
    match name {
        "WebServer" => "Web".into(),
        "Worker" => "Worker".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_family_from_solution_stack() {
        assert_eq!(
            platform_family("64bit Amazon Linux 2 v3.5.0 running Java 17"),
            "Java 17"
        );
        assert_eq!(
            platform_family("64bit Amazon Linux 2 v3.7.0 running Tomcat 9 Corretto 17"),
            "Tomcat 9 Corretto 17"
        );
        assert_eq!(
            platform_family("64bit Amazon Linux 2023 v6.1.0 running Node.js 18"),
            "Node.js 18"
        );
    }

    #[test]
    fn platform_family_from_arn() {
        assert_eq!(
            platform_family(
                "arn:aws:elasticbeanstalk:us-east-1::platform/Java 17 running on 64bit Amazon Linux 2/3.5.0"
            ),
            "Java 17"
        );
    }

    #[test]
    fn platform_family_handles_empty_and_unknown() {
        assert_eq!(platform_family(""), "");
        assert_eq!(platform_family("just a string"), "just a string");
    }

    #[test]
    fn normalize_tier_maps_known_names() {
        assert_eq!(normalize_tier("WebServer"), "Web");
        assert_eq!(normalize_tier("Worker"), "Worker");
        assert_eq!(normalize_tier("Other"), "Other");
    }

    #[test]
    fn derive_dlq_url_appends_suffix() {
        assert_eq!(
            derive_dlq_url("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue"),
            Some("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue-dlq".to_string())
        );
    }

    #[test]
    fn derive_dlq_url_skips_already_dlq() {
        assert_eq!(
            derive_dlq_url("https://sqs.us-east-1.amazonaws.com/123/foo-dlq"),
            None
        );
    }

    #[test]
    fn derive_dlq_url_strips_trailing_slash() {
        assert_eq!(
            derive_dlq_url("https://sqs.us-east-1.amazonaws.com/123/foo/"),
            Some("https://sqs.us-east-1.amazonaws.com/123/foo-dlq".to_string())
        );
    }

    // ─── Mocked-AWS integration tests ─────────────────────────────────────
    //
    // These exercise the SDK code paths against `aws-smithy-mocks` so we
    // can lock down past regressions and run without an AWS account. Each
    // test names the specific bug it pins to keep the intent crisp when
    // a future change "breaks" it.

    use aws_smithy_mocks::{mock, mock_client};

    /// Build a minimal `AwsClient` where only one sub-client is mocked and
    /// the rest are plain SDK defaults (which will fail loudly if any
    /// unmocked code path is reached — exactly the signal we want).
    fn client_with_eb(eb: Client) -> AwsClient {
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        AwsClient::for_tests(
            eb,
            SqsClient::new(&cfg),
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            Ec2Client::new(&cfg),
        )
    }

    fn client_with_sqs(sqs: SqsClient) -> AwsClient {
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        AwsClient::for_tests(
            Client::new(&cfg),
            sqs,
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            Ec2Client::new(&cfg),
        )
    }

    // ── Regression #1 ────────────────────────────────────────────────────
    // `DescribeConfigurationSettings` returns `WorkerQueueURL = ""` when
    // EB autocreates the queue (the operator didn't override it). The
    // original code looked only at option settings and would show "no
    // queue" for the most common worker-tier shape. The fix queries
    // `DescribeEnvironmentResources` first and only falls back to option
    // settings when explicit overrides exist.

    #[tokio::test]
    async fn worker_queues_resolves_via_describe_environment_resources_when_autocreated() {
        use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};

        let der = mock!(Client::describe_environment_resources).then_output(|| {
            DescribeEnvironmentResourcesOutput::builder()
                .environment_resources(
                    EnvironmentResourceDescription::builder()
                        .queues(
                            Queue::builder()
                                .name("WorkerQueue")
                                .url("https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue")
                                .build(),
                        )
                        .queues(
                            Queue::builder()
                                .name("WorkerDeadLetterQueue")
                                .url(
                                    "https://sqs.us-east-1.amazonaws.com/123/awseb-e-foo-queue-dlq",
                                )
                                .build(),
                        )
                        .build(),
                )
                .build()
        });
        // Provide an empty configuration-settings response — that's the
        // exact failure mode the bug fix is defending against.
        let dcs = mock!(Client::describe_configuration_settings).then_output(|| {
            aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder()
                .build()
        });
        let eb = mock_client!(aws_sdk_elasticbeanstalk, [&der, &dcs]);
        let client = client_with_eb(eb);

        // We can't actually fetch SQS stats without mocking SQS too, but
        // the URL resolution is the bit that regressed — assert by
        // calling the option-settings-only path that drives the same
        // logic without the stats round-trip.
        // describe_worker_queues calls queue_stats which would fail
        // against the default sqs client. Use a try-await dance to
        // observe at least the call shape via the mock's call counter.
        let _ = client.describe_worker_queues("eb-app", "eb-env").await;
        assert_eq!(
            der.num_calls(),
            1,
            "describe_environment_resources should be the primary path"
        );
    }

    // ── Regression #2 ────────────────────────────────────────────────────
    // `peek_messages` originally made a single `ReceiveMessage` call —
    // but SQS may return fewer than the requested batch on any one call
    // (it's a maximum, not a guarantee). The fix loops with short long-
    // polling, dedupes by message id across iterations, and bails after
    // two empty batches in a row.

    #[tokio::test]
    async fn peek_messages_loops_and_dedupes_across_batches() {
        use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
        use aws_sdk_sqs::types::Message;

        // First call returns 2 messages, second call returns 1 (including
        // a duplicate of msg-1), third returns empty, fourth returns
        // empty → loop should exit. Expect 3 unique messages.
        fn msg(id: &'static str) -> Message {
            Message::builder().message_id(id).body(id).build()
        }
        let rule = mock!(aws_sdk_sqs::Client::receive_message)
            .sequence()
            .output(|| {
                ReceiveMessageOutput::builder()
                    .messages(msg("msg-1"))
                    .messages(msg("msg-2"))
                    .build()
            })
            .output(|| {
                ReceiveMessageOutput::builder()
                    .messages(msg("msg-1")) // dup
                    .messages(msg("msg-3"))
                    .build()
            })
            .output(|| ReceiveMessageOutput::builder().build())
            .output(|| ReceiveMessageOutput::builder().build())
            .build();
        let sqs = mock_client!(aws_sdk_sqs, [&rule]);
        let client = client_with_sqs(sqs);

        let out = client
            .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 10)
            .await
            .expect("peek should succeed");
        let ids: Vec<String> = out.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, vec!["msg-1", "msg-2", "msg-3"]);
    }

    #[tokio::test]
    async fn peek_messages_stops_after_two_empty_batches() {
        use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
        // Sequence returns empty twice — should stop without exhausting
        // the call cap.
        let rule = mock!(aws_sdk_sqs::Client::receive_message)
            .sequence()
            .output(|| ReceiveMessageOutput::builder().build())
            .output(|| ReceiveMessageOutput::builder().build())
            // If we reach this, the stop-on-two-empty guard is broken.
            .output(|| {
                ReceiveMessageOutput::builder()
                    .messages(
                        aws_sdk_sqs::types::Message::builder()
                            .message_id("late")
                            .body("late")
                            .build(),
                    )
                    .build()
            })
            .build();
        let sqs = mock_client!(aws_sdk_sqs, [&rule]);
        let client = client_with_sqs(sqs);

        let out = client
            .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 10)
            .await
            .expect("peek should succeed");
        assert!(
            out.is_empty(),
            "should have stopped before consuming the 'late' message"
        );
        assert_eq!(
            rule.num_calls(),
            2,
            "exactly two empty-batch calls should terminate the loop"
        );
    }

    // ── Happy-path coverage ──────────────────────────────────────────────
    // Lock down the most-used path so refactors of `list_environments`
    // don't silently break the table-rendering surface.

    #[tokio::test]
    async fn list_environments_maps_describe_environments_to_env_rows() {
        use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
        use aws_sdk_elasticbeanstalk::types::{EnvironmentDescription, EnvironmentTier};

        let de = mock!(Client::describe_environments).then_output(|| {
            DescribeEnvironmentsOutput::builder()
                .environments(
                    EnvironmentDescription::builder()
                        .environment_name("api-prod")
                        .application_name("api")
                        .status("Ready".into())
                        .health("Green".into())
                        .cname("api-prod.eba.amazonaws.com")
                        .version_label("build-42")
                        .solution_stack_name("64bit Amazon Linux 2 v3.5.0 running Java 17")
                        .tier(EnvironmentTier::builder().name("WebServer").build())
                        .build(),
                )
                .build()
        });
        let eb = mock_client!(aws_sdk_elasticbeanstalk, [&de]);
        let client = client_with_eb(eb);

        let envs = client.list_environments().await.expect("ok");
        assert_eq!(envs.len(), 1);
        let e = &envs[0];
        assert_eq!(e.name, "api-prod");
        assert_eq!(e.application, "api");
        assert_eq!(e.tier, "Web", "tier normalises WebServer → Web");
        assert_eq!(e.platform, "Java 17");
        assert_eq!(e.version_label, "build-42");
    }

    // ── MultiSelect picker plumbing ─────────────────────────────────────
    //
    // `:subnets` / `:security-groups` rely on three helpers that all
    // need to round-trip cleanly: VPC discovery via option settings,
    // EC2 inventory listing filtered by VPC, and the comma-split
    // helper that converts EB's CSV format to a clean Vec<String>.

    #[test]
    fn split_csv_trims_and_drops_empties() {
        assert_eq!(
            split_csv("subnet-a,subnet-b, subnet-c, ,subnet-d"),
            vec!["subnet-a", "subnet-b", "subnet-c", "subnet-d"]
        );
        assert!(split_csv("").is_empty());
        assert!(split_csv(",,,").is_empty());
    }

    #[tokio::test]
    async fn fetch_env_vpc_context_pulls_vpc_id_subnets_and_sgs() {
        use aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput;
        use aws_sdk_elasticbeanstalk::types::{
            ConfigurationOptionSetting, ConfigurationSettingsDescription,
        };

        let dcs = mock!(Client::describe_configuration_settings).then_output(|| {
            DescribeConfigurationSettingsOutput::builder()
                .configuration_settings(
                    ConfigurationSettingsDescription::builder()
                        .option_settings(
                            ConfigurationOptionSetting::builder()
                                .namespace("aws:ec2:vpc")
                                .option_name("VPCId")
                                .value("vpc-123")
                                .build(),
                        )
                        .option_settings(
                            ConfigurationOptionSetting::builder()
                                .namespace("aws:ec2:vpc")
                                .option_name("Subnets")
                                .value("subnet-a,subnet-b")
                                .build(),
                        )
                        .option_settings(
                            ConfigurationOptionSetting::builder()
                                .namespace("aws:ec2:vpc")
                                .option_name("ELBSubnets")
                                .value("subnet-x,subnet-y")
                                .build(),
                        )
                        .option_settings(
                            ConfigurationOptionSetting::builder()
                                .namespace("aws:autoscaling:launchconfiguration")
                                .option_name("SecurityGroups")
                                .value("sg-1,sg-2,sg-3")
                                .build(),
                        )
                        // Noise — should be ignored.
                        .option_settings(
                            ConfigurationOptionSetting::builder()
                                .namespace("aws:elasticbeanstalk:application:environment")
                                .option_name("LOG_LEVEL")
                                .value("debug")
                                .build(),
                        )
                        .build(),
                )
                .build()
        });
        let eb = mock_client!(aws_sdk_elasticbeanstalk, [&dcs]);
        let client = client_with_eb(eb);

        let ctx = client
            .fetch_env_vpc_context("api", "api-prod")
            .await
            .expect("ok");
        assert_eq!(ctx.vpc_id.as_deref(), Some("vpc-123"));
        assert_eq!(ctx.subnets, vec!["subnet-a", "subnet-b"]);
        assert_eq!(ctx.elb_subnets, vec!["subnet-x", "subnet-y"]);
        assert_eq!(ctx.security_groups, vec!["sg-1", "sg-2", "sg-3"]);
    }

    #[tokio::test]
    async fn list_subnets_in_vpc_filters_orders_and_extracts_name_tag() {
        use aws_sdk_ec2::operation::describe_subnets::DescribeSubnetsOutput;
        use aws_sdk_ec2::types::{Subnet, Tag};

        let ds = mock!(aws_sdk_ec2::Client::describe_subnets).then_output(|| {
            DescribeSubnetsOutput::builder()
                .subnets(
                    Subnet::builder()
                        .subnet_id("subnet-2b")
                        .availability_zone("us-east-1b")
                        .cidr_block("10.0.2.0/24")
                        .tags(Tag::builder().key("Name").value("private-2b").build())
                        .build(),
                )
                .subnets(
                    Subnet::builder()
                        .subnet_id("subnet-1a")
                        .availability_zone("us-east-1a")
                        .cidr_block("10.0.1.0/24")
                        .build(),
                )
                .subnets(
                    Subnet::builder()
                        .subnet_id("subnet-1a-overlap")
                        .availability_zone("us-east-1a")
                        .cidr_block("10.0.0.0/24")
                        .build(),
                )
                .build()
        });
        let ec2 = mock_client!(aws_sdk_ec2, [&ds]);
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let client = AwsClient::for_tests(
            Client::new(&cfg),
            SqsClient::new(&cfg),
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            ec2,
        );

        let subnets = client.list_subnets_in_vpc("vpc-abc").await.expect("ok");
        // Ordered by AZ then CIDR — subnet-1a-overlap (10.0.0.0/24) precedes
        // subnet-1a (10.0.1.0/24), then subnet-2b.
        let ids: Vec<&str> = subnets.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["subnet-1a-overlap", "subnet-1a", "subnet-2b"]);
        // Name tag extracted when present, None when absent.
        assert_eq!(subnets[2].name_tag.as_deref(), Some("private-2b"));
        assert!(subnets[1].name_tag.is_none());
    }

    // ── Write-path coverage ──────────────────────────────────────────────
    //
    // `update_env_option_settings` is the load-bearing write path —
    // every `:capacity`, `:env`, `:tag`, `:subnets`, `:set-option`, etc.
    // ultimately funnels through it. Pin the request-shape contract and
    // the empty-input guard.

    #[tokio::test]
    async fn update_env_option_settings_builds_correct_request_shape() {
        use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentOutput;
        // `match_requests` runs the closure against every captured
        // request; returning false means "no rule matched" and the
        // SDK call returns an error, which the test would then trip on.
        // So an assertion-style predicate doubles as the test body.
        let rule = mock!(Client::update_environment)
            .match_requests(|input| {
                if input.environment_name.as_deref() != Some("api-prod") {
                    return false;
                }
                let options = input.option_settings();
                if options.len() != 2 {
                    return false;
                }
                // Order is preserved from the caller's slice.
                if options[0].namespace.as_deref() != Some("aws:autoscaling:asg")
                    || options[0].option_name.as_deref() != Some("MinSize")
                    || options[0].value.as_deref() != Some("2")
                {
                    return false;
                }
                if options[1].namespace.as_deref() != Some("aws:autoscaling:launchconfiguration")
                    || options[1].option_name.as_deref() != Some("InstanceType")
                    || options[1].value.as_deref() != Some("t3.medium")
                {
                    return false;
                }
                let removes = input.options_to_remove();
                if removes.len() != 1 {
                    return false;
                }
                removes[0].namespace.as_deref()
                    == Some("aws:elasticbeanstalk:application:environment")
                    && removes[0].option_name.as_deref() == Some("OLD_VAR")
            })
            .then_output(|| UpdateEnvironmentOutput::builder().build());
        let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
        let client = client_with_eb(eb);

        let to_set = vec![
            (
                "aws:autoscaling:asg".to_string(),
                "MinSize".to_string(),
                "2".to_string(),
            ),
            (
                "aws:autoscaling:launchconfiguration".to_string(),
                "InstanceType".to_string(),
                "t3.medium".to_string(),
            ),
        ];
        let to_remove = vec![(
            "aws:elasticbeanstalk:application:environment".to_string(),
            "OLD_VAR".to_string(),
        )];
        client
            .update_env_option_settings("api-prod", &to_set, &to_remove)
            .await
            .expect("expected request shape to match");
        assert_eq!(rule.num_calls(), 1);
    }

    #[tokio::test]
    async fn update_env_option_settings_rejects_empty_input_before_dispatch() {
        // If the guard fails we'd reach the mocked client, which has no
        // rules — that would also error, but with a different message.
        // The empty-input branch must short-circuit *before* any SDK call.
        use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentOutput;
        let trip = mock!(Client::update_environment)
            .then_output(|| UpdateEnvironmentOutput::builder().build());
        let eb = mock_client!(aws_sdk_elasticbeanstalk, [&trip]);
        let client = client_with_eb(eb);

        let err = client
            .update_env_option_settings("api-prod", &[], &[])
            .await
            .expect_err("expected guard to fire");
        assert!(
            err.to_string().contains("nothing to do"),
            "expected nothing-to-do guard, got {err}"
        );
        assert_eq!(
            trip.num_calls(),
            0,
            "guard should short-circuit before any SDK call"
        );
    }

    #[tokio::test]
    async fn update_env_option_settings_surfaces_aws_errors() {
        use aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentError;
        use aws_sdk_elasticbeanstalk::types::error::InsufficientPrivilegesException;
        let err_rule = mock!(Client::update_environment).then_error(|| {
            UpdateEnvironmentError::InsufficientPrivilegesException(
                InsufficientPrivilegesException::builder()
                    .message("not authorized to call UpdateEnvironment")
                    .build(),
            )
        });
        let eb = mock_client!(aws_sdk_elasticbeanstalk, [&err_rule]);
        let client = client_with_eb(eb);

        let err = client
            .update_env_option_settings(
                "api-prod",
                &[("aws:autoscaling:asg".into(), "MinSize".into(), "2".into())],
                &[],
            )
            .await
            .expect_err("expected AWS error to propagate");
        // The flatten wraps the SDK error string; we just confirm the
        // contextual prefix is present so logs are actionable.
        assert!(
            err.to_string()
                .contains("UpdateEnvironment(option_settings)"),
            "expected wrapped error context, got {err}"
        );
    }

    #[tokio::test]
    async fn list_security_groups_in_vpc_orders_by_name() {
        use aws_sdk_ec2::operation::describe_security_groups::DescribeSecurityGroupsOutput;
        use aws_sdk_ec2::types::SecurityGroup;

        let dsg = mock!(aws_sdk_ec2::Client::describe_security_groups).then_output(|| {
            DescribeSecurityGroupsOutput::builder()
                .security_groups(
                    SecurityGroup::builder()
                        .group_id("sg-z")
                        .group_name("zeta")
                        .description("z group")
                        .build(),
                )
                .security_groups(
                    SecurityGroup::builder()
                        .group_id("sg-a")
                        .group_name("alpha")
                        .description("a group")
                        .build(),
                )
                .build()
        });
        let ec2 = mock_client!(aws_sdk_ec2, [&dsg]);
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let client = AwsClient::for_tests(
            Client::new(&cfg),
            SqsClient::new(&cfg),
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            ec2,
        );

        let sgs = client
            .list_security_groups_in_vpc("vpc-abc")
            .await
            .expect("ok");
        assert_eq!(sgs.len(), 2);
        assert_eq!(sgs[0].group_name, "alpha");
        assert_eq!(sgs[1].group_name, "zeta");
    }

    // ── Error-path coverage for the load-bearing read methods ────────────
    //
    // Each of these mocks the SDK to return a typed error and asserts
    // our wrapper preserves the operation-name context. Future
    // refactors of these methods will trip a test if they accidentally
    // drop the `.map_err(|e| eyre!(...))?` prefix and start propagating
    // bare SDK errors.

    #[tokio::test]
    async fn list_environments_surfaces_aws_errors_with_op_context() {
        use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsError;
        let rule = mock!(Client::describe_environments).then_error(|| {
            DescribeEnvironmentsError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("InternalServerError")
                    .message("retry later")
                    .build(),
            )
        });
        let eb = mock_client!(aws_sdk_elasticbeanstalk, [&rule]);
        let client = client_with_eb(eb);

        let err = client
            .list_environments()
            .await
            .expect_err("expected AWS error to propagate");
        assert!(
            err.to_string().contains("DescribeEnvironments"),
            "expected operation context, got {err}"
        );
    }

    #[tokio::test]
    async fn peek_messages_surfaces_sqs_errors_with_op_context() {
        use aws_sdk_sqs::operation::receive_message::ReceiveMessageError;
        let rule = mock!(aws_sdk_sqs::Client::receive_message).then_error(|| {
            ReceiveMessageError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("QueueDoesNotExist")
                    .message("queue gone")
                    .build(),
            )
        });
        let sqs = mock_client!(aws_sdk_sqs, [&rule]);
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let client = AwsClient::for_tests(
            Client::new(&cfg),
            sqs,
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            Ec2Client::new(&cfg),
        );

        let err = client
            .peek_messages("https://sqs.us-east-1.amazonaws.com/123/q", 5)
            .await
            .expect_err("expected SQS error to propagate");
        assert!(
            err.to_string().contains("ReceiveMessage"),
            "expected operation context, got {err}"
        );
    }

    #[tokio::test]
    async fn list_subnets_in_vpc_surfaces_ec2_errors_with_op_context() {
        use aws_sdk_ec2::operation::describe_subnets::DescribeSubnetsError;
        let rule = mock!(aws_sdk_ec2::Client::describe_subnets).then_error(|| {
            DescribeSubnetsError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("InvalidVpcID.NotFound")
                    .message("vpc-xxx not found")
                    .build(),
            )
        });
        let ec2 = mock_client!(aws_sdk_ec2, [&rule]);
        let cfg = aws_config::SdkConfig::builder()
            .region(Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let client = AwsClient::for_tests(
            Client::new(&cfg),
            SqsClient::new(&cfg),
            CwClient::new(&cfg),
            CwLogsClient::new(&cfg),
            S3Client::new(&cfg),
            ec2,
        );

        let err = client
            .list_subnets_in_vpc("vpc-xxx")
            .await
            .expect_err("expected EC2 error to propagate");
        assert!(
            err.to_string().contains("DescribeSubnets"),
            "expected operation context, got {err}"
        );
    }
}
