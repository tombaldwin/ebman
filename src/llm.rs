//! LLM-backed explanations for lint issues.
//!
//! `ebman explain ISSUE_ID` and the TUI `:explain EBL001` dispatch
//! turn a structured `lint::Issue` (rule_id + title + detail +
//! suggestion + fields) into operator-readable next steps via a
//! configured Provider. Two providers ship in v1:
//!
//! - **Anthropic** (Claude API) — `POST /v1/messages` with
//!   `x-api-key` header. Default model `claude-haiku-4-5` —
//!   cheap (~$0.001 per explanation), fast (<2s p50), and sized
//!   right for the structured-Q&A shape we're feeding it.
//! - **Ollama** (local) — `POST /api/generate` against the local
//!   Ollama HTTP server. Operators on locked-down corp networks
//!   that can't reach api.anthropic.com get a local-model path.
//!
//! Both providers consume the same prompt template ([`build_prompt`])
//! so swapping providers doesn't change response quality
//! materially — what changes is where the network call goes.
//!
//! ## Consent gate
//!
//! Opt-in via `[explain] enabled = true` in `config.toml`. Off by
//! default. Presence of `ANTHROPIC_API_KEY` is *not* implicit
//! consent — security-conscious orgs that export API keys for
//! other tools shouldn't have ebman silently start making outbound
//! calls. The error message points the operator at the config
//! edit + env var when they invoke `ebman explain` without
//! configuring it.
//!
//! ## Caching
//!
//! Responses are cached by `SHA256(rule_id || serialized_fields)`
//! to `~/.cache/ebman/explain/{key}.txt`. CI loops running
//! `ebman lint --json | jq -r ... | xargs -I {} ebman explain {}`
//! won't burn API calls on identical issues. `--no-cache` skips
//! the read (forces a fresh call) AND skips the write. There's no
//! TTL — operators can `rm` the directory to force a global
//! refresh.

use color_eyre::eyre::{eyre, Result, WrapErr};
use sha2::Digest;

use crate::lint::Issue;

/// Resolved settings for the explain feature. Built from
/// [`crate::config::Config`] at CLI / TUI invocation time. None of
/// the fields are `Option` — defaults are filled by [`Settings::from_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Master switch. Operators must explicitly opt in via
    /// `[explain] enabled = true` — presence of an API key in
    /// the env var alone is not sufficient. Defaults to false.
    pub enabled: bool,
    /// Provider key — `"anthropic"` or `"ollama"`. Other values
    /// produce a clear error rather than falling through to a
    /// default; explicit-is-better-than-implicit for LLM calls.
    pub provider: String,
    /// Model identifier. For Anthropic: e.g. `claude-haiku-4-5`,
    /// `claude-sonnet-4-6`, `claude-opus-4-7`. For Ollama: the
    /// local model name (`llama3.2`, `mistral`, etc).
    pub model: String,
    /// Env-var name to read the API key from (Anthropic provider).
    /// Defaults to `ANTHROPIC_API_KEY`. Operators with multiple
    /// keys in different env-var names can point at a different
    /// one without exporting again.
    pub api_key_env: String,
    /// HTTP base URL for the Ollama provider. Defaults to the
    /// standard local address; remote Ollama setups can point at
    /// a non-localhost host.
    pub ollama_url: String,
    /// Soft cap on output tokens. Generous default for the
    /// explanation shape — typical responses are 200-400 tokens.
    pub max_tokens: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
            ollama_url: "http://localhost:11434".into(),
            max_tokens: 1024,
        }
    }
}

impl Settings {
    /// Write the settings back into a `Config` struct (the reverse
    /// of [`Settings::from_config`]). Empty-string sentinels are used
    /// for non-defaults so the existing serializer in
    /// `config::serialize` (which skips empty strings + `enabled =
    /// false` + `max_tokens = 0`) emits the config.toml lines only
    /// when the operator has actually configured something.
    pub fn write_to_config(&self, cfg: &mut crate::config::Config) {
        let default = Self::default();
        cfg.explain_enabled = self.enabled;
        cfg.explain_provider = if self.provider == default.provider {
            String::new()
        } else {
            self.provider.clone()
        };
        cfg.explain_model = if self.model == default.model {
            String::new()
        } else {
            self.model.clone()
        };
        cfg.explain_api_key_env = if self.api_key_env == default.api_key_env {
            String::new()
        } else {
            self.api_key_env.clone()
        };
        cfg.explain_ollama_url = if self.ollama_url == default.ollama_url {
            String::new()
        } else {
            self.ollama_url.clone()
        };
        cfg.explain_max_tokens = if self.max_tokens == default.max_tokens {
            0
        } else {
            self.max_tokens
        };
    }

