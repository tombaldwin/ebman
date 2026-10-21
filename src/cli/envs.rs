//! `ebman envs [--json]` — list environments in the current profile
//! / region. The simplest CLI subcommand; ships JSON output too so
//! CI scripts can `jq '.[] | select(.health=="Red")'` etc.

use color_eyre::eyre::Result;

use crate::aws;

/// Firing CloudWatch alarms by env name. An env with none has no entry.
pub(crate) type FiringByEnv = std::collections::HashMap<String, Vec<String>>;

/// Assign the region's `firing` alarms to `envs` by the rule every
/// surface uses (`alarm_dimensions`, `app.ENV.alarm_prefix`), with
/// `envs` as the fleet a prefix gives way to.
pub(crate) fn firing_by_env(
    envs: &[aws::Environment],
    firing: &[aws::FiringAlarm],
    dimension_names: &[String],
    app_tabs: &crate::app_tab::AppTabs,
) -> FiringByEnv {
    envs.iter()
        .filter_map(|e| {
            let matching = crate::alarms::AlarmMatch::for_env(
                dimension_names,
                app_tabs,
                &e.name,
                envs.iter().map(|o| o.name.as_str()),
            );
            let names = crate::alarms::firing_for(&matching, &e.name, firing);
            (!names.is_empty()).then(|| (e.name.clone(), names))
        })
        .collect()
}

/// The firing alarms for `envs`, all in `client`'s region. `Err` is a
/// failed check, which callers must show as unknown, never as none.
pub(crate) async fn fetch_firing_by_env(
    client: &aws::AwsClient,
    envs: &[aws::Environment],
    dimension_names: &[String],
    app_tabs: &crate::app_tab::AppTabs,
) -> Result<FiringByEnv, String> {
    let firing = client
        .list_firing_alarms()
        .await
        .map_err(|e| format!("DescribeAlarms: {e}"))?;
    Ok(firing_by_env(envs, &firing, dimension_names, app_tabs))
}

/// The flat, stable `envs --json` schema — shared verbatim by the
/// MCP `list_environments` tool so agent-facing and script-facing
/// shapes can't drift apart.
///
/// `firing` is `None` when the alarm check failed: every env's
/// `firing_alarms` is then `null`, where `[]` would claim it is quiet.
pub(crate) fn render_envs_json(envs: &[aws::Environment], firing: Option<&FiringByEnv>) -> String {
    let rows: Vec<EnvRow<'_>> = envs.iter().map(|e| EnvRow::of(e, firing)).collect();
    // Serialising borrowed strings and options cannot fail.
    serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
}

/// One env in the `envs --json` / `list_environments` schema, in its
/// field order.
#[derive(serde::Serialize)]
struct EnvRow<'a> {
    name: &'a str,
    application: &'a str,
    tier: &'a str,
    status: &'a str,
    health: &'a str,
    platform: &'a str,
    platform_version: &'a str,
    cname: &'a str,
    version_label: &'a str,
    // `tier`, `updated` and `region` were on the record all along and
    // dropped on the way out, which cost a real incident real minutes: a
    // consumer could not tell a worker from a web env except by guessing
    // from the name, and had no timestamp to tell a condition that
    // started sixteen hours ago from one that started in the last hour.
    //
    // `updated` is EB's `DateUpdated` — the environment's last change,
    // NOT a health-since. It is the more useful of the two anyway when a
    // managed platform update is what moved the health: it timestamps
    // the update itself.
    updated: Option<String>,
    region: Option<&'a str>,
    /// `None` (JSON `null`) when the alarm check failed: `[]` would
    /// claim the env is quiet. The type says so; the hand-built string
    /// this replaced had to be told.
    firing_alarms: Option<Vec<&'a str>>,
}

impl<'a> EnvRow<'a> {
    fn of(e: &'a aws::Environment, firing: Option<&'a FiringByEnv>) -> Self {
        Self {
            name: &e.name,
            application: &e.application,
            tier: &e.tier,
            status: &e.status,
            health: &e.health,
            platform: &e.platform,
            platform_version: &e.platform_version,
            cname: &e.cname,
            version_label: &e.version_label,
            updated: e.updated.map(|t| t.to_rfc3339()),
            region: e.region.as_deref(),
            firing_alarms: firing.map(|map| {
                map.get(&e.name)
                    .map(|names| names.iter().map(String::as_str).collect())
                    .unwrap_or_default()
            }),
        }
    }
}

