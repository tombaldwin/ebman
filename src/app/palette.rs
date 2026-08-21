//! The Ctrl-P command palette and the `f`-key quick-jump, plus the
//! Tab-completion state machine behind the `:` prompt.

use super::*;

impl App {
    /// `:rds` — fetch the env's RDS dbinstance option settings and
    /// Advance / rewind the command-mode completion cycle by
    /// `delta` (+1 = Tab, -1 = Shift-Tab). Captures the operator's
    /// typed prefix on the first Tab; subsequent Tabs cycle
    /// through matches without losing the original prefix (so
    /// they can pop out by typing).
    ///
    /// What gets completed depends on where the cursor is:
    /// - **No whitespace yet** → the command name (the whole input
    ///   is the name fragment).
    /// - **Whitespace, and the first token is an env-arg command**
    ///   (`:diff` / `:config-diff` / `:rds-detach`, see
    ///   [`command_takes_env_arg`]) → the *trailing* token as an
    ///   environment name, drawn from the loaded fleet. `:diff
    ///   ENV-A ENV-B` completes whichever env name is last.
    /// - **Whitespace, any other command** → the command-name
    ///   fragment is re-completed and args after the first space
    ///   pass through untouched. Means `:set-option aws` still
    ///   completes `set-option` if the operator Tabs at the start.
    pub(crate) fn command_completion_step(&mut self, delta: i32) {
        // First Tab of a cycle: snapshot what the operator had typed
        // so a subsequent reverse-Tab (or text input) can restore.
        // `first_step` also anchors the landing spot below so the very
        // first Tab lands on the *first* candidate (forward) / last
        // (backward), rather than immediately stepping past it.
        let first_step = self.completion.origin.is_none();
        if first_step {
            self.completion.origin = Some(self.command_input.text().to_string());
            self.completion.index = 0;
        }
        let origin = self.completion.origin.clone().unwrap_or_default();
        let ws = origin.find(char::is_whitespace);
        // Env-arg mode: a whitespace-bearing input whose first token
        // is one of the env-name-taking commands. Then we complete
        // the trailing token against the loaded env names instead of
        // the command list.
        let env_mode = ws
            .map(|i| command_takes_env_arg(&origin[..i]))
            .unwrap_or(false);
        // `head` is preserved verbatim before the candidate; `tail`
        // is appended after it. Command-name completion keeps the
        // arg tail (`rest`); env completion folds the whole prefix
        // (command + earlier args + the separating space) into
        // `head` and has no tail.
        let (head, candidates, tail): (String, Vec<String>, String) = match ws {
            None => (String::new(), completion_candidates(&origin), String::new()),
            Some(_) if env_mode => {
                let last_ws = origin
                    .rfind(char::is_whitespace)
                    .expect("origin has whitespace in this arm");
                // `rfind` gives the *first byte* of the last whitespace
                // char; step over the whole char so the split lands on a
                // char boundary (a multi-byte space like U+00A0 NBSP
                // otherwise slices mid-char and panics).
                let frag_start =
                    last_ws + origin[last_ws..].chars().next().map_or(1, char::len_utf8);
                let head = origin[..frag_start].to_string();
                let frag = origin[frag_start..].to_string();
                (head, self.env_name_candidates(&frag), String::new())
            }
            Some(i) => (
                String::new(),
                completion_candidates(&origin[..i]),
                origin[i..].to_string(),
            ),
        };
        if candidates.is_empty() {
            // Restore the operator's typed prefix and surface a
            // hint so the silent-no-op doesn't feel broken.
            self.command_input = origin.clone().into();
            self.status_message = Some(if env_mode {
                "no environment matches (Tab cycles env names)".to_string()
            } else {
                let prefix = ws.map(|i| &origin[..i]).unwrap_or(&origin[..]);
                format!("no command matches '{prefix}' (Tab cycles command names)")
            });
            return;
        }
        let n = candidates.len() as i32;
        let next = if first_step {
            // Land on the first (forward) / last (backward) match.
            if delta >= 0 {
                0
            } else {
                (n - 1) as usize
            }
        } else {
            let cur = self.completion.index as i32;
            (cur + delta).rem_euclid(n) as usize
        };
        self.completion.index = next;
        self.command_input = format!("{head}{}{tail}", candidates[next]).into();
        self.status_message = Some(format!(
            "completion {}/{} — Tab cycles, Esc cancels",
            next + 1,
            n
        ));
    }

