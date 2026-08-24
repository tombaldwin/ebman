//! Suspending the TUI to hand the terminal to a child process: the
//! embedded `:shell`, and `$EDITOR` for the `:env` buffer.
//!
//! Both must leave the alternate screen and restore it on the way
//! back, including on the error paths.

use super::*;

impl App {
    /// Open an embedded SSM session into `instance_id`. Allocates a PTY,
    /// spawns `aws ssm start-session` inside it, and switches to
    /// `Mode::Shell` where keystrokes are forwarded to the subprocess
    /// instead of running ebman bindings. **F12** detaches back to the
    /// previous mode; the session keeps running and the user can re-open
    /// the pane (state preserved). The session ends when the subprocess
    /// exits — typically via the user typing `exit` or `^D`.
    pub(crate) fn open_embedded_shell(
        &mut self,
        terminal: &mut Tui,
        instance_id: &str,
    ) -> Result<()> {
        // Demo-mode short-circuit. The fixture's instance IDs are
        // synthetic, the AwsClient is a stub, and `aws ssm start-
        // session` would fail with "InstanceNotFound" (or hang
        // waiting for the session-manager-plugin handshake). Instead
        // spin up a fake `ShellSession` with a vt100::Parser
        // pre-loaded with canned content (session banner + a few
        // operator-realistic commands), and route into `Mode::Shell`
        // exactly like a real session. VHS captures show a real-
        // looking SSM pane; F12 detaches per the usual contract.
        if self.demo_mode {
            let size = terminal.size()?;
            let rows = size.height.saturating_sub(2).max(4);
            let cols = size.width.max(20);
            let content = crate::demo_fixture::canned_ssm_session(instance_id);
            let session =
                crate::shell::ShellSession::demo(instance_id.to_string(), &content, rows, cols);
            self.shell_return_mode = self.mode;
            self.current_shell = Some(Box::new(session));
            self.mode = Mode::Shell;
            return Ok(());
        }
        let region = self.context.region.clone();
        let profile = self
            .override_profile
            .clone()
            .or_else(|| self.context.profile.clone());
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            profile.as_deref(),
            &region,
            "SsmSession",
            instance_id,
            &[],
        );

        let size = terminal.size()?;
        // Reserve 2 rows for a thin status bar so the pane title + detach
        // hint are always visible.
        let rows = size.height.saturating_sub(2).max(4);
        let cols = size.width.max(20);

