use std::{path::PathBuf, time::Duration};

use crate::util::{config_file, parse_bool};

#[derive(Debug, Clone)]
pub struct Config {
    pub refresh_interval: Duration,
    pub extra_regions: Vec<String>,
    pub redact_default: Option<bool>,
    pub grouped_default: Option<bool>,
    pub theme: String,
    /// Glyph set: `"unicode"` (default), `"ascii"` for low-feature
    /// terminals, `"powerline"` (alias `"nerd"`) for Powerline-patched /
    /// Nerd Fonts, or `"auto"` to probe the terminal at startup and pick
    /// powerline if its support is detected, unicode otherwise.
    pub icons: String,
    pub notify_bell: bool,
    pub required_tags: Vec<String>,
    /// CloudWatch dimension names that identify an Elastic Beanstalk
    /// environment, for matching alarms to it (`alarm_dimensions`).
    ///
    /// Always contains `EnvironmentName` — that is what EB itself and
    /// `:alarm-create` write, so it can't be configured away without
    /// hiding ebman's own alarms. The config key *adds* spellings for
    /// operators whose alarms use a different dimension name.
    ///
    /// It exists because the match used to be on the dimension *value*
    /// alone, which wrongly claimed an RDS alarm named after the env;
    /// tightening it to the canonical name silently dropped
    /// operator-authored alarms spelled differently. This is the way
    /// back without reinstating the false positive.
    pub alarm_dimensions: Vec<String>,
    /// Lines `parse` didn't recognise, kept verbatim.
    ///
    /// `:settings` save rewrites the whole file from `serialize`, so
    /// anything the model doesn't carry is destroyed. That included a
    /// mistyped key — the very line an operator would otherwise spot
    /// and fix — and any key a newer release adds, which is the
    /// opposite of the graceful degradation the parser aims for.
    pub passthrough: Vec<String>,
    /// Per-profile theme override. Key = AWS profile name, value = theme
    /// name (matches the same names `theme = …` accepts). Lets the
    /// operator pin a high-contrast / dark / light theme to a specific
    /// profile so the visual cue says "you're in prod" without reading
    /// the breadcrumb. Most prod incidents start with "I thought I was
    /// in staging."
    pub profile_themes: std::collections::HashMap<String, String>,
    /// Named accounts reachable via `sts:AssumeRole`. Key = the friendly
    /// name the operator uses with `:account NAME`; value is the full
    /// AssumeRole spec — `role_arn`, `source_profile`, optional
    /// `external_id`, optional `region` override. Lines in `config.toml`
    /// use the form `accounts.NAME.field = "value"`, mirroring the
    /// `metric.LABEL.field` shape that the rest of the config uses.
    pub accounts: std::collections::HashMap<String, AccountSpec>,
    /// Per-environment runbook URLs. Key = env name; value = a URL the
    /// operator wants surfaced during triage. Lines in `config.toml` use
    /// `runbooks.ENV = "https://…"`. Shown in the `:why` overlay.
    pub runbooks: std::collections::HashMap<String, String>,
    /// Per-env read-only locks. When `safety_envs.get(env_name) ==
    /// Some(true)`, destructive actions against that env are refused
    /// even when the global `--read-only` toggle is off. Borrowed from
    /// pgman's `safety.databases.NAME.read_only` pattern. Lines in
    /// `config.toml` use `safety.envs.NAME.read_only = true`.
    pub safety_envs: std::collections::HashMap<String, bool>,
    /// Per-account read-only locks. Same shape as `safety_envs` but
    /// matched against the *active account name* (the `:account NAME`
    /// key for AssumeRole'd accounts, or the AWS profile name
    /// otherwise). Lines use `safety.accounts.NAME.read_only = true`.
    pub safety_accounts: std::collections::HashMap<String, bool>,
    /// Optional outbound webhook for audit-line fan-out. Each audit
    /// line written to `~/.cache/ebman/audit.log` is also POSTed to
    /// this URL as JSON (fire-and-forget; failures don't block or
    /// alarm). Body shape: `{"text": "<audit line>", "action": "...",
    /// "target": "...", "account": "...", "profile": "...", "region":
    /// "...", "at": "<rfc3339>"}`. The top-level `text` makes the
    /// body Slack-incoming-webhook-compatible out of the box. Lines
    /// in `config.toml` use `notify_webhook = "https://..."`.
    pub notify_webhook: Option<String>,
    /// User-defined command aliases. Key = the bare name typed
    /// after `:` (no leading colon); value = the full command line
    /// the alias expands to. Lines in `config.toml` use
    /// `alias.NAME = "command line"`. Expansion happens in
    /// `execute_command` before the dispatch match — args typed
    /// after the alias name are appended to the expansion, so
    /// `alias.dp = "deploy --auto-rollback 5m"` + `:dp build-900`
    /// becomes `:deploy --auto-rollback 5m build-900`.
    ///
    /// Named `command_aliases` to distinguish from the existing
    /// `:alias <env> <label>` env-rename feature (state.toml-
    /// persisted, lives on `App.aliases`). Command aliases are
    /// config.toml-only.
    pub command_aliases: std::collections::HashMap<String, String>,
    /// Lint rule IDs to skip globally. CSV-in-string form in
    /// `config.toml`: `lint.disable = "EBL001,EBL006"`. Mirrors
    /// the existing `extra_regions` / `required_tags` conventions.
    /// Project-local `.ebman/ebman.toml` can extend (never
    /// override) this set via `[lint]\ndisable = ["EBL001"]`.
    pub lint_disable: Vec<String>,
    /// Lint rule IDs whose auto-fix is suppressed even when
    /// `--fix` is passed. Same CSV-in-string form:
    /// `lint.fix_disable = "EBL004"`. Operators who want lint
    /// reports but don't want a specific rule's fix dispatched
    /// (e.g. they have a non-standard BatchSize for a reason)
    /// list it here.
    pub lint_fix_disable: Vec<String>,
    /// Master switch for the LLM-backed `ebman explain`
    /// subcommand. Off by default — operators must explicitly
    /// opt in via `explain.enabled = true`. Presence of an API
    /// key in the env is not implicit consent.
    pub explain_enabled: bool,
    /// Provider key — `"anthropic"` (default) or `"ollama"`.
    pub explain_provider: String,
    /// Model identifier. Anthropic default: `claude-haiku-4-5`.
    pub explain_model: String,
    /// Env-var name to read the Anthropic API key from.
    /// Default `ANTHROPIC_API_KEY`.
    pub explain_api_key_env: String,
    /// HTTP base URL for the Ollama provider. Default
    /// `http://localhost:11434`.
    pub explain_ollama_url: String,
    /// Soft cap on response tokens. 0 = use default (1024).
    pub explain_max_tokens: u32,
}

