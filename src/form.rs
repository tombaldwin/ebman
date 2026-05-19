//! Modal forms — reusable scaffolding for multi-field option-settings
//! editors. The first concrete consumer is `:capacity` (MinSize / MaxSize /
//! InstanceType / Cooldown); the Network / Security pickers will follow
//! once we add a multi-select field kind.
//!
//! Each [`Form`] carries its own field list, cursor position, and lifecycle
//! state. Field values are always stored as `String` regardless of kind —
//! [`FieldKind`] tells the renderer + input handler how to interpret them.
//! Submission converts the field values into `(namespace, option_name,
//! value)` triples and dispatches via the shared
//! `spawn_option_settings_update` helper.

/// One modal-form session. Owned by the [`App`] while [`crate::app::Mode::Form`]
/// is active; replaced wholesale on a new `:command` that opens a form.
#[derive(Debug, Clone)]
pub struct Form {
    pub title: String,
    pub fields: Vec<FormField>,
    pub cursor: usize,
    pub state: FormState,
    /// What to do with the field values on submit. Determines which AWS
    /// call dispatches and how each field maps to an option setting.
    pub submit: FormSubmit,
    /// Toast / status target on successful submit (e.g. "capacity update").
    pub summary: String,
    /// Env name the form was opened against. Captured at open-time so a
    /// later cursor move on the main table doesn't redirect the submit.
    pub env_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormState {
    /// Fetching the pre-fill values from AWS.
    Loading,
    /// Form is interactive.
    Ready,
    /// Submit dispatched; AWS round-trip in flight.
    Submitting,
}

#[derive(Debug, Clone)]
pub struct FormField {
    /// Stable identifier used by `FormSubmit::OptionSettings` to map a
    /// field to a `(namespace, option_name)` triple. Not shown to the user.
    pub key: String,
    /// User-visible label rendered to the left of the input.
    pub label: String,
    /// Current value as a string. The renderer + input handler interpret it
    /// per [`FieldKind`].
    pub value: String,
    pub kind: FieldKind,
    /// Optional one-line hint rendered dimmed under the field.
    pub help: Option<String>,
    /// Set by [`Form::validate`] if the current value fails its constraints.
    /// Rendered in red under the field.
    pub error: Option<String>,
}

// Boolean + Select are reserved for forms that haven't shipped yet (the
// :capacity MVP only uses Text + Integer). Suppress the dead-code lint
// at the enum level; the variants are referenced by the renderer and key
// handler match arms which gives them real call sites once a form uses
// them.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    /// Free-form text. No validation on shape.
    Text,
    /// Integer (signed). Validates as an `i64`; allows empty for "unset".
    Integer {
        min: Option<i64>,
        max: Option<i64>,
        allow_empty: bool,
    },
    /// Boolean. Value is "true" or "false"; space toggles, t/f set directly.
    Boolean,
    /// Pick exactly one from a fixed list. Value is the selected option's
    /// string (so submit doesn't need to look the index back up).
    Select { options: Vec<String> },
}

/// What the form does on submit.
#[derive(Debug, Clone)]
pub enum FormSubmit {
    /// Every field maps to one `(namespace, option_name)` pair. Empty
    /// integer fields (when `allow_empty`) are dropped from the update so
    /// EB keeps its existing value.
    OptionSettings {
        /// Map from field `key` → `(namespace, option_name)`. Order is
        /// preserved when building the to_set vec.
        mappings: Vec<(String, String, String)>,
    },
    /// Writes back to `~/.config/ebman/config.toml` and updates the live
    /// `App.cfg` in place. Field `key`s must match `Config` field names so
    /// the submit handler can update the right slots without per-field
    /// branching. No AWS round-trip.
    LocalConfig,
}