    /// Build resolved settings from a [`crate::config::Config`].
    /// Defaults match the values documented above. The merge is
    /// total — every field is filled — so callers don't need to
    /// handle `Option`s downstream.
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        Self {
            enabled: cfg.explain_enabled,
            provider: if cfg.explain_provider.is_empty() {
                "anthropic".into()
            } else {
                cfg.explain_provider.clone()
            },
            model: if cfg.explain_model.is_empty() {
                "claude-haiku-4-5".into()
            } else {
                cfg.explain_model.clone()
            },
            api_key_env: if cfg.explain_api_key_env.is_empty() {
                "ANTHROPIC_API_KEY".into()
            } else {
                cfg.explain_api_key_env.clone()
            },
            ollama_url: if cfg.explain_ollama_url.is_empty() {
                "http://localhost:11434".into()
            } else {
                cfg.explain_ollama_url.clone()
            },
            max_tokens: if cfg.explain_max_tokens == 0 {
                1024
            } else {
                cfg.explain_max_tokens
            },
        }
    }
}

/// Compose the user-facing prompt sent to the provider. The shape
/// is deliberately structured (rule id + title + detail +
/// suggestion + fields) — same shape the lint engine emits. Pure
/// so we can unit-test the wording without an HTTP roundtrip.
pub fn build_prompt(issue: &Issue) -> String {
    let mut p = String::new();
    p.push_str("You are helping an AWS Elastic Beanstalk operator understand and remediate a ");
    p.push_str("diagnostic issue. Explain in 4-8 sentences what the issue means, why it matters ");
    p.push_str("operationally, and what the operator should do next.\n\n");
    p.push_str("Issue:\n");
    p.push_str(&format!("  rule_id: {}\n", issue.rule_id));
    p.push_str(&format!("  severity: {}\n", issue.severity.as_str()));
    if let Some(env) = &issue.env_name {
        p.push_str(&format!("  env: {env}\n"));
    }
    p.push_str(&format!("  title: {}\n", issue.title));
    p.push_str(&format!("  detail: {}\n", issue.detail));
    if let Some(s) = &issue.suggestion {
        p.push_str(&format!("  suggestion: {s}\n"));
    }
    if !issue.fields.is_empty() {
        p.push_str("  fields:\n");
        for (k, v) in &issue.fields {
            p.push_str(&format!("    {k}: {v}\n"));
        }
    }
    p.push_str(
        "\nGround your explanation in the specific values shown. Don't restate the rule_id; ",
    );
    p.push_str("don't apologise; don't add a markdown heading. Plain prose only.");
    p
}

/// Cache key for an issue's response. Stable across runs as long
/// as the rule_id, `env_name`, and the structured `fields` map are
/// the same. We don't include title/detail/suggestion since those
/// can churn across releases without the underlying advice
/// changing.
///
/// `env_name` is part of the key because [`build_prompt`] embeds it
/// in the prompt — Anthropic may personalise the response ("for
/// prod-api, set MinSize to 4 because…"). Two envs hitting the
/// same rule with the same `fields` (e.g. EBL005 single-instance on
/// `dev-a` and `dev-b`) would otherwise share a cache entry and
/// the first env's personalised advice would leak to the second.
pub fn cache_key(issue: &Issue) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(issue.rule_id.as_bytes());
    hasher.update(b"\0");
    if let Some(env) = &issue.env_name {
        hasher.update(env.as_bytes());
    }
    hasher.update(b"\0");
    // Sort-stable iteration via BTreeMap (already used in lint.rs).
    for (k, v) in &issue.fields {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\0");
    }
    let digest = hasher.finalize();
    // 16 hex chars (64 bits) is plenty for cache-key collision
    // — operators won't hit a birthday-attack-grade scale.
    let mut s = String::with_capacity(16);
    for b in &digest[..8] {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Filesystem path the cache entry for `issue` lives at. Caller
/// is responsible for `create_dir_all` of the parent.
pub fn cache_path(issue: &Issue) -> std::path::PathBuf {
    let mut p = crate::util::cache_dir();
    p.push("explain");
    p.push(format!("{}-{}.txt", issue.rule_id, cache_key(issue)));
    p
}

/// Try to read a cached response for `issue`. Returns `None` if
/// the file doesn't exist or can't be read; ignores I/O errors
/// rather than failing the explain call (the cache is an
/// optimisation, not a source of truth).
pub fn read_cache(issue: &Issue) -> Option<String> {
    std::fs::read_to_string(cache_path(issue)).ok()
}

/// Write a response to the cache. Errors are swallowed — a failed
/// write shouldn't prevent the operator seeing their explanation.
pub fn write_cache(issue: &Issue, body: &str) {
    let path = cache_path(issue);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, body);
}