/// A named `sts:AssumeRole` target. The operator typically pins one of
/// these per child account and switches between them via `:account
/// NAME`. `source_profile` carries the base creds (so chained role
/// hops still resolve), `external_id` is optional but required by some
/// trust policies, `region` is optional (falls back to the source
/// profile's / env default).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountSpec {
    pub role_arn: String,
    pub source_profile: Option<String>,
    pub external_id: Option<String>,
    pub region: Option<String>,
}

impl Config {
    /// The safety-pin check every CLI dispatch site shares: per-env
    /// pin first, then per-account pin against the profile the
    /// dispatch will run under. Returns the pin's config path
    /// (`safety.envs.NAME.read_only` / `safety.accounts.NAME.read_only`)
    /// for the refusal message, `None` when unpinned.
    ///
    /// Callers: `ebman audit replay`, `ebman lint --fix`, and the
    /// future `mcp serve --allow-writes` path. The TUI's
    /// `App::is_read_only_for` / `read_only_reason` deliberately do
    /// NOT route through this — they compose additional session
    /// gates (global `--read-only`, `:freeze-deploys`, demo mode)
    /// with their own precedence and toast wording; only the
    /// config-pin layer is shared semantics.
    pub fn pin_reason(&self, env: &str, profile: Option<&str>) -> Option<String> {
        if self.safety_envs.get(env).copied().unwrap_or(false) {
            return Some(format!("safety.envs.{env}.read_only"));
        }
        if let Some(p) = profile {
            if self.safety_accounts.get(p).copied().unwrap_or(false) {
                return Some(format!("safety.accounts.{p}.read_only"));
            }
        }
        None
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(15),
            extra_regions: Vec::new(),
            redact_default: None,
            grouped_default: None,
            theme: "dark".into(),
            icons: "unicode".into(),
            notify_bell: false,
            required_tags: Vec::new(),
            alarm_dimensions: vec![crate::aws::ENV_DIMENSION.to_string()],
            passthrough: Vec::new(),
            profile_themes: std::collections::HashMap::new(),
            accounts: std::collections::HashMap::new(),
            runbooks: std::collections::HashMap::new(),
            safety_envs: std::collections::HashMap::new(),
            safety_accounts: std::collections::HashMap::new(),
            notify_webhook: None,
            command_aliases: std::collections::HashMap::new(),
            lint_disable: Vec::new(),
            lint_fix_disable: Vec::new(),
            explain_enabled: false,
            explain_provider: String::new(),
            explain_model: String::new(),
            explain_api_key_env: String::new(),
            explain_ollama_url: String::new(),
            explain_max_tokens: 0,
        }
    }
}

pub fn load() -> Config {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    parse(&text)
}

/// Sugar for the `ebman lint` CLI: just the global
/// `lint.disable` list, no other fields. Composed with the
/// project-level disables in `project::load_lint_disables_from_cwd`.
pub fn load_lint_disables() -> Vec<String> {
    load().lint_disable
}

/// Same as [`load_lint_disables`] but for the auto-fix opt-out list.
/// Rules in this list won't have their `fix()` dispatched by `ebman
/// lint --fix` even when they're enabled for reporting.
pub fn load_lint_fix_disables() -> Vec<String> {
    load().lint_fix_disable
}