        let mut args = vec![
            "ssm",
            "start-session",
            "--target",
            instance_id,
            "--region",
            &region,
        ];
        let prof = profile.clone();
        if let Some(p) = prof.as_deref() {
            args.push("--profile");
            args.push(p);
        }
        match crate::shell::ShellSession::spawn(
            "aws",
            &args,
            rows,
            cols,
            format!("ssm: {instance_id}"),
        ) {
            Ok(session) => {
                self.current_shell = Some(Box::new(session));
                self.shell_return_mode = self.mode;
                self.mode = Mode::Shell;
                self.status_message = Some(format!(
                    "ssm session into {instance_id} — F12 detaches, ^D / exit closes"
                ));
            }
            Err(e) => {
                self.error_message = Some(format!(
                    "could not start SSM session ({e}). Install the AWS CLI + session-manager-plugin and check ssm:StartSession IAM"
                ));
            }
        }
        Ok(())
    }

    /// Forward a key event to the running shell's PTY. Called only when
    /// `Mode::Shell` is active. F12 is consumed locally as the detach key.
    pub(crate) fn handle_shell_key(&mut self, key: KeyEvent) {
        // F12 detaches without killing the subprocess. Demo sessions
        // (no real PTY behind them) also accept Esc as a detach — VHS
        // can't emit F12 reliably, and there's no subprocess to
        // forward bytes to anyway. Real sessions keep Esc forwarded
        // to the PTY because vim / less / many TUIs need it.
        let is_demo_session = self
            .current_shell
            .as_ref()
            .is_some_and(|s| s.writer.is_none());
        let detach = matches!(key.code, KeyCode::F(12))
            || (is_demo_session && matches!(key.code, KeyCode::Esc));
        if detach {
            self.mode = self.shell_return_mode;
            self.status_message = Some(
                "detached from shell — F12 reattaches, or open shell again from Instances tab"
                    .into(),
            );
            return;
        }
        if let Some(shell) = self.current_shell.as_mut() {
            if let Some(bytes) = crate::shell::key_event_to_bytes(&key) {
                let _ = shell.send(&bytes);
            }
        }
    }

    /// Tear down a finished shell session: the subprocess has exited, the
    /// reader thread returned. Surfaces a status message and routes the
    /// user back to where they came from.
    pub(crate) fn close_shell_session(&mut self) {
        if let Some(mut s) = self.current_shell.take() {
            s.kill();
            self.status_message = Some(format!("{} ended", s.label));
        }
        self.mode = self.shell_return_mode;
    }

    /// Open the operator's `$EDITOR` against a temp file holding
    /// the current env vars in `KEY=VALUE` form. On save, parses
    /// the file, diffs against `original`, and dispatches the
    /// deltas via `spawn_option_settings_update`. Cancel paths
    /// (unchanged file / missing file / editor non-zero exit)
    /// are no-ops with a clear status message.
    ///
    /// Drops out of the alt-screen for the editor (vim / nano /
    /// VS Code's `code --wait` etc. all need the terminal directly)
    /// and re-enters when the editor exits.
    pub(crate) fn run_env_editor(
        &mut self,
        terminal: &mut Tui,
        env_name: &str,
        original: &[(String, String)],
    ) -> Result<()> {
        use crossterm::{
            event::EnableMouseCapture,
            execute,
            terminal::{enable_raw_mode, EnterAlternateScreen},
        };

        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".to_string());

        // Temp file path. Use the OS temp dir + a fingerprint
        // built from the env name + epoch nanos so concurrent
        // sessions can't collide. Format suffix `.env` so editor
        // syntax-highlighters give the operator a useful default.
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let safe = env_name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("ebman-env-{safe}-{now_ns}.env"));

        let body = build_env_edit_body(env_name, original);
        // 0600: the body is the env's variables — secrets — sitting in
        // the shared temp dir for the whole $EDITOR session.
        crate::util::write_secure(&path, body.as_bytes()).wrap_err("writing env-edit temp file")?;

        // Leave the TUI for the editor — best-effort, all steps.
        // This was `disable_raw_mode()?` followed by a `?` on the
        // execute, so a failure in the first spawned $EDITOR into a
        // terminal still in raw mode and still on the alternate screen.
        crate::restore_terminal(terminal);

        let status = std::process::Command::new(&editor).arg(&path).status();

        // Always re-enter, regardless of editor outcome.
        enable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture
        )?;
        terminal.hide_cursor()?;
        terminal.clear()?;

        match status {
            Ok(s) if !s.success() => {
                self.error_message = Some(format!(
                    "$EDITOR ({editor}) exited {} — no changes dispatched",
                    s.code().unwrap_or(-1)
                ));
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
            Err(e) => {
                self.error_message = Some(format!(
                    "couldn't launch editor ({editor}): {e} — set $EDITOR / $VISUAL"
                ));
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
            _ => {}
        }

        let edited = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.error_message = Some(format!(
                    "couldn't re-read temp file at {} — no changes dispatched ({e})",
                    path.display()
                ));
                // Every other branch removes the (secrets-bearing)
                // temp file — this one must too.
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
        };
        let _ = std::fs::remove_file(&path);

        let edited_map = parse_env_edit_body(&edited);
        let original_map: std::collections::BTreeMap<String, String> = original
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let (to_set, to_remove) = diff_env_vars(
            "aws:elasticbeanstalk:application:environment",
            &original_map,
            &edited_map,
        );

        if to_set.is_empty() && to_remove.is_empty() {
            self.status_message = Some("env-edit: no changes — nothing dispatched".into());
            return Ok(());
        }

        let label = format!(
            "env-edit ({} set, {} removed)",
            to_set.len(),
            to_remove.len()
        );
        self.spawn_option_settings_update(label, to_set, to_remove);
        Ok(())
    }
}
