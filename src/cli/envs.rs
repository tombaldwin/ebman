//! `ebman envs [--json]` — list environments in the current profile
//! / region. The simplest CLI subcommand; ships JSON output too so
//! CI scripts can `jq '.[] | select(.health=="Red")'` etc.

use color_eyre::eyre::Result;

use crate::aws;
use crate::cli::cli_esc;

/// The flat, stable `envs --json` schema — shared verbatim by the
/// MCP `list_environments` tool so agent-facing and script-facing
/// shapes can't drift apart.
pub(crate) fn render_envs_json(envs: &[aws::Environment]) -> String {
    let entries: Vec<String> = envs
        .iter()
        .map(|e| {
            // `tier`, `updated` and `region` were on the record all
            // along and dropped on the way out, which cost
            // a real incident real minutes: a consumer could not tell a
            // worker from a web env except by guessing from the name, and
            // had no timestamp to tell a condition that started sixteen
            // hours ago from one that started in the last hour.
            //
            // `updated` is EB's `DateUpdated` — the environment's last
            // change, NOT a health-since. It is the more useful of the
            // two anyway when a managed platform update is what moved
            // the health: it timestamps the update itself.
            let updated = match &e.updated {
                Some(t) => format!("\"{}\"", cli_esc(&t.to_rfc3339())),
                None => "null".to_string(),
            };
            let region = match &e.region {
                Some(r) => format!("\"{}\"", cli_esc(r)),
                None => "null".to_string(),
            };
            format!(
                "{{\"name\":\"{}\",\"application\":\"{}\",\"tier\":\"{}\",\"status\":\"{}\",\"health\":\"{}\",\"platform\":\"{}\",\"cname\":\"{}\",\"version_label\":\"{}\",\"updated\":{},\"region\":{}}}",
                cli_esc(&e.name),
                cli_esc(&e.application),
                cli_esc(&e.tier),
                cli_esc(&e.status),
                cli_esc(&e.health),
                cli_esc(&e.platform),
                cli_esc(&e.cname),
                cli_esc(&e.version_label),
                updated,
                region,
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

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
    if json {
        println!("{}", render_envs_json(&envs));
    } else {
        println!("NAME\tAPPLICATION\tSTATUS\tHEALTH\tPLATFORM\tCNAME\tVERSION");
        for e in &envs {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                e.name, e.application, e.status, e.health, e.platform, e.cname, e.version_label
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod envs_json_tests {
    use super::render_envs_json;
    use crate::aws::Environment;

    fn env() -> Environment {
        Environment {
            name: "Uflexi-prod-wk".into(),
            application: "uflexi".into(),
            status: "Ready".into(),
            health: "Yellow".into(),
            platform: "Java 17".into(),
            solution_stack: String::new(),
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
        let json = render_envs_json(&[env()]);
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
            "\"name\":\"Uflexi-prod-wk\"",
            "\"application\":\"uflexi\"",
            "\"health\":\"Yellow\"",
            "\"version_label\":\"build-900\"",
        ] {
            assert!(
                json.contains(old),
                "{old} must not have been dropped: {json}"
            );
        }
    }

    /// Absent optionals must be JSON `null`, not the string "None" and
    /// not an empty string that reads as a real value.
    #[test]
    fn absent_optionals_are_null() {
        let mut e = env();
        e.updated = None;
        e.region = None;
        let json = render_envs_json(&[e]);
        assert!(json.contains("\"updated\":null"), "{json}");
        assert!(json.contains("\"region\":null"), "{json}");
        assert!(
            !json.contains("None"),
            "a Rust Option must never be rendered by Debug into JSON: {json}"
        );
    }
}