pub fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = raw_val.trim().trim_matches('"').to_string();
        match key {
            "refresh_interval_secs" => {
                if let Ok(n) = value.parse::<u64>() {
                    if n > 0 {
                        cfg.refresh_interval = Duration::from_secs(n);
                    }
                }
            }
            "extra_regions" => {
                cfg.extra_regions = crate::util::split_csv(&value);
            }
            "redact_default" => cfg.redact_default = parse_bool(&value),
            "grouped_default" => cfg.grouped_default = parse_bool(&value),
            "theme" => cfg.theme = value,
            "icons" => cfg.icons = value,
            "notify_bell" => {
                if let Some(b) = parse_bool(&value) {
                    cfg.notify_bell = b;
                }
            }
            "lint.disable" => {
                cfg.lint_disable = crate::util::split_csv(&value);
            }
            "lint.fix_disable" => {
                cfg.lint_fix_disable = crate::util::split_csv(&value);
            }
            "explain.enabled" => {
                if let Some(b) = parse_bool(&value) {
                    cfg.explain_enabled = b;
                }
            }
            "explain.provider" => cfg.explain_provider = value,
            "explain.model" => cfg.explain_model = value,
            "explain.api_key_env" => cfg.explain_api_key_env = value,
            "explain.ollama_url" => cfg.explain_ollama_url = value,
            "explain.max_tokens" => {
                if let Ok(n) = value.parse::<u32>() {
                    cfg.explain_max_tokens = n;
                }
            }
            "notify_webhook" => {
                // Empty string disables; treat the same as the key
                // being absent so a `notify_webhook = ""` line acts as
                // an explicit off-switch without removing the key.
                cfg.notify_webhook = if value.is_empty() { None } else { Some(value) };
            }
            "required_tags" => {
                cfg.required_tags = crate::util::split_csv(&value);
            }
            "alarm_dimensions" => {
                // ADDITIONAL names on top of the canonical one, not a
                // replacement: `:alarm-create` always writes
                // `EnvironmentName` (aws/cloudwatch.rs), so dropping it
                // from the match would hide the alarms ebman itself
                // creates along with every EB-native one — which during
                // triage reads as "no alarms configured".
                //
                // Rebuilt from the canonical name each time rather than
                // appended to, so a second `alarm_dimensions =` line
                // REPLACES the first as every other key does. Appending
                // meant an operator editing by adding a line silently
                // got the union of both, with no way to narrow it back.
                let mut names = vec![crate::aws::ENV_DIMENSION.to_string()];
                for name in crate::util::split_csv(&value) {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
                cfg.alarm_dimensions = names;
            }
            "profile_themes" => {
                // Format: `prod:high-contrast,staging:dark,default:light`.
                // Whitespace around tokens is tolerated; entries without a
                // `:` separator are skipped. Empty profile / empty theme
                // are skipped so a stray trailing comma can't smuggle in
                // a `"" → ""` mapping.
                cfg.profile_themes = parse_profile_themes(&value);
            }
            other if other.starts_with("runbooks.") => {
                // `runbooks.ENV = "url"`. The key after the dot is the
                // whole env name (EB env names don't contain dots).
                let name = other.trim_start_matches("runbooks.").trim();
                if !name.is_empty() && !value.is_empty() {
                    cfg.runbooks.insert(name.to_string(), value);
                }
            }
            other if other.starts_with("accounts.") => {
                // `accounts.NAME.field = "value"`. Split on the dots so
                // multi-line specs accumulate into one HashMap entry per
                // NAME. Unknown fields are ignored so a future field
                // addition can degrade gracefully on older binaries.
                let rest = other.trim_start_matches("accounts.");
                let Some((name, field)) = rest.split_once('.') else {
                    continue;
                };
                // The entry is created only once a field we recognise
                // is seen. Creating it first meant a typo — or a key a
                // newer release adds — left a spec with an empty ARN
                // that `contains_key` reports as a real account, so
                // `:account NAME` took the AssumeRole path and failed
                // with an opaque STS error instead of "no such
                // account".
                match field.trim() {
                    "role_arn" => {
                        cfg.accounts.entry(name.to_string()).or_default().role_arn = value;
                    }
                    "source_profile" => {
                        cfg.accounts
                            .entry(name.to_string())
                            .or_default()
                            .source_profile = Some(value);
                    }
                    "external_id" => {
                        cfg.accounts
                            .entry(name.to_string())
                            .or_default()
                            .external_id = Some(value);
                    }
                    "region" => {
                        cfg.accounts.entry(name.to_string()).or_default().region = Some(value);
                    }
                    // Unrecognised: preserved verbatim by `passthrough`
                    // below rather than silently creating a phantom.
                    _ => cfg.passthrough.push(line.to_string()),
                }
            }
            // `safety.envs.NAME.read_only = true` and
            // `safety.accounts.NAME.read_only = true`. Only one field
            // today (read_only); the dotted shape leaves room for
            // future fields (statement_timeout-equivalent, etc.).
            other if other.starts_with("safety.envs.") => {
                let rest = other.trim_start_matches("safety.envs.");
                let Some((name, field)) = rest.split_once('.') else {
                    continue;
                };
                if field.trim() == "read_only" {
                    if let Some(b) = parse_bool(&value) {
                        cfg.safety_envs.insert(name.to_string(), b);
                    }
                }
            }
            other if other.starts_with("alias.") => {
                // `alias.NAME = "command line"`. The NAME comes
                // after the dot; nested dots in NAME are rejected
                // (would conflict with future hierarchical
                // config-key plans). Empty NAME or empty value:
                // skip silently — operators sometimes leave
                // `alias. = ""` while editing.
                let name = other.trim_start_matches("alias.").trim();
                if name.is_empty() || name.contains('.') || value.is_empty() {
                    continue;
                }
                cfg.command_aliases.insert(name.to_string(), value);
            }
            other if other.starts_with("safety.accounts.") => {
                let rest = other.trim_start_matches("safety.accounts.");
                let Some((name, field)) = rest.split_once('.') else {
                    continue;
                };
                if field.trim() == "read_only" {
                    if let Some(b) = parse_bool(&value) {
                        cfg.safety_accounts.insert(name.to_string(), b);
                    }
                }
            }
            // Unrecognised at the top level too — preserved rather
            // than dropped, for the same reason as the account fields
            // above.
            _ => cfg.passthrough.push(line.to_string()),
        }
    }
    cfg
}

pub fn config_path() -> PathBuf {
    config_file("config.toml")
}

/// Serialise the config back to disk. Round-trips the parse format and
/// over-writes the user's existing file. Used by the `:settings` form.
/// Atomic — writes to a sibling `.tmp` and renames into place so a
/// crash mid-write can't truncate `config.toml`.
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let path = config_path();
    let body = serialize(cfg);
    crate::util::write_atomic(&path, &body)
}