/// Dispatch an explain request to the configured provider.
/// Returns the raw response text on success, an `eyre`-shaped
/// error on transport / parse / consent failure.
pub async fn dispatch(settings: &Settings, prompt: &str) -> Result<String> {
    if !settings.enabled {
        // Surface the right next-step depending on provider. For
        // Anthropic the API-key env var matters; for Ollama there's
        // no key and the operator needs a running local server.
        let next_step = match settings.provider.as_str() {
            "ollama" => format!("ensure Ollama is running at `{}`", settings.ollama_url),
            _ => format!("ensure `{}` is exported", settings.api_key_env),
        };
        return Err(eyre!(
            "ebman explain: feature is disabled. Set `[explain] enabled = true` in {} and {next_step}. See docs/configuration.md.",
            crate::util::config_file("config.toml").display(),
        ));
    }
    match settings.provider.as_str() {
        "anthropic" => call_anthropic(settings, prompt).await,
        "ollama" => call_ollama(settings, prompt).await,
        other => Err(eyre!(
            "ebman explain: unknown provider '{other}' (supported: anthropic, ollama)"
        )),
    }
}

async fn call_anthropic(settings: &Settings, prompt: &str) -> Result<String> {
    let api_key = std::env::var(&settings.api_key_env).map_err(|_| {
        eyre!(
            "ebman explain: env var `{}` is not set. Export the Anthropic API key first.",
            settings.api_key_env
        )
    })?;
    // Hand-rolled JSON body — short + fixed shape. No serde_json
    // needed (project convention is hand-rolled for outbound).
    let body = format!(
        "{{\"model\":{},\"max_tokens\":{},\"system\":{},\"messages\":[{{\"role\":\"user\",\"content\":{}}}]}}",
        json_str(&settings.model),
        settings.max_tokens,
        json_str(
            "You are a senior AWS Elastic Beanstalk operator. Give concrete, actionable answers \
             in 4-8 sentences. Plain prose, no markdown headings, no apologies."
        ),
        json_str(prompt),
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .wrap_err("explain: building reqwest client failed")?;
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .wrap_err("explain: POST to api.anthropic.com failed")?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .wrap_err("explain: reading response body failed")?;
    if !status.is_success() {
        return Err(eyre!(
            "explain: Anthropic returned {} — body: {}",
            status.as_u16(),
            truncate_for_error(&text, 400)
        ));
    }
    // Parse the response via serde_yml (JSON is a valid YAML
    // subset, so this works without serde_json).
    let parsed: serde_yml::Value =
        serde_yml::from_str(&text).wrap_err("explain: response wasn't valid JSON")?;
    let content = parsed
        .get("content")
        .and_then(|v| v.as_sequence())
        .ok_or_else(|| eyre!("explain: response missing `content` array"))?;
    let mut out = String::new();
    for block in content {
        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                out.push_str(t);
            }
        }
    }
    if out.is_empty() {
        return Err(eyre!(
            "explain: Anthropic returned an empty response (no `text` blocks)"
        ));
    }
    Ok(out)
}

