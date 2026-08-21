//! Getting data *out*: yanking to the clipboard, writing JSON / TSV /
//! Markdown exports, and opening the current selection in the AWS
//! console.

use super::*;

impl App {
    pub(crate) fn yank_cli(&mut self) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(env) = env_opt else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let cmd = build_describe_cli(
            &env.name,
            &self.context.region,
            self.override_profile
                .as_deref()
                .or(self.context.profile.as_deref()),
        );
        match yank(&cmd) {
            Ok(()) => {
                self.status_message = Some("equivalent AWS CLI command copied".into());
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    pub(crate) fn export_json(&mut self) {
        let count = self.cached_filtered.len();
        let mut out = String::from("[\n");
        for (idx, &i) in self.cached_filtered.iter().enumerate() {
            let e = &self.environments[i];
            let cname = if self.redact {
                redact_block(&e.cname)
            } else {
                e.cname.clone()
            };
            let updated = e
                .updated
                .map(|u| format!("\"{}\"", u.to_rfc3339()))
                .unwrap_or_else(|| "null".into());
            out.push_str(&format!(
                "  {{\"name\":\"{}\",\"application\":\"{}\",\"tier\":\"{}\",\"status\":\"{}\",\"health\":\"{}\",\"platform\":\"{}\",\"version\":\"{}\",\"cname\":\"{}\",\"updated\":{}}}",
                json_escape(&e.name),
                json_escape(&e.application),
                json_escape(&e.tier),
                json_escape(&e.status),
                json_escape(&e.health),
                json_escape(&e.platform),
                json_escape(&e.version_label),
                json_escape(&cname),
                updated,
            ));
            if idx + 1 < count {
                out.push(',');
            }
            out.push('\n');
        }
        out.push(']');
        match yank(&out) {
            Ok(()) => {
                self.status_message = Some(format!("exported {count} rows (JSON) to clipboard"));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    pub(crate) fn export_markdown(&mut self) {
        let count = self.cached_filtered.len();
        let mut out = String::new();
        out.push_str("| NAME | APPLICATION | TIER | STATUS | HEALTH | PLATFORM | VERSION | CNAME | UPDATED |\n");
        out.push_str("| ---- | ----------- | ---- | ------ | ------ | -------- | ------- | ----- | ------- |\n");
        for &i in &self.cached_filtered {
            let e = &self.environments[i];
            let cname = if self.redact {
                redact_block(&e.cname)
            } else {
                e.cname.clone()
            };
            let updated = e.updated.map(|u| u.to_rfc3339()).unwrap_or_default();
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                md_escape(&e.name),
                md_escape(&e.application),
                e.tier,
                e.status,
                e.health,
                md_escape(&e.platform),
                md_escape(&e.version_label),
                md_escape(&cname),
                updated,
            ));
        }
        match yank(&out) {
            Ok(()) => {
                self.status_message =
                    Some(format!("exported {count} rows (Markdown) to clipboard"));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    pub(crate) fn open_describe_overlay(&mut self) {
        let env = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(env) = env else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        self.current_overlay = Some(Overlay::Describe(describe_env(&env)));
    }

    pub(crate) fn open_in_console(&mut self) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(env) = env_opt else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let url = console_url(&self.context.region, &env.application, &env.name);
        match open_url(&url) {
            Ok(()) => {
                self.status_message = Some(format!("opened {} in browser", env.name));
            }
            Err(e) => {
                self.error_message = Some(format!("couldn't open browser: {e}"));
            }
        }
    }

    /// Open the currently-selected instance (in the Instances tab) in the
    /// EC2 console. No-op when no instance is selected.
    pub(crate) fn open_instance_in_console(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(inst) = d.instances.get(d.instances_cursor) else {
            return;
        };
        let region = self.context.region.clone();
        let id = inst.id.clone();
        let url = format!(
            "https://{region}.console.aws.amazon.com/ec2/home?region={region}#InstanceDetails:instanceId={id}"
        );
        let display = id.clone();
        let result = std::process::Command::new(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        })
        .arg(&url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
        match result {
            Ok(_) => {
                self.status_message = Some(format!("opened {display} in EC2 console"));
            }
            Err(e) => {
                self.error_message = Some(format!("could not open browser: {e}"));
            }
        }
    }

    /// Copy the currently-selected instance ID to the clipboard.
    pub(crate) fn yank_instance_id(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(inst) = d.instances.get(d.instances_cursor) else {
            return;
        };
        let id = inst.id.clone();
        match yank(&id) {
            Ok(()) => self.status_message = Some(format!("yanked instance id: {id}")),
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    pub(crate) fn yank_selected(&mut self, kind: YankKind) {
        let Some(env) = self.selected_env() else {
            self.status_message = Some("nothing to yank".into());
            return;
        };
        let value = match kind {
            YankKind::Cname => env.cname.clone(),
            YankKind::Name => env.name.clone(),
        };
        if value.is_empty() {
            self.status_message = Some("selected env has no value to yank".into());
            return;
        }
        match yank(&value) {
            Ok(()) => {
                self.status_message = Some(format!(
                    "copied {} to clipboard",
                    match kind {
                        YankKind::Cname => "CNAME",
                        YankKind::Name => "name",
                    }
                ));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    pub(crate) fn export_tsv(&mut self) {
        let count = self.cached_filtered.len();
        let mut out = String::new();
        out.push_str(
            "NAME\tAPPLICATION\tTIER\tSTATUS\tHEALTH\tPLATFORM\tVERSION\tCNAME\tUPDATED\n",
        );
        for &i in &self.cached_filtered {
            let e = &self.environments[i];
            let cname = if self.redact {
                redact_block(&e.cname)
            } else {
                e.cname.clone()
            };
            let updated = e.updated.map(|u| u.to_rfc3339()).unwrap_or_default();
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                e.name,
                e.application,
                e.tier,
                e.status,
                e.health,
                e.platform,
                e.version_label,
                cname,
                updated
            ));
        }
        match yank(&out) {
            Ok(()) => {
                self.status_message = Some(format!("exported {count} rows (TSV) to clipboard"));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    /// Open the EB applications-page console URL for the selected
    /// application in the browser. Mirrors `open_in_console`'s
    /// `arboard`-clipboard-on-failure shape so the operator still has
    /// the URL available when the browser launch fails (SSH session,
    /// no DISPLAY, etc.).
    pub(crate) fn open_app_in_console(&mut self) {
        let Some(idx) = self.app_table_state.selected() else {
            self.status_message = Some("no application selected".into());
            return;
        };
        let Some(name) = self.applications.get(idx).map(|a| a.name.clone()) else {
            return;
        };
        let region = &self.context.region;
        let app_enc = urlencode(&name);
        let url = format!(
            "https://{region}.console.aws.amazon.com/elasticbeanstalk/home?region={region}#/application/overview?applicationName={app_enc}"
        );
        match open_url(&url) {
            Ok(()) => {
                self.status_message = Some(format!("opened {name} in browser"));
            }
            Err(e) => {
                self.error_message = Some(format!("couldn't open browser: {e}"));
            }
        }
    }
}