impl Form {
    /// Build a form in the `Loading` state. Caller spawns the pre-fill fetch
    /// and replaces `state` with `Ready` once values land.
    pub fn loading(
        title: impl Into<String>,
        env_name: impl Into<String>,
        summary: impl Into<String>,
        fields: Vec<FormField>,
        submit: FormSubmit,
    ) -> Self {
        Self {
            title: title.into(),
            fields,
            cursor: 0,
            state: FormState::Loading,
            submit,
            summary: summary.into(),
            env_name: env_name.into(),
        }
    }

    /// Convenience for the field at the cursor position.
    pub fn current_field(&self) -> Option<&FormField> {
        self.fields.get(self.cursor)
    }

    pub fn current_field_mut(&mut self) -> Option<&mut FormField> {
        self.fields.get_mut(self.cursor)
    }

    /// Move the cursor forward or backward through the field list. Wraps.
    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.fields.len() as isize;
        if len <= 0 {
            return;
        }
        let mut next = self.cursor as isize + delta;
        next = ((next % len) + len) % len;
        self.cursor = next as usize;
    }

    /// Run per-field validators. Stores errors on each `FormField.error` and
    /// returns `Ok(())` only if every field passes. Pure — no side effects
    /// beyond the field error strings.
    pub fn validate(&mut self) -> Result<(), Vec<usize>> {
        let mut failing: Vec<usize> = Vec::new();
        for (i, field) in self.fields.iter_mut().enumerate() {
            field.error = match validate_field(&field.value, &field.kind) {
                Ok(()) => None,
                Err(msg) => {
                    failing.push(i);
                    Some(msg)
                }
            };
        }
        if failing.is_empty() {
            Ok(())
        } else {
            Err(failing)
        }
    }

    /// Build `(to_set, to_remove)` slices suitable for
    /// `aws::update_env_option_settings`. Skips empty integer fields when
    /// the field's `allow_empty` is true — those imply "don't touch this".
    /// Pre-validation is the caller's responsibility.
    #[allow(clippy::type_complexity)]
    pub fn to_option_settings(&self) -> (Vec<(String, String, String)>, Vec<(String, String)>) {
        let mappings = match &self.submit {
            FormSubmit::OptionSettings { mappings } => mappings,
            FormSubmit::LocalConfig => return (Vec::new(), Vec::new()),
        };
        let mut to_set: Vec<(String, String, String)> = Vec::new();
        // No field kind currently maps to "remove this setting"; the field's
        // empty integer + allow_empty case is handled by skipping the field
        // entirely (leave EB's value alone), not by explicit remove.
        let to_remove: Vec<(String, String)> = Vec::new();
        for (key, ns, opt) in mappings {
            let Some(field) = self.fields.iter().find(|f| &f.key == key) else {
                continue;
            };
            match &field.kind {
                FieldKind::Integer {
                    allow_empty: true, ..
                } if field.value.trim().is_empty() => {
                    // Operator left it blank — leave the AWS-side value alone.
                    continue;
                }
                _ => {}
            }
            to_set.push((ns.clone(), opt.clone(), field.value.clone()));
        }
        (to_set, to_remove)
    }

    /// Apply the form's field values onto a [`crate::config::Config`]. Field
    /// keys are matched to config slots by string; unknown keys are silently
    /// ignored so callers can carry extra UI-only fields without breaking
    /// this. Pure — does not touch disk. Pre-validation is the caller's
    /// responsibility.
    ///
    /// Comma-separated list fields (`extra_regions`, `required_tags`) split
    /// on commas and trim each entry; empty entries are dropped.
    pub fn apply_to_config(&self, base: &crate::config::Config) -> crate::config::Config {
        let mut cfg = base.clone();
        for field in &self.fields {
            let value = field.value.trim();
            match field.key.as_str() {
                "theme" => cfg.theme = value.to_string(),
                "icons" => cfg.icons = value.to_string(),
                "refresh_interval_secs" => {
                    if let Ok(n) = value.parse::<u64>() {
                        if n > 0 {
                            cfg.refresh_interval = std::time::Duration::from_secs(n);
                        }
                    }
                }
                "redact_default" => {
                    cfg.redact_default = match value {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    };
                }
                "grouped_default" => {
                    cfg.grouped_default = match value {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    };
                }
                "notify_bell" => {
                    cfg.notify_bell = matches!(value, "true");
                }
                "required_tags" => {
                    cfg.required_tags = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "extra_regions" => {
                    cfg.extra_regions = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "webhook_url" => {
                    cfg.webhook_url = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
                _ => {}
            }
        }
        cfg
    }
}

impl FormField {
    pub fn text(
        key: impl Into<String>,
        label: impl Into<String>,
        help: Option<impl Into<String>>,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            value: String::new(),
            kind: FieldKind::Text,
            help: help.map(Into::into),
            error: None,
        }
    }

    pub fn integer(
        key: impl Into<String>,
        label: impl Into<String>,
        help: Option<impl Into<String>>,
        min: Option<i64>,
        max: Option<i64>,
        allow_empty: bool,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            value: String::new(),
            kind: FieldKind::Integer {
                min,
                max,
                allow_empty,
            },
            help: help.map(Into::into),
            error: None,
        }
    }

    #[allow(dead_code)]
    pub fn boolean(
        key: impl Into<String>,
        label: impl Into<String>,
        help: Option<impl Into<String>>,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            value: "false".into(),
            kind: FieldKind::Boolean,
            help: help.map(Into::into),
            error: None,
        }
    }

    pub fn select(
        key: impl Into<String>,
        label: impl Into<String>,
        options: Vec<String>,
        help: Option<impl Into<String>>,
    ) -> Self {
        let value = options.first().cloned().unwrap_or_default();
        Self {
            key: key.into(),
            label: label.into(),
            value,
            kind: FieldKind::Select { options },
            help: help.map(Into::into),
            error: None,
        }
    }
}

