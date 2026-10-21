//! Which CloudWatch alarms belong to an environment.
//!
//! One rule for every surface that asks: the fleet view's ALARM column,
//! the Health and App tabs, `:alarms`, `:why`, `ebman envs` and the MCP
//! tools. It lived in `app_tab.rs` because the App tab's `alarm_prefix`
//! introduced it, but most of its callers have nothing to do with that
//! tab.

/// Which alarms belong to an environment: a dimension naming it, or a
/// name starting with the env's `app.ENV.alarm_prefix`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AlarmMatch {
    /// Dimension names that identify an env (`alarm_dimensions`).
    pub dimension_names: Vec<String>,
    pub name_prefix: Option<String>,
    /// Other environments whose names start with the prefix: an alarm
    /// named for one of them is theirs, not this env's. `Shop-prod-`
    /// would otherwise take `Shop-prod-wk-…`, the worker's alarms.
    pub shadowed_by: Vec<String>,
}

impl AlarmMatch {
    /// The match for `env`: the configured dimension names, and that
    /// env's App tab alarm prefix if it has one.
    ///
    /// `fleet` is every environment name ebman knows of, so a prefix
    /// can give way to a longer env name it also covers.
    pub(crate) fn for_env<'a>(
        dimension_names: &[String],
        app_tabs: &crate::app_tab::AppTabs,
        env: &str,
        fleet: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let name_prefix = app_tabs.envs.get(env).and_then(|s| s.alarm_prefix.clone());
        let shadowed_by = match &name_prefix {
            Some(p) if !p.is_empty() => fleet
                .into_iter()
                .filter(|other| *other != env && other.starts_with(p.as_str()))
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        Self {
            dimension_names: dimension_names.to_vec(),
            name_prefix,
            shadowed_by,
        }
    }

    pub(crate) fn matches(
        &self,
        env: &str,
        name: &str,
        dimensions: &[(Option<&str>, Option<&str>)],
    ) -> bool {
        let by_name = self
            .name_prefix
            .as_deref()
            .is_some_and(|p| !p.is_empty() && name.starts_with(p))
            && !self
                .shadowed_by
                .iter()
                .any(|other| alarm_names_env(name, other));
        let by_dimension = dimensions.iter().any(|(n, v)| {
            *v == Some(env) && n.is_some_and(|n| self.dimension_names.iter().any(|w| w == n))
        });
        by_name || by_dimension
    }
}

/// The names of the `firing` alarms that belong to `env` under
/// `matching`: the fleet view's per-env alarm count, decided by the same
/// rule as the Health and App tabs' alarm lists so the two never
/// disagree about an env.
pub(crate) fn firing_for(
    matching: &AlarmMatch,
    env: &str,
    firing: &[crate::aws::FiringAlarm],
) -> Vec<String> {
    firing
        .iter()
        .filter(|a| {
            let dims: Vec<(Option<&str>, Option<&str>)> = a
                .dimensions
                .iter()
                .map(|(n, v)| (Some(n.as_str()), Some(v.as_str())))
                .collect();
            matching.matches(env, &a.name, &dims)
        })
        .map(|a| a.name.clone())
        .collect()
}

/// Whether alarm `name` is named for environment `env`: the env's name
/// itself, or it followed by `-`. A bare prefix would take
/// `Shop-prod-wkld-cpu` as the worker `Shop-prod-wk`'s.
pub(crate) fn alarm_names_env(name: &str, env: &str) -> bool {
    name == env
        || name
            .strip_prefix(env)
            .is_some_and(|rest| rest.starts_with('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firing_for_takes_an_envs_alarms_by_dimension_and_prefix_only() {
        let firing = |name: &str, dims: &[(&str, &str)]| crate::aws::FiringAlarm {
            name: name.into(),
            dimensions: dims
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
        };
        let alarms = vec![
            firing("by-dimension", &[("EnvironmentName", "shop-prod")]),
            firing("shop-prod-5xx", &[("LoadBalancer", "app/x")]),
            // Another env's: by its dimension, and by a name the
            // prefix covers but that names the longer env.
            firing("other", &[("EnvironmentName", "shop-staging")]),
            firing("shop-prod-wk-depth", &[("QueueName", "q")]),
        ];
        let matching = AlarmMatch {
            dimension_names: vec!["EnvironmentName".into()],
            name_prefix: Some("shop-prod-".into()),
            shadowed_by: vec!["shop-prod-wk".into()],
        };
        assert_eq!(
            firing_for(&matching, "shop-prod", &alarms),
            vec!["by-dimension", "shop-prod-5xx"]
        );
        // No prefix: the dimension alone.
        let matching = AlarmMatch {
            name_prefix: None,
            shadowed_by: Vec::new(),
            ..matching
        };
        assert_eq!(
            firing_for(&matching, "shop-prod", &alarms),
            vec!["by-dimension"]
        );
    }
}