    /// Environment names from the loaded fleet that start with
    /// `prefix`, sorted + deduped — the candidate list for
    /// command-bar argument completion (see
    /// [`Self::command_completion_step`]).
    pub(crate) fn env_name_candidates(&self, prefix: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .environments
            .iter()
            .map(|e| e.name.clone())
            .filter(|n| n.starts_with(prefix))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub(crate) fn open_palette(&mut self) {
        self.palette_input.clear();
        self.palette_items = build_palette_items(self);
        self.palette_refilter();
        self.mode = Mode::Palette;
    }

    pub(crate) fn palette_refilter(&mut self) {
        let needle = self.palette_input.text().to_lowercase();
        let mut scored: Vec<(usize, isize)> = self
            .palette_items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                let s = palette_score(&needle, &it.label, &it.detail)?;
                Some((i, s))
            })
            .collect();
        scored.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        self.palette_filtered = scored.into_iter().map(|(i, _)| i).collect();
        self.palette_state
            .select(if self.palette_filtered.is_empty() {
                None
            } else {
                Some(0)
            });
    }

    pub(crate) fn palette_move(&mut self, delta: i32) {
        let n = self.palette_filtered.len();
        if n == 0 {
            self.palette_state.select(None);
            return;
        }
        let cur = self.palette_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(n as i32) as usize;
        self.palette_state.select(Some(next));
    }

    pub(crate) fn palette_execute(&mut self) {
        let Some(pos) = self.palette_state.selected() else {
            return;
        };
        let Some(&idx) = self.palette_filtered.get(pos) else {
            return;
        };
        let Some(item) = self.palette_items.get(idx).cloned() else {
            return;
        };
        self.mode = Mode::Normal;
        self.palette_input.clear();
        match item.action {
            PaletteAction::RunCommand(cmd) => self.execute_command(&cmd),
            PaletteAction::PrefillCommand(prefix) => {
                self.command_input = prefix.into();
                self.mode = Mode::Command;
            }
            PaletteAction::JumpEnv(name) => {
                if let Some(pos) = self.view.display().iter().position(|r| match r {
                    DisplayRow::Env(i) => self.environments[*i].name == name,
                    DisplayRow::Separator => false,
                }) {
                    self.table_state.select(Some(pos));
                    self.status_message = Some(format!("jumped to {name}"));
                }
            }
            PaletteAction::LoadView(name) => {
                self.execute_command(&format!("view {name}"));
            }
        }
    }

    pub(crate) fn quickjump_apply(&mut self) {
        if self.quickjump_input.is_empty() {
            return;
        }
        let needle = self.quickjump_input.text().to_lowercase();
        for (pos, row) in self.view.display().iter().enumerate() {
            if let DisplayRow::Env(i) = row {
                let e = &self.environments[*i];
                let alias = self
                    .aliases
                    .get(&e.name)
                    .map(|a| a.to_lowercase())
                    .unwrap_or_default();
                if e.name.to_lowercase().starts_with(&needle) || alias.starts_with(&needle) {
                    self.table_state.select(Some(pos));
                    return;
                }
            }
        }
    }

    pub(crate) fn quick_jump(&mut self, n: usize) {
        // 1..=9 maps to position n-1 in the visible env rows.
        let Some(target_env) = self
            .view
            .display()
            .iter()
            .filter(|r| matches!(r, DisplayRow::Env(_)))
            .nth(n.saturating_sub(1))
        else {
            return;
        };
        if let Some(pos) = self
            .view
            .display()
            .iter()
            .position(|r| std::ptr::eq(r, target_env))
        {
            self.table_state.select(Some(pos));
        }
    }
}