/// `ebman envs` — list environments, optionally as JSON.
///
/// Exit 2 on a usage error.
pub async fn run(args: &[String]) -> Result<()> {
    let mut json = false;
    // Reject unknown flags like every other subcommand — `--jsn`
    // used to silently print the text table, exit 0.
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--json" => json = true,
            other => {
                eprintln!("ebman envs: unknown flag '{other}' (usage: ebman envs [--json])");
                std::process::exit(2);
            }
        }
    }
    let aws = aws::AwsClient::with(None, None).await?;
    let envs = aws
        .list_environments()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_environments: {e}"))?;
    let cfg = crate::config::load();
    let firing = match fetch_firing_by_env(&aws, &envs, &cfg.alarm_dimensions, &cfg.app_tabs).await
    {
        Ok(f) => Some(f),
        // The listing stands; the alarms are unknown, and say so.
        Err(e) => {
            eprintln!("warning: could not check for firing alarms ({e}); shown as unknown");
            None
        }
    };
    if json {
        println!("{}", render_envs_json(&envs, firing.as_ref()));
    } else {
        println!("NAME\tAPPLICATION\tSTATUS\tHEALTH\tPLATFORM\tCNAME\tVERSION\tALARMS");
        for e in &envs {
            println!("{}", envs_text_row(e, firing.as_ref()));
        }
    }
    Ok(())
}

/// One tab-separated row of the `ebman envs` text table. PLATFORM
/// carries the platform version, as the TUI column does; ALARMS names
/// the firing alarms, `-` for none and `?` when the check failed. The
/// names are comma-joined for reading, and an alarm name may itself
/// contain a comma: a script splits `--json`'s `firing_alarms` instead.
pub(crate) fn envs_text_row(e: &aws::Environment, firing: Option<&FiringByEnv>) -> String {
    let platform = if e.platform_version.is_empty() {
        e.platform.clone()
    } else {
        format!("{} {}", e.platform, e.platform_version)
    };
    let alarms = match firing {
        None => "?".to_string(),
        Some(map) => map
            .get(&e.name)
            .map(|names| names.join(","))
            .unwrap_or_else(|| "-".to_string()),
    };
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        e.name, e.application, e.status, e.health, platform, e.cname, e.version_label, alarms
    )
}

#[cfg(test)]
mod envs_json_tests {
    use super::*;
    use crate::aws::Environment;

    fn env() -> Environment {
        Environment {
            name: "poly-prod-wk".into(),
            application: "poly".into(),
            status: "Ready".into(),
            health: "Yellow".into(),
            platform: "Java 17".into(),
            solution_stack: String::new(),
            platform_version: String::new(),
            tier: "Worker".into(),
            cname: String::new(),
            version_label: "build-900".into(),
            arn: None,
            updated: chrono::DateTime::parse_from_rfc3339("2026-09-17T06:04:50+00:00")
                .ok()
                .map(|d| d.with_timezone(&chrono::Utc)),
            id: None,
            region: Some("us-west-1".into()),
        }
    }

    /// `tier` and `updated` were on the record and dropped on the way
    /// out, and both cost a real incident real minutes: a consumer could
    /// not tell a worker from a web env except by guessing at the name,
    /// and had no timestamp to tell a sixteen-hour-old condition from a
    /// live one.
    #[test]
    fn the_env_json_carries_tier_and_the_update_timestamp() {
        let json = render_envs_json(&[env()], Some(&FiringByEnv::new()));
        assert!(
            json.contains("\"tier\":\"Worker\""),
            "a worker env must say so rather than leaving it to the name: {json}"
        );
        assert!(
            json.contains("\"updated\":\"2026-09-17T06:04:50+00:00\""),
            "the update timestamp reframes the whole triage: {json}"
        );
        assert!(json.contains("\"region\":\"us-west-1\""), "{json}");
        // The pre-existing fields must survive — this is a documented
        // shape shared with `ebman envs --json`.
        for old in [
            "\"name\":\"poly-prod-wk\"",
            "\"application\":\"poly\"",
            "\"health\":\"Yellow\"",
            "\"version_label\":\"build-900\"",
        ] {
            assert!(
                json.contains(old),
                "{old} must not have been dropped: {json}"
            );
        }
    }