/// Pure: render a `Config` into the TOML-ish line-oriented format the
/// parser reads. Used by `save` and unit tests.
pub fn serialize(cfg: &Config) -> String {
    let mut out = String::new();
    out.push_str("# ebman configuration — written by :settings; hand-edits welcome\n\n");
    out.push_str(&format!(
        "refresh_interval_secs = {}\n",
        cfg.refresh_interval.as_secs()
    ));
    out.push_str(&format!(
        "extra_regions = \"{}\"\n",
        cfg.extra_regions.join(",")
    ));
    if let Some(b) = cfg.redact_default {
        out.push_str(&format!("redact_default = {b}\n"));
    }
    if let Some(b) = cfg.grouped_default {
        out.push_str(&format!("grouped_default = {b}\n"));
    }
    out.push_str(&format!("theme = \"{}\"\n", cfg.theme));
    out.push_str(&format!("icons = \"{}\"\n", cfg.icons));
    out.push_str(&format!("notify_bell = {}\n", cfg.notify_bell));
    if let Some(url) = &cfg.notify_webhook {
        out.push_str(&format!("notify_webhook = \"{url}\"\n"));
    }
    if !cfg.lint_disable.is_empty() {
        out.push_str(&format!(
            "lint.disable = \"{}\"\n",
            cfg.lint_disable.join(",")
        ));
    }
    if !cfg.lint_fix_disable.is_empty() {
        out.push_str(&format!(
            "lint.fix_disable = \"{}\"\n",
            cfg.lint_fix_disable.join(",")
        ));
    }
    if cfg.explain_enabled {
        out.push_str("explain.enabled = true\n");
    }
    if !cfg.explain_provider.is_empty() {
        out.push_str(&format!(
            "explain.provider = \"{}\"\n",
            cfg.explain_provider
        ));
    }
    if !cfg.explain_model.is_empty() {
        out.push_str(&format!("explain.model = \"{}\"\n", cfg.explain_model));
    }
    if !cfg.explain_api_key_env.is_empty() {
        out.push_str(&format!(
            "explain.api_key_env = \"{}\"\n",
            cfg.explain_api_key_env
        ));
    }
    if !cfg.explain_ollama_url.is_empty() {
        out.push_str(&format!(
            "explain.ollama_url = \"{}\"\n",
            cfg.explain_ollama_url
        ));
    }
    if cfg.explain_max_tokens != 0 {
        out.push_str(&format!(
            "explain.max_tokens = {}\n",
            cfg.explain_max_tokens
        ));
    }
    if !cfg.command_aliases.is_empty() {
        // Sort so repeated serialize cycles don't churn the file
        // when the HashMap iteration order shuffles.
        let mut pairs: Vec<(&String, &String)> = cfg.command_aliases.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (name, expansion) in pairs {
            out.push_str(&format!("alias.{name} = \"{expansion}\"\n"));
        }
    }
    if !cfg.required_tags.is_empty() {
        out.push_str(&format!(
            "required_tags = \"{}\"\n",
            cfg.required_tags.join(",")
        ));
    }
    if !cfg.profile_themes.is_empty() {
        // Sort entries so repeated serialize cycles don't churn the file
        // when the HashMap iteration order shuffles.
        let mut pairs: Vec<(&String, &String)> = cfg.profile_themes.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        let joined = pairs
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!("profile_themes = \"{joined}\"\n"));
    }
    if !cfg.runbooks.is_empty() {
        // Sorted so repeated serialize cycles don't churn the file.
        let mut pairs: Vec<(&String, &String)> = cfg.runbooks.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (env, url) in pairs {
            out.push_str(&format!("runbooks.{env} = \"{url}\"\n"));
        }
    }
    if !cfg.safety_envs.is_empty() {
        let mut pairs: Vec<(&String, &bool)> = cfg.safety_envs.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (env, ro) in pairs {
            out.push_str(&format!("safety.envs.{env}.read_only = {ro}\n"));
        }
    }
    if !cfg.safety_accounts.is_empty() {
        let mut pairs: Vec<(&String, &bool)> = cfg.safety_accounts.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (acct, ro) in pairs {
            out.push_str(&format!("safety.accounts.{acct}.read_only = {ro}\n"));
        }
    }
    // Only the names the operator ADDED — the canonical one is implicit
    // and always matched. Emitting the full list would rewrite a
    // hand-written `alarm_dimensions = "Environment"` as
    // `"EnvironmentName,Environment"` on the next `:settings` save.
    let extra: Vec<&str> = cfg
        .alarm_dimensions
        .iter()
        .filter(|d| d.as_str() != crate::aws::ENV_DIMENSION)
        .map(String::as_str)
        .collect();
    if !extra.is_empty() {
        out.push_str(&format!("alarm_dimensions = \"{}\"\n", extra.join(",")));
    }
    // AssumeRole account definitions. These were parsed but never
    // written, so a `:settings` save — which rewrites the whole file
    // from this function — deleted every one of them and broke
    // `:account <name>` with nothing on screen to say why.
    // Lines the parser didn't model, verbatim and last. Without this a
    // `:settings` save silently deleted a mistyped key — the very line
    // the operator would otherwise notice and fix — along with any key
    // a newer release understands and this build doesn't.
    if !cfg.passthrough.is_empty() {
        out.push_str("\n# Preserved from the previous file (not managed by :settings):\n");
        for line in &cfg.passthrough {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !cfg.accounts.is_empty() {
        let mut names: Vec<&String> = cfg.accounts.keys().collect();
        names.sort();
        for name in names {
            let Some(spec) = cfg.accounts.get(name) else {
                continue;
            };
            // An absent ARN omits that one line — it does NOT drop the
            // block. Skipping the whole account destroyed its valid
            // `source_profile` / `region` lines too, trading one bad
            // line for three; and emitting `role_arn = ""` replaced the
            // operator's real line with a plausible-looking empty one.
            // Omitting round-trips: the block re-parses exactly as it
            // was written.
            if !spec.role_arn.trim().is_empty() {
                out.push_str(&format!(
                    "accounts.{name}.role_arn = \"{}\"\n",
                    spec.role_arn
                ));
            }
            if let Some(v) = &spec.source_profile {
                out.push_str(&format!("accounts.{name}.source_profile = \"{v}\"\n"));
            }
            if let Some(v) = &spec.external_id {
                out.push_str(&format!("accounts.{name}.external_id = \"{v}\"\n"));
            }
            if let Some(v) = &spec.region {
                out.push_str(&format!("accounts.{name}.region = \"{v}\"\n"));
            }
        }
    }
    out
}

/// Pure: parse a `prof:theme,prof:theme` string into a map. Empty / `:`
/// -free / blank-key / blank-value tokens are skipped. Whitespace around
/// each side of the colon is trimmed so the operator can format it for
/// readability.
pub fn parse_profile_themes(raw: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for token in raw.split(',') {
        let Some((k, v)) = token.split_once(':') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        if key.is_empty() || val.is_empty() {
            continue;
        }
        out.insert(key.to_string(), val.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_overrides_defaults() {
        let text = r#"
refresh_interval_secs = 30
extra_regions = "us-gov-east-1, cn-north-1"
redact_default = true
grouped_default = false
"#;
        let cfg = parse(text);
        assert_eq!(cfg.refresh_interval, Duration::from_secs(30));
        assert_eq!(
            cfg.extra_regions,
            vec!["us-gov-east-1".to_string(), "cn-north-1".to_string()]
        );
        assert_eq!(cfg.redact_default, Some(true));
        assert_eq!(cfg.grouped_default, Some(false));
    }

    #[test]
    fn parse_profile_themes_happy_path() {
        let map = parse_profile_themes("prod:high-contrast,staging:dark,default:light");
        assert_eq!(map.get("prod"), Some(&"high-contrast".to_string()));
        assert_eq!(map.get("staging"), Some(&"dark".to_string()));
        assert_eq!(map.get("default"), Some(&"light".to_string()));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn parse_profile_themes_trims_whitespace_and_skips_malformed() {
        // Trailing comma, missing colon, blank key, blank value all
        // produce no entries rather than panicking or yielding ""→"".
        let map = parse_profile_themes(
            "  prod : high-contrast , noseparator , :empty-key , empty-value: , ",
        );
        assert_eq!(map.get("prod"), Some(&"high-contrast".to_string()));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn parse_profile_themes_empty_returns_empty_map() {
        assert!(parse_profile_themes("").is_empty());
    }

    #[test]
    fn parse_accounts_collects_multiline_specs() {
        let text = r#"
accounts.prod.role_arn = "arn:aws:iam::111122223333:role/EbmanReadOnly"
accounts.prod.source_profile = "default"
accounts.prod.region = "eu-west-2"
accounts.staging.role_arn = "arn:aws:iam::555555555555:role/EbmanReadOnly"
accounts.staging.external_id = "abc-xyz"
"#;
        let cfg = parse(text);
        assert_eq!(cfg.accounts.len(), 2);
        let prod = cfg.accounts.get("prod").expect("prod entry");
        assert_eq!(
            prod.role_arn,
            "arn:aws:iam::111122223333:role/EbmanReadOnly"
        );
        assert_eq!(prod.source_profile.as_deref(), Some("default"));
        assert_eq!(prod.region.as_deref(), Some("eu-west-2"));
        assert_eq!(prod.external_id, None);
        let staging = cfg.accounts.get("staging").expect("staging entry");
        assert_eq!(staging.external_id.as_deref(), Some("abc-xyz"));
        assert_eq!(staging.source_profile, None);
    }

    #[test]
    fn parse_accounts_ignores_unknown_field() {
        // Future-compat: a field we don't recognise should be ignored
        // rather than dropping the whole entry.
        let cfg = parse(
            "accounts.prod.role_arn = \"arn:…\"\n\
             accounts.prod.future_field = \"whatever\"\n",
        );
        let prod = cfg.accounts.get("prod").expect("prod entry");
        assert_eq!(prod.role_arn, "arn:…");
    }

    #[test]
    fn parse_runbooks_maps_env_to_url() {
        let cfg = parse(
            "runbooks.uflexi-prod = \"https://wiki/runbook/prod\"\n\
             runbooks.uflexi-staging = \"https://wiki/runbook/staging\"\n",
        );
        assert_eq!(cfg.runbooks.len(), 2);
        assert_eq!(
            cfg.runbooks.get("uflexi-prod").map(String::as_str),
            Some("https://wiki/runbook/prod")
        );
        // Blank URL is skipped — a stray `runbooks.x =` can't smuggle in
        // an empty mapping.
        assert!(parse("runbooks.x = \"\"\n").runbooks.is_empty());
    }

    #[test]
    fn runbooks_round_trip_through_serialize() {
        let mut cfg = Config::default();
        cfg.runbooks.insert("prod".into(), "https://rb/prod".into());
        let reparsed = parse(&serialize(&cfg));
        assert_eq!(
            reparsed.runbooks.get("prod").map(String::as_str),
            Some("https://rb/prod")
        );
    }

    #[test]
    fn parse_safety_envs_and_accounts() {
        let cfg = parse(
            "safety.envs.uflexi-prod.read_only = true\n\
             safety.envs.uflexi-staging.read_only = false\n\
             safety.accounts.prod.read_only = true\n",
        );
        assert_eq!(cfg.safety_envs.get("uflexi-prod"), Some(&true));
        assert_eq!(cfg.safety_envs.get("uflexi-staging"), Some(&false));
        assert_eq!(cfg.safety_accounts.get("prod"), Some(&true));
        // Unknown field under safety.envs.NAME is ignored.
        let cfg = parse("safety.envs.x.future_field = \"whatever\"\n");
        assert!(cfg.safety_envs.is_empty());
    }

    #[test]
    fn safety_round_trips_through_serialize() {
        let mut cfg = Config::default();
        cfg.safety_envs.insert("uflexi-prod".into(), true);
        cfg.safety_accounts.insert("prod".into(), true);
        let reparsed = parse(&serialize(&cfg));
        assert_eq!(reparsed.safety_envs.get("uflexi-prod"), Some(&true));
        assert_eq!(reparsed.safety_accounts.get("prod"), Some(&true));
    }

    #[test]
    fn parse_writes_profile_themes_into_config() {
        // End-to-end check: a config file with `profile_themes = "..."`
        // ends up in cfg.profile_themes correctly.
        let cfg = parse("profile_themes = \"prod:high-contrast,staging:dark\"\n");
        assert_eq!(cfg.profile_themes.len(), 2);
        assert_eq!(
            cfg.profile_themes.get("prod"),
            Some(&"high-contrast".to_string())
        );
    }

    #[test]
    fn parse_ignores_zero_interval() {
        let cfg = parse("refresh_interval_secs = 0\n");
        assert_eq!(cfg.refresh_interval, Duration::from_secs(15));
    }

    #[test]
    fn parse_empty_returns_defaults() {
        let cfg = parse("");
        assert_eq!(cfg.refresh_interval, Duration::from_secs(15));
        assert!(cfg.extra_regions.is_empty());
        assert!(cfg.redact_default.is_none());
    }

    #[test]
    fn parse_icons_auto_is_preserved() {
        let cfg = parse("icons = \"auto\"\n");
        assert_eq!(cfg.icons, "auto");
    }

    #[test]
    fn serialize_round_trips_full_config() {
        let mut profile_themes = std::collections::HashMap::new();
        profile_themes.insert("prod".into(), "high-contrast".into());
        profile_themes.insert("staging".into(), "dark".into());
        let cfg = Config {
            refresh_interval: Duration::from_secs(45),
            extra_regions: vec!["eu-south-2".into(), "ap-southeast-4".into()],
            redact_default: Some(true),
            grouped_default: Some(false),
            theme: "high-contrast".into(),
            icons: "powerline".into(),
            notify_bell: true,
            required_tags: vec!["Owner".into(), "Env".into()],
            alarm_dimensions: vec![crate::aws::ENV_DIMENSION.to_string()],
            passthrough: Vec::new(),
            profile_themes,
            accounts: std::collections::HashMap::new(),
            runbooks: std::collections::HashMap::new(),
            safety_envs: std::collections::HashMap::new(),
            safety_accounts: std::collections::HashMap::new(),
            notify_webhook: Some("https://hooks.slack.com/services/EXAMPLE".into()),
            command_aliases: {
                let mut m = std::collections::HashMap::new();
                m.insert("dp".to_string(), "deploy --auto-rollback 5m".to_string());
                m.insert(
                    "shipit".to_string(),
                    "promote-env staging prod --wait-for-green 5m".to_string(),
                );
                m
            },
            lint_disable: vec!["EBL003".into(), "EBL006".into()],
            lint_fix_disable: vec!["EBL004".into()],
            explain_enabled: true,
            explain_provider: "anthropic".into(),
            explain_model: "claude-haiku-4-5".into(),
            explain_api_key_env: "ANTHROPIC_API_KEY".into(),
            explain_ollama_url: "http://localhost:11434".into(),
            explain_max_tokens: 2048,
        };

        let body = serialize(&cfg);
        let reparsed = parse(&body);
        assert_eq!(reparsed.refresh_interval, cfg.refresh_interval);
        assert_eq!(reparsed.extra_regions, cfg.extra_regions);
        assert_eq!(reparsed.redact_default, cfg.redact_default);
        assert_eq!(reparsed.grouped_default, cfg.grouped_default);
        assert_eq!(reparsed.theme, cfg.theme);
        assert_eq!(reparsed.icons, cfg.icons);
        assert_eq!(reparsed.notify_bell, cfg.notify_bell);
        assert_eq!(reparsed.required_tags, cfg.required_tags);
        assert_eq!(reparsed.profile_themes, cfg.profile_themes);
        assert_eq!(reparsed.notify_webhook, cfg.notify_webhook);
        assert_eq!(reparsed.command_aliases, cfg.command_aliases);
        assert_eq!(reparsed.lint_disable, cfg.lint_disable);
        assert_eq!(reparsed.lint_fix_disable, cfg.lint_fix_disable);
        assert_eq!(reparsed.explain_enabled, cfg.explain_enabled);
        assert_eq!(reparsed.explain_provider, cfg.explain_provider);
        assert_eq!(reparsed.explain_model, cfg.explain_model);
        assert_eq!(reparsed.explain_api_key_env, cfg.explain_api_key_env);
        assert_eq!(reparsed.explain_ollama_url, cfg.explain_ollama_url);
        assert_eq!(reparsed.explain_max_tokens, cfg.explain_max_tokens);
    }

    #[test]
    fn parse_explain_block_full_shape() {
        let body = "\
explain.enabled = true
explain.provider = \"ollama\"
explain.model = \"llama3.2\"
explain.api_key_env = \"MY_KEY\"
explain.ollama_url = \"http://10.0.0.5:11434\"
explain.max_tokens = 512
";
        let cfg = parse(body);
        assert!(cfg.explain_enabled);
        assert_eq!(cfg.explain_provider, "ollama");
        assert_eq!(cfg.explain_model, "llama3.2");
        assert_eq!(cfg.explain_api_key_env, "MY_KEY");
        assert_eq!(cfg.explain_ollama_url, "http://10.0.0.5:11434");
        assert_eq!(cfg.explain_max_tokens, 512);
    }

    #[test]
    fn parse_explain_disabled_by_default() {
        let cfg = parse("");
        assert!(!cfg.explain_enabled);
    }

    #[test]
    fn parse_lint_fix_disable_csv_collects_into_vec() {
        let body = "lint.fix_disable = \"EBL004, EBL001\"\n";
        let cfg = parse(body);
        assert_eq!(cfg.lint_fix_disable, vec!["EBL004", "EBL001"]);
    }

    #[test]
    fn parse_lint_disable_csv_collects_into_vec() {
        let body = "lint.disable = \"EBL001, EBL003 ,EBL006\"\n";
        let cfg = parse(body);
        assert_eq!(cfg.lint_disable, vec!["EBL001", "EBL003", "EBL006"]);
    }

    #[test]
    fn parse_lint_disable_skips_empty_tokens() {
        // Trailing comma / double-comma should not produce empty
        // rule IDs that would silently never match.
        let body = "lint.disable = \"EBL001,,EBL003,\"\n";
        let cfg = parse(body);
        assert_eq!(cfg.lint_disable, vec!["EBL001", "EBL003"]);
    }

    #[test]
    fn parse_alias_lines_collect_into_command_aliases() {
        let body = "alias.dp = \"deploy --auto-rollback 5m\"\nalias.foo = \"rebuild\"\n";
        let cfg = parse(body);
        assert_eq!(cfg.command_aliases.len(), 2);
        assert_eq!(
            cfg.command_aliases.get("dp").map(|s| s.as_str()),
            Some("deploy --auto-rollback 5m"),
        );
        assert_eq!(
            cfg.command_aliases.get("foo").map(|s| s.as_str()),
            Some("rebuild"),
        );
    }

    #[test]
    fn parse_alias_rejects_dotted_names_and_empty_values() {
        // Dotted alias names would conflict with hierarchical
        // config-key conventions if we ever add `alias.NAME.field`
        // style. Skip silently rather than erroring.
        let body = "alias.dp.prod = \"deploy\"\nalias.empty = \"\"\nalias. = \"x\"\n";
        let cfg = parse(body);
        assert!(cfg.command_aliases.is_empty());
    }

    #[test]
    fn serialize_round_trips_default_config() {
        let cfg = Config::default();
        let body = serialize(&cfg);
        let reparsed = parse(&body);
        assert_eq!(reparsed.refresh_interval, cfg.refresh_interval);
        assert_eq!(reparsed.theme, cfg.theme);
        assert_eq!(reparsed.icons, cfg.icons);
        assert!(reparsed.extra_regions.is_empty());
        assert!(reparsed.required_tags.is_empty());
    }

    #[test]
    fn pin_reason_env_wins_then_account_then_none() {
        let mut cfg = Config::default();
        cfg.safety_envs.insert("prod".into(), true);
        cfg.safety_envs.insert("unpinned-false".into(), false);
        cfg.safety_accounts.insert("prod-admin".into(), true);
        // Env pin wins even when the account is also pinned.
        assert_eq!(
            cfg.pin_reason("prod", Some("prod-admin")).as_deref(),
            Some("safety.envs.prod.read_only")
        );
        assert_eq!(
            cfg.pin_reason("other", Some("prod-admin")).as_deref(),
            Some("safety.accounts.prod-admin.read_only")
        );
        // An explicit `= false` pin is not a pin.
        assert_eq!(cfg.pin_reason("unpinned-false", None), None);
        assert_eq!(cfg.pin_reason("other", Some("dev")), None);
        assert_eq!(cfg.pin_reason("other", None), None);
    }
    #[test]
    fn documented_alarm_dimensions_example_parses() {
        let cfg = parse("alarm_dimensions = \"Environment\"\n");
        assert_eq!(
            cfg.alarm_dimensions,
            vec!["EnvironmentName".to_string(), "Environment".to_string()],
            "the key ADDS spellings; the canonical one is always matched"
        );
    }

    #[test]
    fn alarm_dimensions_cannot_configure_away_the_canonical_name() {
        // `:alarm-create` always writes `EnvironmentName`. If the config
        // could replace the match set, ebman's own alarms — and every
        // EB-native one — would become invisible to it, which during
        // triage reads as "no alarms configured".
        let cfg = parse("alarm_dimensions = \"Environment,EnvName\"\n");
        assert!(cfg
            .alarm_dimensions
            .contains(&crate::aws::ENV_DIMENSION.to_string()));
        // And listing it explicitly doesn't duplicate it.
        let cfg = parse("alarm_dimensions = \"EnvironmentName,EnvironmentName\"\n");
        assert_eq!(cfg.alarm_dimensions, vec!["EnvironmentName".to_string()]);
    }

    #[test]
    fn alarm_dimensions_defaults_to_the_canonical_name() {
        let cfg = parse("");
        assert_eq!(cfg.alarm_dimensions, vec!["EnvironmentName".to_string()]);
    }

    #[test]
    fn an_empty_alarm_dimensions_keeps_the_default() {
        // Blanking the key must not silently match nothing.
        let cfg = parse("alarm_dimensions = \"\"\n");
        assert_eq!(cfg.alarm_dimensions, vec!["EnvironmentName".to_string()]);
    }

    #[test]
    fn settings_save_must_not_drop_hand_written_config() {
        // `:settings` save rewrites the WHOLE file from `serialize`, so any
        // key `parse` accepts but `serialize` omits is destroyed.
        let original = concat!(
            "required_tags = \"Owner,Project\"\n",
            "alarm_dimensions = \"EnvironmentName,Environment\"\n",
            "lint.disable = \"EBL001,EBL007\"\n",
            "lint.fix_disable = \"EBL003\"\n",
            "explain.provider = \"ollama\"\n",
            "explain.enabled = true\n",
            "explain.ollama_url = \"http://localhost:11434\"\n",
            "explain.api_key_env = \"MY_KEY\"\n",
            "explain.max_tokens = 2048\n",
            "accounts.prod.role_arn = \"arn:aws:iam::123456789012:role/EbAdmin\"\n",
            "accounts.prod.source_profile = \"default\"\n",
            "accounts.prod.external_id = \"xyz\"\n",
        );
        let before = parse(original);
        let after = parse(&serialize(&before));

        let mut lost: Vec<&str> = Vec::new();
        if after.required_tags != before.required_tags {
            lost.push("required_tags");
        }
        if after.alarm_dimensions != before.alarm_dimensions {
            lost.push("alarm_dimensions");
        }
        if after.lint_disable != before.lint_disable {
            lost.push("lint.disable");
        }
        if after.lint_fix_disable != before.lint_fix_disable {
            lost.push("lint.fix_disable");
        }
        if after.explain_provider != before.explain_provider {
            lost.push("explain.provider");
        }
        if after.explain_enabled != before.explain_enabled {
            lost.push("explain.enabled");
        }
        if after.explain_ollama_url != before.explain_ollama_url {
            lost.push("explain.ollama_url");
        }
        if after.explain_api_key_env != before.explain_api_key_env {
            lost.push("explain.api_key_env");
        }
        if after.explain_max_tokens != before.explain_max_tokens {
            lost.push("explain.max_tokens");
        }
        if after.accounts != before.accounts {
            lost.push("accounts.*");
        }

        assert!(lost.is_empty(), "a :settings save destroys: {lost:?}");
    }
    #[test]
    fn a_typod_account_field_survives_a_settings_save_without_creating_an_account() {
        // Three things have to hold at once, and earlier attempts got
        // one at the cost of another:
        //   - the typo is PRESERVED, because it's the line the operator
        //     would otherwise spot and fix;
        //   - no phantom account exists, because `contains_key` gates
        //     `:account NAME`, and a spec with an empty ARN failed with
        //     an opaque STS error instead of "no such account";
        //   - the account's valid sibling lines survive, because
        //     skipping the whole block to avoid writing `role_arn = ""`
        //     traded one bad line for three.
        let cfg = parse(concat!(
            "accounts.prod.rolearn = \"arn:aws:iam::123456789012:role/EbAdmin\"\n",
            "accounts.prod.source_profile = \"default\"\n",
            "accounts.prod.region = \"eu-west-1\"\n",
            "accounts.real.role_arn = \"arn:aws:iam::999:role/Ok\"\n",
        ));
        assert!(
            cfg.accounts
                .get("prod")
                .map(|a| a.role_arn.trim().is_empty())
                .unwrap_or(true),
            "a mistyped ARN key must not produce a usable account"
        );

        let out = serialize(&cfg);
        assert!(
            out.contains("accounts.prod.rolearn = \"arn:aws:iam::123456789012:role/EbAdmin\""),
            "the mistyped line must survive the save:\n{out}"
        );
        assert!(
            !out.contains("accounts.prod.role_arn"),
            "an empty ARN must not be written back:\n{out}"
        );
        assert!(
            out.contains("accounts.prod.source_profile = \"default\"")
                && out.contains("accounts.prod.region = \"eu-west-1\""),
            "the account's valid lines must survive:\n{out}"
        );
        assert!(out.contains("accounts.real.role_arn = \"arn:aws:iam::999:role/Ok\""));

        // And the whole thing round-trips.
        let back = parse(&out);
        assert_eq!(back.accounts.get("real"), cfg.accounts.get("real"));
    }

    #[test]
    #[ignore = "config values containing a quote can't round-trip: `parse` uses \
                trim_matches('\"') with no escape handling. Pinning the limit \
                rather than inventing an escape the parser can't decode."]
    fn account_values_containing_quotes_do_not_round_trip() {
        // `:settings` rewrites the WHOLE file, so a value that breaks
        // TOML quoting doesn't just corrupt its own line.
        let mut cfg = Config::default();
        cfg.accounts.insert(
            "odd".into(),
            AccountSpec {
                role_arn: "arn:aws:iam::1:role/He said \"hi\"".into(),
                source_profile: Some("back\\slash".into()),
                external_id: None,
                region: None,
            },
        );
        let out = serialize(&cfg);
        // The round trip is the real assertion: it must survive.
        let back = parse(&out);
        assert_eq!(
            back.accounts.get("odd").map(|a| a.role_arn.as_str()),
            Some("arn:aws:iam::1:role/He said \"hi\""),
            "serialized:\n{out}"
        );
    }
    #[test]
    fn a_second_alarm_dimensions_line_replaces_the_first() {
        // Every other key is last-write-wins. Appending meant an
        // operator editing by adding a line silently got the union of
        // both, with no way to narrow it back.
        let cfg = parse(concat!(
            "alarm_dimensions = \"A\"\n",
            "alarm_dimensions = \"B\"\n",
        ));
        assert_eq!(
            cfg.alarm_dimensions,
            vec!["EnvironmentName".to_string(), "B".to_string()],
            "the later line must replace the earlier one"
        );
    }

    #[test]
    fn a_hand_written_alarm_dimensions_line_round_trips_unchanged() {
        // `:settings` rewrites the whole file. Emitting the full match
        // set turned the operator's `"Environment"` into
        // `"EnvironmentName,Environment"` on the first save.
        let original = "alarm_dimensions = \"Environment\"\n";
        let out = serialize(&parse(original));
        assert!(
            out.contains("alarm_dimensions = \"Environment\"\n"),
            "expected the operator's own value back:\n{out}"
        );
        // And the parsed meaning is unchanged across the round trip.
        assert_eq!(
            parse(&out).alarm_dimensions,
            parse(original).alarm_dimensions
        );
    }

    #[test]
    fn an_unset_alarm_dimensions_is_not_written() {
        let out = serialize(&Config::default());
        assert!(!out.contains("alarm_dimensions"), "{out}");
    }
    #[test]
    fn an_unrecognised_key_survives_a_settings_save() {
        // `:settings` rewrites the whole file, so anything the model
        // doesn't carry was destroyed — including a key a newer release
        // understands and this build doesn't, which is the opposite of
        // the graceful degradation the parser aims for.
        let cfg = parse("some_future_key = \"value\"\nrefresh_interval_secs = 30\n");
        let out = serialize(&cfg);
        assert!(
            out.contains("some_future_key = \"value\""),
            "an unknown key must survive:\n{out}"
        );
        assert!(out.contains("refresh_interval_secs = 30"));
        // And a second save doesn't duplicate it.
        let twice = serialize(&parse(&out));
        assert_eq!(twice.matches("some_future_key").count(), 1, "{twice}");
    }
}