async fn call_ollama(settings: &Settings, prompt: &str) -> Result<String> {
    // Ollama is unauthenticated by default — no API key plumbing.
    let url = format!("{}/api/generate", settings.ollama_url.trim_end_matches('/'));
    let body = format!(
        "{{\"model\":{},\"prompt\":{},\"stream\":false}}",
        json_str(&settings.model),
        json_str(prompt),
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .wrap_err("explain: building reqwest client failed")?;
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .wrap_err_with(|| format!("explain: POST to {url} failed (Ollama running locally?)"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .wrap_err("explain: reading response body failed")?;
    if !status.is_success() {
        return Err(eyre!(
            "explain: Ollama returned {} — body: {}",
            status.as_u16(),
            truncate_for_error(&text, 400)
        ));
    }
    let parsed: serde_yml::Value =
        serde_yml::from_str(&text).wrap_err("explain: Ollama response wasn't valid JSON")?;
    let out = parsed
        .get("response")
        .and_then(|v| v.as_str())
        .ok_or_else(|| eyre!("explain: Ollama response missing `response` field"))?;
    Ok(out.to_string())
}

// Hand-rolled JSON request body for the LLM provider. Local alias
// for `crate::util::json_string` so call-site rewrites are
// unnecessary; semantics identical.
use crate::util::json_string as json_str;

fn truncate_for_error(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sample_issue() -> Issue {
        let mut fields = BTreeMap::new();
        fields.insert("policy".into(), "AllAtOnce".into());
        fields.insert("max_size".into(), "4".into());
        Issue {
            rule_id: "EBL001".into(),
            severity: crate::lint::Severity::Warn,
            env_name: Some("prod-api".into()),
            title: "AllAtOnce on 4-instance env".into(),
            detail: "100% capacity loss during deploys".into(),
            suggestion: Some(":deployment-policy Rolling".into()),
            fields,
        }
    }

    #[test]
    fn build_prompt_includes_rule_id_title_fields() {
        let p = build_prompt(&sample_issue());
        assert!(p.contains("EBL001"));
        assert!(p.contains("AllAtOnce on 4-instance env"));
        assert!(p.contains("max_size: 4"));
        assert!(p.contains("policy: AllAtOnce"));
        assert!(p.contains("severity: warn"));
        assert!(p.contains("env: prod-api"));
    }

    #[test]
    fn cache_key_is_stable_across_calls() {
        let issue = sample_issue();
        let a = cache_key(&issue);
        let b = cache_key(&issue);
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn cache_key_differs_by_rule_id() {
        let mut i1 = sample_issue();
        let mut i2 = sample_issue();
        i2.rule_id = "EBL004".into();
        assert_ne!(cache_key(&i1), cache_key(&i2));
        // Title change DOES NOT affect cache key (advice is keyed on rule_id+fields).
        i1.title = "Different wording".into();
        assert_eq!(cache_key(&i1), cache_key(&sample_issue()));
    }

    #[test]
    fn cache_key_differs_by_field_values() {
        let i1 = sample_issue();
        let mut i2 = sample_issue();
        i2.fields.insert("max_size".into(), "8".into());
        assert_ne!(cache_key(&i1), cache_key(&i2));
    }

    #[test]
    fn cache_key_differs_by_env_name() {
        // Same rule, same fields, two envs — the env_name appears
        // in the prompt (`build_prompt` line that emits `env: ...`),
        // so the LLM may personalise the response. The cache key
        // MUST distinguish so env-a's response doesn't leak to env-b.
        let mut i_a = sample_issue();
        i_a.env_name = Some("prod-a".into());
        let mut i_b = sample_issue();
        i_b.env_name = Some("prod-b".into());
        assert_ne!(cache_key(&i_a), cache_key(&i_b));
    }

    #[test]
    fn cache_key_handles_missing_env_name() {
        let mut i_none = sample_issue();
        i_none.env_name = None;
        // Doesn't panic; produces a stable 16-hex key.
        let k = cache_key(&i_none);
        assert_eq!(k.len(), 16);
        // Still differs from the sample-with-env-name case.
        assert_ne!(cache_key(&i_none), cache_key(&sample_issue()));
    }

    #[test]
    fn config_with_explicit_defaults_collapses_on_round_trip() {
        // Documented operator-UX wart in 0.15: if the operator
        // wrote `explain.provider = "anthropic"` (matching the
        // default) into config.toml, `Settings::from_config` resolves
        // it to "anthropic", then `write_to_config` collapses it
        // back to "" (empty-string sentinel for default), and the
        // serializer skips the line. Net effect: `:settings save`
        // removes the explicit-but-default line from disk. Semantics
        // are preserved (next load re-applies the default), but the
        // file churns. Pinning the behaviour here so future tweaks
        // notice; revisit if operators complain about the disappearing
        // lines (0.16+).
        let cfg_with_explicit = crate::config::Config {
            explain_provider: "anthropic".into(),
            explain_max_tokens: 1024,
            ..crate::config::Config::default()
        };
        let s = Settings::from_config(&cfg_with_explicit);
        let mut cfg_after = crate::config::Config::default();
        s.write_to_config(&mut cfg_after);
        // Both fields explicitly equal-default → collapsed to sentinel.
        assert_eq!(cfg_after.explain_provider, "");
        assert_eq!(cfg_after.explain_max_tokens, 0);
        // Non-default values still round-trip cleanly.
        let cfg_with_override = crate::config::Config {
            explain_provider: "ollama".into(),
            explain_max_tokens: 2048,
            ..crate::config::Config::default()
        };
        let s2 = Settings::from_config(&cfg_with_override);
        let mut cfg_after2 = crate::config::Config::default();
        s2.write_to_config(&mut cfg_after2);
        assert_eq!(cfg_after2.explain_provider, "ollama");
        assert_eq!(cfg_after2.explain_max_tokens, 2048);
    }

    #[test]
    fn settings_round_trip_through_config() {
        // App-side path: load Settings → write_to_config → Settings
        // again must be identity. Default values intentionally
        // collapse to empty-string sentinels in Config (so the
        // serializer skips them), but Settings::from_config restores
        // the defaults on the way back. Net round-trip is identity
        // on a default Settings.
        let s = Settings::default();
        let mut cfg = crate::config::Config::default();
        s.write_to_config(&mut cfg);
        let restored = Settings::from_config(&cfg);
        assert_eq!(restored, s);

        // Non-default: round-trips losslessly.
        let s2 = Settings {
            enabled: true,
            provider: "ollama".into(),
            model: "llama3.2".into(),
            api_key_env: "MY_KEY".into(),
            ollama_url: "http://10.0.0.5:11434".into(),
            max_tokens: 2048,
        };
        let mut cfg2 = crate::config::Config::default();
        s2.write_to_config(&mut cfg2);
        let restored2 = Settings::from_config(&cfg2);
        assert_eq!(restored2, s2);
    }

    #[test]
    fn settings_from_default_config_is_disabled() {
        let cfg = crate::config::Config::default();
        let s = Settings::from_config(&cfg);
        assert!(!s.enabled);
        assert_eq!(s.provider, "anthropic");
        assert_eq!(s.model, "claude-haiku-4-5");
        assert_eq!(s.api_key_env, "ANTHROPIC_API_KEY");
        assert_eq!(s.max_tokens, 1024);
    }

    #[test]
    fn settings_from_config_honours_overrides() {
        let cfg = crate::config::Config {
            explain_enabled: true,
            explain_provider: "ollama".into(),
            explain_model: "llama3.2".into(),
            explain_ollama_url: "http://192.0.2.1:11434".into(),
            explain_max_tokens: 2048,
            ..crate::config::Config::default()
        };
        let s = Settings::from_config(&cfg);
        assert!(s.enabled);
        assert_eq!(s.provider, "ollama");
        assert_eq!(s.model, "llama3.2");
        assert_eq!(s.ollama_url, "http://192.0.2.1:11434");
        assert_eq!(s.max_tokens, 2048);
    }

    #[tokio::test]
    async fn dispatch_refuses_when_disabled() {
        let s = Settings {
            enabled: false,
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
            ollama_url: "http://localhost:11434".into(),
            max_tokens: 1024,
        };
        let result = dispatch(&s, "prompt").await;
        let err = result.expect_err("dispatch should refuse when disabled");
        assert!(err.to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn dispatch_refuses_unknown_provider() {
        let s = Settings {
            enabled: true,
            provider: "magic-llm-9000".into(),
            model: "x".into(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
            ollama_url: "http://localhost:11434".into(),
            max_tokens: 1024,
        };
        let err = dispatch(&s, "prompt").await.expect_err("should refuse");
        assert!(err.to_string().contains("unknown provider"));
    }

    #[test]
    fn json_str_escapes_quotes_and_control_chars() {
        assert_eq!(json_str("hello"), "\"hello\"");
        assert_eq!(json_str("with \"quotes\""), "\"with \\\"quotes\\\"\"");
        assert_eq!(json_str("a\nb"), "\"a\\nb\"");
        assert_eq!(json_str("a\\b"), "\"a\\\\b\"");
        // Round-trip via serde_yml since JSON is a YAML subset.
        let s = "with \"quotes\" and \n newlines and \\ backslashes";
        let escaped = json_str(s);
        let parsed: String = serde_yml::from_str(&escaped).expect("round-trip");
        assert_eq!(parsed, s);
    }

    #[test]
    fn truncate_for_error_keeps_short_strings() {
        assert_eq!(truncate_for_error("short", 10), "short");
    }

    #[test]
    fn truncate_for_error_clips_long_strings() {
        let long = "a".repeat(500);
        let t = truncate_for_error(&long, 50);
        assert_eq!(t.len(), 50 + 3); // 50 chars + "..."
        assert!(t.ends_with("..."));
    }
}
