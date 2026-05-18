use std::collections::BTreeMap;

use crate::util::config_file;

/// Parsed user-defined command template.
#[derive(Debug, Clone)]
pub struct Plugin {
    pub template: String,
    pub description: Option<String>,
}

/// Result of parsing `commands.toml`: the keep-list plus any warnings the user
/// should know about (collisions with built-ins, missing templates, …).
#[derive(Debug, Default, Clone)]
pub struct Loaded {
    pub plugins: BTreeMap<String, Plugin>,
    pub warnings: Vec<String>,
}

pub fn load(reserved: &[&str]) -> Loaded {
    let path = config_file("commands.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Loaded::default();
    };
    parse(&text, reserved)
}

/// Parse a minimal TOML subset specific to `commands.toml`:
///
/// ```toml
/// [commands.NAME]
/// template = "curl https://{cname}/_warm"
/// description = "optional"   # optional
/// ```
///
/// Other sections / keys are silently ignored. Quoted strings only; no
/// multi-line or triple-quoted strings. This keeps the parser tiny and
/// matches what real users will hand-write.
///
/// `reserved` is the set of names already claimed by built-in `:commands`. Any
/// plugin defined with a colliding name is dropped, and a warning is recorded
/// in [`Loaded::warnings`] so the app can surface it to the user instead of
/// silently picking the built-in.
pub fn parse(text: &str, reserved: &[&str]) -> Loaded {
    let mut out: BTreeMap<String, Plugin> = BTreeMap::new();
    let mut current_name: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let rest = rest.trim();
            current_name = rest.strip_prefix("commands.").map(|n| n.trim().to_string());
            continue;
        }
        let Some(name) = current_name.as_deref() else { continue };
        let Some((key, raw_val)) = line.split_once('=') else { continue };
        let value = raw_val.trim().trim_matches('"').to_string();
        let entry = out
            .entry(name.to_string())
            .or_insert(Plugin { template: String::new(), description: None });
        match key.trim() {
            "template" => entry.template = value,
            "description" => entry.description = Some(value),
            _ => {}
        }
    }
    let mut warnings = Vec::new();
    // Drop entries without templates.
    out.retain(|name, p| {
        if p.template.is_empty() {
            warnings.push(format!(
                "plugin '{name}' has no template — skipped"
            ));
            return false;
        }
        if reserved.contains(&name.as_str()) {
            warnings.push(format!(
                "plugin '{name}' collides with a built-in :{name} command — skipped"
            ));
            return false;
        }
        true
    });
    Loaded { plugins: out, warnings }
}

/// Substitute `{name}`, `{cname}`, `{application}`, `{tier}`, `{region}`,
/// `{profile}` placeholders. Unknown placeholders are left untouched.
pub fn render(
    template: &str,
    env_name: &str,
    cname: &str,
    application: &str,
    tier: &str,
    region: &str,
    profile: Option<&str>,
) -> String {
    template
        .replace("{name}", env_name)
        .replace("{env}", env_name)
        .replace("{cname}", cname)
        .replace("{application}", application)
        .replace("{app}", application)
        .replace("{tier}", tier)
        .replace("{region}", region)
        .replace("{profile}", profile.unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_commands_toml() {
        let text = r#"
# user commands
[commands.warm-cache]
template = "curl https://{cname}/_warm"

[commands.ssh]
template = "ssh ec2-user@{cname}"
description = "shell into one instance"
"#;
        let loaded = parse(text, &[]);
        let p = &loaded.plugins;
        assert_eq!(p.len(), 2);
        assert_eq!(p["warm-cache"].template, "curl https://{cname}/_warm");
        assert!(p["warm-cache"].description.is_none());
        assert_eq!(p["ssh"].template, "ssh ec2-user@{cname}");
        assert_eq!(p["ssh"].description.as_deref(), Some("shell into one instance"));
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn parse_skips_entries_without_template() {
        let text = r#"
[commands.broken]
description = "no template here"
"#;
        let loaded = parse(text, &[]);
        assert!(loaded.plugins.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("broken"));
    }

    #[test]
    fn parse_drops_and_warns_on_builtin_collision() {
        let text = r#"
[commands.refresh]
template = "echo hijacked"

[commands.warm]
template = "echo ok"
"#;
        let loaded = parse(text, &["refresh", "quit"]);
        assert!(loaded.plugins.contains_key("warm"));
        assert!(!loaded.plugins.contains_key("refresh"));
        assert!(loaded
            .warnings
            .iter()
            .any(|w| w.contains("refresh") && w.contains("built-in")));
    }

    #[test]
    fn parse_uses_default_template_when_only_description_after_collision() {
        // Sanity check: entries that survive the collision filter still need a template.
        let text = r#"
[commands.empty]
description = "no template here"
[commands.ok]
template = "echo"
"#;
        let loaded = parse(text, &[]);
        assert_eq!(loaded.plugins.len(), 1);
        assert!(loaded.plugins.contains_key("ok"));
    }

    #[test]
    fn render_substitutes_known_placeholders() {
        let out = render(
            "curl https://{cname}/_warm --header x-env:{name}",
            "prod-api",
            "prod-api.elb.amazonaws.com",
            "my-app",
            "Web",
            "eu-west-2",
            Some("prod"),
        );
        assert_eq!(
            out,
            "curl https://prod-api.elb.amazonaws.com/_warm --header x-env:prod-api"
        );
    }

    #[test]
    fn render_leaves_unknown_placeholders() {
        let out = render(
            "echo {nonsense} {name}",
            "p",
            "p.elb",
            "a",
            "Web",
            "us-east-1",
            None,
        );
        assert_eq!(out, "echo {nonsense} p");
    }
}