    #[test]
    fn the_env_json_carries_platform_version_and_firing_alarms() {
        let mut e = env();
        e.platform_version = "4.1.0".into();
        let firing = FiringByEnv::from([("poly-prod-wk".to_string(), vec!["a\"b".to_string()])]);
        let json = render_envs_json(&[e.clone()], Some(&firing));
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v[0]["platform_version"], "4.1.0");
        assert_eq!(v[0]["firing_alarms"], serde_json::json!(["a\"b"]));
        // Quiet is `[]`; a failed check is `null`, not `[]`.
        let v: serde_json::Value =
            serde_json::from_str(&render_envs_json(&[e.clone()], Some(&FiringByEnv::new())))
                .unwrap();
        assert_eq!(v[0]["firing_alarms"], serde_json::json!([]));
        let v: serde_json::Value = serde_json::from_str(&render_envs_json(&[e], None)).unwrap();
        assert!(v[0]["firing_alarms"].is_null());
    }

    #[test]
    fn the_text_row_carries_platform_version_and_alarms() {
        let mut e = env();
        e.platform_version = "4.1.0".into();
        let firing =
            FiringByEnv::from([("poly-prod-wk".to_string(), vec!["x".into(), "y".into()])]);
        let row = envs_text_row(&e, Some(&firing));
        let cols: Vec<&str> = row.split('\t').collect();
        assert_eq!(cols[4], "Java 17 4.1.0");
        assert_eq!(cols[7], "x,y");
        assert_eq!(
            envs_text_row(&e, Some(&FiringByEnv::new()))
                .split('\t')
                .nth(7),
            Some("-")
        );
        assert_eq!(envs_text_row(&e, None).split('\t').nth(7), Some("?"));
        e.platform_version.clear();
        assert_eq!(envs_text_row(&e, None).split('\t').nth(4), Some("Java 17"));
    }

    #[test]
    fn firing_by_env_keeps_a_longer_named_envs_alarms_out_of_a_prefix() {
        let mk = |name: &str| Environment {
            name: name.into(),
            ..env()
        };
        let envs = [mk("shop-prod"), mk("shop-prod-wk")];
        let mut tabs = crate::app_tab::AppTabs::default();
        tabs.envs
            .entry("shop-prod".into())
            .or_default()
            .alarm_prefix = Some("shop-prod-".into());
        let alarm = |name: &str, dim: (&str, &str)| crate::aws::FiringAlarm {
            name: name.into(),
            dimensions: vec![(dim.0.into(), dim.1.into())],
        };
        let firing = [
            alarm("shop-prod-db", ("DBInstanceIdentifier", "db")),
            alarm("shop-prod-wk-age", ("QueueName", "q")),
            alarm("wk-dim", ("EnvironmentName", "shop-prod-wk")),
        ];
        let got = firing_by_env(&envs, &firing, &["EnvironmentName".into()], &tabs);
        assert_eq!(
            got.get("shop-prod"),
            Some(&vec!["shop-prod-db".to_string()])
        );
        assert_eq!(got.get("shop-prod-wk"), Some(&vec!["wk-dim".to_string()]));
    }

    /// Absent optionals must be JSON `null`, not the string "None" and
    /// not an empty string that reads as a real value.
    #[test]
    fn absent_optionals_are_null() {
        let mut e = env();
        e.updated = None;
        e.region = None;
        let json = render_envs_json(&[e], Some(&FiringByEnv::new()));
        assert!(json.contains("\"updated\":null"), "{json}");
        assert!(json.contains("\"region\":null"), "{json}");
        assert!(
            !json.contains("None"),
            "a Rust Option must never be rendered by Debug into JSON: {json}"
        );
    }
}