/// Pure: returns `Ok(())` if `value` is acceptable for `kind`, else the
/// human-readable error to render below the field.
pub fn validate_field(value: &str, kind: &FieldKind) -> Result<(), String> {
    match kind {
        FieldKind::Text => Ok(()),
        FieldKind::Integer {
            min,
            max,
            allow_empty,
        } => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                if *allow_empty {
                    return Ok(());
                }
                return Err("required".into());
            }
            let n: i64 = trimmed
                .parse()
                .map_err(|_| format!("not an integer: '{trimmed}'"))?;
            if let Some(lo) = min {
                if n < *lo {
                    return Err(format!("must be ≥ {lo}"));
                }
            }
            if let Some(hi) = max {
                if n > *hi {
                    return Err(format!("must be ≤ {hi}"));
                }
            }
            Ok(())
        }
        FieldKind::Boolean => {
            if matches!(value, "true" | "false") {
                Ok(())
            } else {
                Err(format!("must be true or false (got '{value}')"))
            }
        }
        FieldKind::Select { options } => {
            if options.iter().any(|o| o == value) {
                Ok(())
            } else {
                Err(format!("not in list: '{value}'"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int_field(key: &str, allow_empty: bool) -> FormField {
        FormField::integer(
            key,
            "Some Int",
            None::<String>,
            Some(0),
            Some(100),
            allow_empty,
        )
    }

    #[test]
    fn validate_integer_happy_path_and_bounds() {
        assert!(validate_field(
            "5",
            &FieldKind::Integer {
                min: Some(0),
                max: Some(10),
                allow_empty: false
            }
        )
        .is_ok());
        assert!(validate_field(
            "-1",
            &FieldKind::Integer {
                min: Some(0),
                max: Some(10),
                allow_empty: false
            }
        )
        .is_err());
        assert!(validate_field(
            "11",
            &FieldKind::Integer {
                min: Some(0),
                max: Some(10),
                allow_empty: false
            }
        )
        .is_err());
    }

    #[test]
    fn validate_integer_empty_respects_allow_empty() {
        assert!(validate_field(
            "",
            &FieldKind::Integer {
                min: None,
                max: None,
                allow_empty: true
            }
        )
        .is_ok());
        assert!(validate_field(
            "",
            &FieldKind::Integer {
                min: None,
                max: None,
                allow_empty: false
            }
        )
        .is_err());
    }

    #[test]
    fn validate_boolean_accepts_only_true_or_false() {
        assert!(validate_field("true", &FieldKind::Boolean).is_ok());
        assert!(validate_field("false", &FieldKind::Boolean).is_ok());
        assert!(validate_field("yes", &FieldKind::Boolean).is_err());
        assert!(validate_field("", &FieldKind::Boolean).is_err());
    }

    #[test]
    fn validate_select_requires_membership() {
        let kind = FieldKind::Select {
            options: vec!["a".into(), "b".into()],
        };
        assert!(validate_field("a", &kind).is_ok());
        assert!(validate_field("c", &kind).is_err());
    }

    #[test]
    fn form_move_cursor_wraps_both_directions() {
        let mut f = Form::loading(
            "t",
            "env",
            "sum",
            vec![
                int_field("a", false),
                int_field("b", false),
                int_field("c", false),
            ],
            FormSubmit::OptionSettings { mappings: vec![] },
        );
        f.cursor = 0;
        f.move_cursor(1);
        assert_eq!(f.cursor, 1);
        f.move_cursor(-2);
        assert_eq!(f.cursor, 2);
        f.move_cursor(1);
        assert_eq!(f.cursor, 0);
    }

    #[test]
    fn form_validate_collects_failing_field_indexes() {
        let mut f = Form::loading(
            "t",
            "env",
            "sum",
            vec![
                FormField {
                    key: "a".into(),
                    label: "A".into(),
                    value: "5".into(),
                    kind: FieldKind::Integer {
                        min: Some(0),
                        max: Some(10),
                        allow_empty: false,
                    },
                    help: None,
                    error: None,
                },
                FormField {
                    key: "b".into(),
                    label: "B".into(),
                    value: "not-a-number".into(),
                    kind: FieldKind::Integer {
                        min: None,
                        max: None,
                        allow_empty: false,
                    },
                    help: None,
                    error: None,
                },
            ],
            FormSubmit::OptionSettings { mappings: vec![] },
        );
        let err = f.validate().unwrap_err();
        assert_eq!(err, vec![1]);
        assert!(f.fields[0].error.is_none());
        assert!(f.fields[1].error.is_some());
    }

    #[test]
    fn apply_to_config_updates_known_fields() {
        use crate::config::Config;
        let base = Config::default();
        let f = Form {
            title: "settings".into(),
            fields: vec![
                FormField {
                    key: "theme".into(),
                    label: "Theme".into(),
                    value: "high-contrast".into(),
                    kind: FieldKind::Text,
                    help: None,
                    error: None,
                },
                FormField {
                    key: "icons".into(),
                    label: "Icons".into(),
                    value: "powerline".into(),
                    kind: FieldKind::Text,
                    help: None,
                    error: None,
                },
                FormField {
                    key: "refresh_interval_secs".into(),
                    label: "Refresh".into(),
                    value: "20".into(),
                    kind: FieldKind::Integer {
                        min: Some(1),
                        max: Some(600),
                        allow_empty: false,
                    },
                    help: None,
                    error: None,
                },
                FormField {
                    key: "redact_default".into(),
                    label: "Redact".into(),
                    value: "true".into(),
                    kind: FieldKind::Text,
                    help: None,
                    error: None,
                },
                FormField {
                    key: "notify_bell".into(),
                    label: "Bell".into(),
                    value: "true".into(),
                    kind: FieldKind::Boolean,
                    help: None,
                    error: None,
                },
                FormField {
                    key: "required_tags".into(),
                    label: "Tags".into(),
                    value: "Owner, Env".into(),
                    kind: FieldKind::Text,
                    help: None,
                    error: None,
                },
                FormField {
                    key: "extra_regions".into(),
                    label: "Regions".into(),
                    value: "".into(),
                    kind: FieldKind::Text,
                    help: None,
                    error: None,
                },
                FormField {
                    key: "webhook_url".into(),
                    label: "Webhook".into(),
                    value: "  ".into(),
                    kind: FieldKind::Text,
                    help: None,
                    error: None,
                },
            ],
            cursor: 0,
            state: FormState::Ready,
            submit: FormSubmit::LocalConfig,
            summary: "settings update".into(),
            env_name: "".into(),
        };
        let updated = f.apply_to_config(&base);
        assert_eq!(updated.theme, "high-contrast");
        assert_eq!(updated.icons, "powerline");
        assert_eq!(updated.refresh_interval, std::time::Duration::from_secs(20));
        assert_eq!(updated.redact_default, Some(true));
        assert!(updated.notify_bell);
        assert_eq!(updated.required_tags, vec!["Owner", "Env"]);
        assert!(updated.extra_regions.is_empty());
        // Whitespace-only webhook → None.
        assert!(updated.webhook_url.is_none());
    }

    #[test]
    fn apply_to_config_unknown_keys_are_ignored() {
        use crate::config::Config;
        let base = Config::default();
        let f = Form {
            title: "x".into(),
            fields: vec![FormField {
                key: "this-field-does-not-map".into(),
                label: "x".into(),
                value: "ignored".into(),
                kind: FieldKind::Text,
                help: None,
                error: None,
            }],
            cursor: 0,
            state: FormState::Ready,
            submit: FormSubmit::LocalConfig,
            summary: "x".into(),
            env_name: "".into(),
        };
        let updated = f.apply_to_config(&base);
        assert_eq!(updated.theme, base.theme);
        assert_eq!(updated.icons, base.icons);
    }

    #[test]
    fn local_config_submit_yields_no_option_settings() {
        let f = Form {
            title: "x".into(),
            fields: vec![],
            cursor: 0,
            state: FormState::Ready,
            submit: FormSubmit::LocalConfig,
            summary: "x".into(),
            env_name: "".into(),
        };
        let (set, remove) = f.to_option_settings();
        assert!(set.is_empty());
        assert!(remove.is_empty());
    }

    #[test]
    fn form_to_option_settings_drops_empty_optional_integers() {
        let f = Form {
            title: "t".into(),
            fields: vec![
                FormField {
                    key: "min".into(),
                    label: "min".into(),
                    value: "2".into(),
                    kind: FieldKind::Integer {
                        min: None,
                        max: None,
                        allow_empty: false,
                    },
                    help: None,
                    error: None,
                },
                FormField {
                    key: "cooldown".into(),
                    label: "cooldown".into(),
                    value: "".into(),
                    kind: FieldKind::Integer {
                        min: None,
                        max: None,
                        allow_empty: true,
                    },
                    help: None,
                    error: None,
                },
                FormField {
                    key: "type".into(),
                    label: "type".into(),
                    value: "t3.medium".into(),
                    kind: FieldKind::Text,
                    help: None,
                    error: None,
                },
            ],
            cursor: 0,
            state: FormState::Ready,
            submit: FormSubmit::OptionSettings {
                mappings: vec![
                    ("min".into(), "aws:autoscaling:asg".into(), "MinSize".into()),
                    (
                        "cooldown".into(),
                        "aws:autoscaling:asg".into(),
                        "Cooldown".into(),
                    ),
                    (
                        "type".into(),
                        "aws:autoscaling:launchconfiguration".into(),
                        "InstanceType".into(),
                    ),
                ],
            },
            summary: "sum".into(),
            env_name: "env".into(),
        };
        let (set, remove) = f.to_option_settings();
        assert!(remove.is_empty());
        // cooldown dropped (empty + allow_empty), the other two retained in order.
        assert_eq!(
            set,
            vec![
                ("aws:autoscaling:asg".into(), "MinSize".into(), "2".into(),),
                (
                    "aws:autoscaling:launchconfiguration".into(),
                    "InstanceType".into(),
                    "t3.medium".into(),
                ),
            ]
        );
    }
}
