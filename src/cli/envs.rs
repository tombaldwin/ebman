//! `ebman envs [--json]` — list environments in the current profile
//! / region. The simplest CLI subcommand; ships JSON output too so
//! CI scripts can `jq '.[] | select(.health=="Red")'` etc.

use color_eyre::eyre::Result;

use crate::aws;
use crate::cli::cli_esc;

pub async fn run(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let aws = aws::AwsClient::with(None, None).await?;
    let envs = aws
        .list_environments()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_environments: {e}"))?;
    if json {
        // Hand-rolled JSON to avoid pulling serde_json. Schema is flat and stable.
        let entries: Vec<String> = envs
            .iter()
            .map(|e| {
                format!(
                    "{{\"name\":\"{}\",\"application\":\"{}\",\"status\":\"{}\",\"health\":\"{}\",\"platform\":\"{}\",\"cname\":\"{}\",\"version_label\":\"{}\"}}",
                    cli_esc(&e.name),
                    cli_esc(&e.application),
                    cli_esc(&e.status),
                    cli_esc(&e.health),
                    cli_esc(&e.platform),
                    cli_esc(&e.cname),
                    cli_esc(&e.version_label),
                )
            })
            .collect();
        println!("[{}]", entries.join(","));
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
