//! What may be published: the tracked file list, and the account IDs
//! and client names none of it may carry. The `.githooks` check reads
//! the same list as the guards here.

/// Every file git tracks — which is what `cargo package` publishes,
/// bar `Cargo.toml`'s `exclude` and two files cargo generates.
///
/// `None` ONLY when there is genuinely no repository: `cargo mutants`
/// builds in a scratch copy of the tree with no `.git`, so `git
/// ls-files` fails there through no fault of the code under test. Any
/// other git failure panics — "git errored" and "there is no repo" are
/// different, and treating the first as the second is how a guard goes
/// quiet in the environment that matters.
///
/// Carries its own floor, like [`source_files`]: a listing that finds
/// almost nothing is a broken listing, and a guard over it passes
/// vacuously.
pub(crate) fn tracked_files() -> Option<Vec<String>> {
    let out = match std::process::Command::new("git")
        .args(["ls-files"])
        // `LC_ALL=C`, so the stderr match below is not locale-dependent.
        // git translates "not a git repository" — Apple Git ships no
        // translations, Homebrew and Linux git do — so on a localised
        // machine the skip condition would not match and every caller
        // would fail loudly instead of skipping. Loud is the safer wrong
        // direction, but a guard that cannot run where `cargo mutants`
        // runs is not much of a guard.
        .env("LC_ALL", "C")
        .output()
    {
        Ok(out) => out,
        Err(e) => panic!("could not run git: {e}"),
    };
    if !out.status.success() {
        let no_repo = String::from_utf8_lossy(&out.stderr).contains("not a git repository");
        assert!(
            no_repo,
            "git ls-files failed for a reason other than a missing \
             repository: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        files.len() > 50,
        "only {} tracked files — the listing failed and a guard over it \
         would pass on an empty result",
        files.len()
    );
    Some(files)
}

/// The account IDs AWS's own documentation uses as placeholders. A
/// fixture using one of these is recognisably a fixture.
pub(crate) const AWS_DOC_ACCOUNT_IDS: &[&str] = &[
    "123456789012",
    "111122223333",
    "444455556666",
    "555555555555",
    "777788889999",
];

/// Every token in `text` that could be a REAL AWS account ID, as
/// (1-based line, value).
///
/// A candidate is a run of exactly twelve ASCII digits with no ASCII
/// letter, digit or `_` on either side — the shape of an account ID
/// standing alone, or as the account field of an ARN, where it sits
/// between `:` delimiters. So one rule covers both halves of the
/// backlog item: a separate ARN check would find nothing this does not.
///
/// The alphanumeric boundary is what keeps `Cargo.lock` quiet: its
/// checksums are hex, and hex routinely holds a twelve-digit run
/// between two letters.
///
/// Also the console's display form, three groups of four joined by `-`
/// (`1234-5678-9012`), bounded the same way and by no further `-`, so a
/// longer dashed number is not taken for one. Checked against the
/// placeholders and repdigits by its digits.
///
/// Not candidates: [`AWS_DOC_ACCOUNT_IDS`], and repdigits
/// (`000000000000`, `111111111111`, …). The risk this exists for is a
/// real value that LOOKS like a fixture, and nobody mistakes a
/// repdigit for anything but a placeholder — so excluding the class is
/// a rule about the shape, not an allowlist of values.
pub(crate) fn account_id_candidates(text: &str) -> Vec<(usize, &str)> {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut hits = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if !bytes[i].is_ascii_digit() {
                i += 1;
                continue;
            }
            if let Some(end) = dashed_id_at(bytes, i) {
                let run = &line[i..end];
                let digits: String = run.chars().filter(char::is_ascii_digit).collect();
                let repdigit = digits.bytes().all(|b| b == digits.as_bytes()[0]);
                if !repdigit && !AWS_DOC_ACCOUNT_IDS.contains(&digits.as_str()) {
                    hits.push((idx + 1, run));
                }
                i = end;
                continue;
            }
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let bounded = (start == 0 || !is_word(bytes[start - 1]))
                && (i == bytes.len() || !is_word(bytes[i]));
            if i - start != 12 || !bounded {
                continue;
            }
            // ASCII digits only, so this slice is on char boundaries.
            let run = &line[start..i];
            let repdigit = run.bytes().all(|b| b == run.as_bytes()[0]);
            if !repdigit && !AWS_DOC_ACCOUNT_IDS.contains(&run) {
                hits.push((idx + 1, run));
            }
        }
    }
    hits
}

/// The end of a `dddd-dddd-dddd` account ID starting at `start`, if one
/// does: bounded on both sides by neither a word character nor `-`.
fn dashed_id_at(bytes: &[u8], start: usize) -> Option<usize> {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    if start > 0 && is_word(bytes[start - 1]) {
        return None;
    }
    let end = start + 14;
    let shape = bytes.get(start..end)?;
    let ok = shape.iter().enumerate().all(|(k, b)| match k {
        4 | 9 => *b == b'-',
        _ => b.is_ascii_digit(),
    });
    (ok && bytes.get(end).is_none_or(|b| !is_word(*b))).then_some(end)
}

/// Where the names of clients whose data must never ship are listed,
/// locally. Gitignored and in `Cargo.toml`'s `exclude`: the list is
/// itself the sensitive data, so it can live anywhere but the repo.
/// Every commit message (labelled by its short hash) and tag
/// annotation in the repository, for the protected-name scan; `None`
/// outside a git checkout, as [`tracked_files`]. A shallow clone would
/// make that scan see one commit and pass, so in CI it is an error.
pub(crate) fn history_texts() -> Option<Vec<(String, String)>> {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    tracked_files()?;
    if std::env::var_os("CI").is_some() {
        assert_ne!(
            git(&["rev-parse", "--is-shallow-repository"])
                .as_deref()
                .map(str::trim),
            Some("true"),
            "a shallow clone: the commit-message scan would see one commit. \
             Check out with `fetch-depth: 0`"
        );
    }
    let mut texts = Vec::new();
    // NUL-separated records: a message may contain anything else.
    // What is published: HEAD, branches and tags. Not `--all`, which
    // also reads the stash and stale remote-tracking refs, local state
    // that can fail this for reasons the code does not have.
    let log = git(&[
        "log",
        "HEAD",
        "--branches",
        "--tags",
        "--format=%h%x00%B%x00",
    ])
    .unwrap_or_default();
    let mut parts = log.split('\0');
    while let (Some(hash), Some(body)) = (parts.next(), parts.next()) {
        let hash = hash.trim();
        if !hash.is_empty() {
            texts.push((format!("commit {hash}"), body.to_string()));
        }
    }
    let tags = git(&[
        "for-each-ref",
        "refs/tags",
        "--format=%(refname:short)%00%(contents)%00",
    ])
    .unwrap_or_default();
    let mut parts = tags.split('\0');
    while let (Some(tag), Some(body)) = (parts.next(), parts.next()) {
        let tag = tag.trim();
        if !tag.is_empty() {
            texts.push((format!("tag {tag}"), body.to_string()));
        }
    }
    Some(texts)
}

pub(crate) const PROTECTED_NAMES_FILE: &str = ".protected-names";

/// The same list for CI, from a repository secret.
pub(crate) const PROTECTED_NAMES_ENV: &str = "EBMAN_PROTECTED_NAMES";

/// Names shorter than this would match inside ordinary words and make
/// the guard fire on everything — at which point it gets ignored.
const PROTECTED_NAME_MIN_LEN: usize = 4;

/// Parse a protected-names list: one name per line or comma-separated,
/// `#` starts a comment line, blanks ignored, matched case-insensitively.
///
/// A name too short to scan for is an ERROR, not a skip: silently
/// dropping an entry would leave the operator believing a name is
/// protected when nothing checks for it.
pub(crate) fn parse_protected_names(text: &str) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for part in line.split(',') {
            let name = part.trim().to_lowercase();
            if name.is_empty() {
                continue;
            }
            if name.chars().count() < PROTECTED_NAME_MIN_LEN {
                return Err(format!(
                    "a protected name is shorter than {PROTECTED_NAME_MIN_LEN} characters \
                     and would match inside ordinary words"
                ));
            }
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    Ok(names)
}

/// Every (1-based line, index into `names`) where a protected name
/// appears in `text`, case-insensitively and anywhere in a word —
/// `Name-prod` and `name_truth.txt` are the shapes that leaked.
pub(crate) fn protected_name_hits(text: &str, names: &[String]) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let lower = line.to_lowercase();
        for (k, name) in names.iter().enumerate() {
            if lower.contains(name.as_str()) {
                hits.push((idx + 1, k));
            }
        }
    }
    hits
}

/// The protected names configured for this run: the CI secret and the
/// local file, merged. Empty when neither is present. A malformed list
/// panics — see [`parse_protected_names`].
pub(crate) fn protected_names() -> Vec<String> {
    let mut text = std::env::var(PROTECTED_NAMES_ENV).unwrap_or_default();
    if let Some(local) = protected_names_paths()
        .into_iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
    {
        text.push('\n');
        text.push_str(&local);
    }
    parse_protected_names(&text).unwrap_or_else(|e| panic!("{e}"))
}

/// Where the list may be: this checkout, then the main checkout. A
/// linked worktree has no copy of the gitignored file, and the guard
/// there found an empty list and passed. The hooks look in the same two
/// places (`.githooks/protected-names.sh`).
fn protected_names_paths() -> Vec<std::path::PathBuf> {
    let mut paths = vec![std::path::PathBuf::from(PROTECTED_NAMES_FILE)];
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
    {
        if out.status.success() {
            let common = String::from_utf8_lossy(&out.stdout).trim().to_string();
            paths.push(
                std::path::Path::new(&common)
                    .join("..")
                    .join(PROTECTED_NAMES_FILE),
            );
        }
    }
    paths
}

/// `123456789012` -> `12…12`: enough to find in `path:line`, not
/// enough to be the leak. A guard whose job is stopping a real account
/// ID being published must not print one into a public CI log.
pub(crate) fn mask_account_id(id: &str) -> String {
    match (id.get(..2), id.get(id.len().saturating_sub(2)..)) {
        (Some(head), Some(tail)) if id.len() > 4 => format!("{head}…{tail}"),
        _ => "…".to_owned(),
    }
}

#[cfg(test)]
mod tracked {
    /// Nothing that is not source, docs or config may reach the crate.
    ///
    /// 0.34.1 was YANKED because its published tarball carried 22
    /// `mutants.out/` files, one of which held the build machine's
    /// hostname and username. `Cargo.toml`'s `exclude` was tightened
    /// then — and it excludes by NAME, so it stops the thing that
    /// happened and not the class.
    ///
    /// It happened again: `sed -i.bak` writes its backup IN PLACE, a
    /// `git add -A` swept it up, and a 55KB copy of a test file was
    /// committed and would have shipped in 0.36.0. Caught by review,
    /// not by any check.
    ///
    /// So this asserts on the tracked file list by EXTENSION rather
    /// than by name. `.gitignore` stops the accident; this stops it
    /// being committed deliberately or by a tool that ignores
    /// `.gitignore`.
    #[test]
    fn no_backup_or_scratch_files_are_tracked() {
        // Through the shared helper, which skips only when there is
        // genuinely no repository (the `cargo mutants` scratch copy) and
        // carries the floor this guard used to hold inline.
        let Some(files) = super::tracked_files() else {
            return;
        };

        let bad: Vec<&str> = files
            .iter()
            .map(String::as_str)
            .filter(|f| {
                f.ends_with(".bak")
                    || f.ends_with(".orig")
                    || f.ends_with(".rej")
                    || f.ends_with('~')
                    || f.contains("mutants.out/")
            })
            .collect();
        assert!(
            bad.is_empty(),
            "these are tracked and would be published in the crate \
             tarball: {bad:?}"
        );
    }

    /// No published file may carry an account ID that could be real.
    ///
    /// Everything git tracks ships to crates.io, and docs.rs renders the
    /// test modules as browsable HTML. A crates.io version cannot be
    /// unpublished, only yanked, and a yanked version stays downloadable
    /// — so a real account ID pasted as a fixture is published, indexed
    /// and permanent. 0.34.1 was yanked for publishing a hostname; this
    /// is the same door.
    ///
    /// Review cannot be relied on for it: a real value beside genuine
    /// placeholders reads as "12 digits, test file, fine". The likeliest
    /// source is a test of redaction or masking — pasting a real value
    /// is how you prove the masking works — and `CHANGELOG.md`, which
    /// carries field reports from real fleets.
    ///
    /// Reads every tracked file lossily rather than skipping the ones
    /// that are not UTF-8: a text file with one stray byte must not drop
    /// out of the scan in silence.
    #[test]
    fn no_published_file_carries_a_real_looking_account_id() {
        let Some(files) = super::tracked_files() else {
            return;
        };
        let mut scanned = 0usize;
        let mut hits = Vec::new();
        for path in &files {
            // A tracked file deleted in the working tree is not read;
            // the floor below stops that becoming a vacuous pass.
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            scanned += 1;
            let text = String::from_utf8_lossy(&bytes);
            for (line, id) in super::account_id_candidates(&text) {
                hits.push(format!("{path}:{line}: {}", super::mask_account_id(id)));
            }
        }
        assert!(
            scanned > 50,
            "only {scanned} tracked files could be read — a scan over \
             nothing passes vacuously"
        );
        assert!(
            hits.is_empty(),
            "these look like real AWS account IDs and would be published \
             permanently in the crate: {hits:#?}\n\
             Replace each with one of AWS's documentation placeholders \
             ({:?}) or a repdigit. Do NOT add it to the placeholder list: \
             that list is AWS's, and widening it to quiet this guard is a \
             stop condition in CLAUDE.md. If it is not an account ID at \
             all, the detector is wrong for that shape — fix the detector.",
            super::AWS_DOC_ACCOUNT_IDS
        );
    }
}

/// No published file may name a client whose data has leaked before.
///
/// The account-ID guard cannot catch this: a name has no shape. One
/// client's name was scrubbed from this repository in 0.38.0 and again
/// on 2026-09-18, and was written back into `PLAN.md` and a design note
/// the next day — by agent sessions citing that client's fleet as
/// evidence — then shipped in four releases.
///
/// The list lives outside the repo (see [`super::PROTECTED_NAMES_FILE`]
/// and [`super::PROTECTED_NAMES_ENV`]), because a list in the repo would
/// publish exactly what it protects. Hence the two asymmetries below:
/// with no list, this passes on a contributor's machine but FAILS in CI,
/// where an empty list would make the guard vacuous; and the list file
/// must never be tracked.
///
/// Reports `path:line` and the name's position in the list — never the
/// name, which would put it in a CI log.
#[cfg(test)]
mod protected_names {
    #[test]
    fn no_tracked_file_names_a_protected_client() {
        // First, so the `cargo mutants` scratch copy (no `.git`) skips
        // before the name list is required.
        let Some(files) = super::tracked_files() else {
            return;
        };
        assert!(
            !files.iter().any(|f| f == super::PROTECTED_NAMES_FILE),
            "{} is TRACKED — it would be published in the crate. \
             `git rm --cached {}` and keep it local",
            super::PROTECTED_NAMES_FILE,
            super::PROTECTED_NAMES_FILE
        );
        let names = super::protected_names();
        if names.is_empty() {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI has no protected-names list, so this guard would pass \
                 vacuously. Set the `{}` repository secret (and the \
                 Dependabot secret of the same name) and pass it to every \
                 job that runs `cargo test`",
                super::PROTECTED_NAMES_ENV
            );
            return;
        }
        let mut hits = Vec::new();
        for path in &files {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            for (line, k) in super::protected_name_hits(&String::from_utf8_lossy(&bytes), &names) {
                hits.push(format!("{path}:{line}: protected name #{}", k + 1));
            }
        }
        assert!(
            hits.is_empty(),
            "these name a protected client and would be published \
             permanently in the crate: {hits:#?}\n\
             Use the `poly-*` placeholder convention, or \"a production \
             fleet\" when citing real-world evidence."
        );
    }

    /// Commit messages and tag annotations are published too, and the
    /// release notes are generated from the messages; the file scan
    /// above never read them. History was rewritten clean on
    /// 2026-10-21, so this holds over all of it.
    #[test]
    fn no_commit_message_or_tag_names_a_protected_client() {
        let Some(texts) = super::history_texts() else {
            return;
        };
        // It reads the messages at all: a word every history here has
        // is found, through the same matcher.
        let probe = vec!["refactor".to_string()];
        assert!(
            texts.iter().any(|(what, t)| what.starts_with("commit ")
                && !super::protected_name_hits(t, &probe).is_empty()),
            "the history scan read no commit messages ({} texts)",
            texts.len()
        );
        // Tags: there are some to read (a word in them would be chance).
        if std::process::Command::new("git")
            .args(["tag", "--list"])
            .output()
            .is_ok_and(|o| !o.stdout.is_empty())
        {
            assert!(
                texts
                    .iter()
                    .any(|(what, t)| what.starts_with("tag ") && !t.trim().is_empty()),
                "the history scan read no tag texts"
            );
        }
        let names = super::protected_names();
        if names.is_empty() {
            // The file scan above fails CI for a missing list.
            return;
        }
        let mut hits = Vec::new();
        for (what, text) in &texts {
            for (line, k) in super::protected_name_hits(text, &names) {
                hits.push(format!("{what} line {line}: protected name #{}", k + 1));
            }
        }
        assert!(
            hits.is_empty(),
            "these name a protected client, and history is public: {hits:#?}\n\
             A pushed commit needs its message rewritten (and a force \
             push); an unpushed one, `git commit --amend`."
        );
    }

    /// pre-commit refuses a change to the mutation allowlist, the
    /// config file or a `mutants::skip` in Rust, unless the maintainer
    /// says so; any other staged change passes it.
    #[test]
    fn the_pre_commit_hook_keeps_the_mutation_allowlist_the_maintainer_s() {
        if !std::path::Path::new(".githooks/pre-commit").exists()
            || super::tracked_files().is_none()
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!("ebman-test-{}-allowlist", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let isolated = |program: &str| {
            let mut cmd = std::process::Command::new(program);
            cmd.env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .env_remove("EBMAN_MAINTAINER")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("EBMAN_PROTECTED_NAMES", "zorblax");
            cmd
        };
        // Spelled apart, so this file's own diff is not an attribute.
        let skip = format!("#[mutants::{}]", "skip");
        let attr = format!("#[cfg_attr(test, mutants::{})]", "skip");
        for (n, (path, body, maintainer, refused)) in [
            (".cargo/mutants.toml", "exclude_re = [\"x\"]\n", false, true),
            (".cargo/mutants.toml", "exclude_re = [\"x\"]\n", true, false),
            (
                "src/lib.rs",
                &*format!("{skip}\nfn f() {{}}\n"),
                false,
                true,
            ),
            (
                "src/lib.rs",
                &*format!("{skip}\nfn f() {{}}\n"),
                true,
                false,
            ),
            (
                "notes.md",
                &*format!("{skip} is the maintainer's\n"),
                false,
                false,
            ),
            ("src/lib.rs", "fn f() {}\n", false, false),
            (
                "src/lib.rs",
                "/// a mutants::skip is the maintainer's\nfn f() {}\n",
                false,
                false,
            ),
            (
                "src/lib.rs",
                &*format!("{attr}\nfn f() {{}}\n"),
                false,
                true,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let repo = dir.join(n.to_string());
            std::fs::create_dir_all(repo.join(".githooks")).unwrap();
            for f in ["pre-commit", "protected-names.sh"] {
                std::fs::copy(format!(".githooks/{f}"), repo.join(".githooks").join(f)).unwrap();
            }
            let git = |args: &[&str]| {
                isolated("git")
                    .args(args)
                    .current_dir(&repo)
                    .output()
                    .expect("git")
            };
            assert!(git(&["init", "-q"]).status.success());
            let file = repo.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, body).unwrap();
            assert!(git(&["add", path]).status.success());
            let mut hook = isolated("bash");
            hook.arg(".githooks/pre-commit").current_dir(&repo);
            if maintainer {
                hook.env("EBMAN_MAINTAINER", "1");
            }
            let out = hook.output().expect("run the hook");
            assert_eq!(
                !out.status.success(),
                refused,
                "{path} (maintainer {maintainer}): {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// The commit-msg and pre-commit hooks check with their own shell
    /// copy of the list rules; they must find what this matcher finds,
    /// and say which entry without naming it.
    #[test]
    fn the_commit_hooks_agree_with_the_guard_and_never_print_a_name() {
        // The hooks run inside a git checkout (they find their helper
        // through `git rev-parse`); `cargo mutants` tests a copy with no
        // `.git`, where they cannot run at all, as the other git guards
        // skip.
        if !std::path::Path::new(".githooks/commit-msg").exists()
            || super::tracked_files().is_none()
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!("ebman-test-{}-hooks", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Removed however the test ends, a failed assertion included.
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        // Every git and hook child: none of this repository's state (a
        // test run from inside a git hook inherits GIT_DIR and the
        // index), and none of the developer's git config (a global
        // `commit.gpgsign` would call gpg, a global hooks path would run
        // their hooks).
        let isolated = |program: &str| {
            let mut cmd = std::process::Command::new(program);
            cmd.env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1");
            cmd
        };
        let fixture = "# made up\nZorblax, quuxcorp";
        let names = parse_protected_names(fixture).unwrap();
        for (message, named) in [
            ("feat: a fix for the zorblax fleet", true),
            ("feat: QuuxCorp's alarms", true),
            ("feat: a fix for a production fleet", false),
        ] {
            let path = dir.join("msg");
            std::fs::write(&path, message).unwrap();
            let out = isolated("bash")
                .arg(".githooks/commit-msg")
                .arg(&path)
                .env("EBMAN_PROTECTED_NAMES", fixture)
                .output()
                .expect("run the hook");
            assert_eq!(!out.status.success(), named, "{message}");
            assert_eq!(
                !protected_name_hits(message, &names).is_empty(),
                named,
                "the guard agrees: {message}"
            );
            let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
            assert!(
                !stderr.contains("zorblax") && !stderr.contains("quuxcorp"),
                "the hook printed a name: {stderr}"
            );
        }
        // pre-commit, in a throwaway repo with no Cargo.toml: the name
        // check runs before the cargo steps would bail, and it reads
        // staged PATHS as well as lines.
        let repo = dir.join("repo");
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join(".githooks")).unwrap();
        for f in ["pre-commit", "protected-names.sh"] {
            std::fs::copy(format!(".githooks/{f}"), repo.join(".githooks").join(f)).unwrap();
        }
        // Not this repository's: a test run from inside a git hook would
        // otherwise inherit GIT_DIR and act on it.
        let git_in = |dir: &std::path::Path, args: &[&str]| {
            isolated("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git")
        };
        let git = |args: &[&str]| git_in(&repo, args);
        assert!(git(&["init", "-q"]).status.success());
        std::fs::write(repo.join("zorblax-notes.txt"), "nothing to see\n").unwrap();
        assert!(git(&["add", "zorblax-notes.txt"]).status.success());
        let out = isolated("bash")
            .arg(".githooks/pre-commit")
            .current_dir(&repo)
            .env("EBMAN_PROTECTED_NAMES", fixture)
            .output()
            .expect("run the hook");
        assert!(
            !out.status.success(),
            "a staged PATH naming a client passed"
        );
        assert!(!String::from_utf8_lossy(&out.stderr)
            .to_lowercase()
            .contains("zorblax"));

        // A linked worktree has no copy of the gitignored list: the
        // hooks there read the main checkout's, rather than passing on
        // an empty one.
        let identity = [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ];
        let commit = [&identity[..], &["commit", "-q", "--no-verify", "-m", "x"]].concat();
        assert!(git(&commit).status.success());
        std::fs::write(repo.join(".protected-names"), "zorblax\n").unwrap();
        let tree = dir.join("linked");
        let _ = std::fs::remove_dir_all(&tree);
        assert!(git(&["worktree", "add", "-q", tree.to_str().unwrap()])
            .status
            .success());
        std::fs::create_dir_all(tree.join(".githooks")).unwrap();
        for f in ["commit-msg", "protected-names.sh"] {
            std::fs::copy(format!(".githooks/{f}"), tree.join(".githooks").join(f)).unwrap();
        }
        let msg = dir.join("linked-msg");
        std::fs::write(&msg, "fix the zorblax fleet").unwrap();
        let out = isolated("bash")
            .arg(".githooks/commit-msg")
            .arg(&msg)
            .current_dir(&tree)
            .env_remove("EBMAN_PROTECTED_NAMES")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("run the hook");
        assert!(
            !out.status.success(),
            "the worktree's hook found no list: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = git_in(
            &repo,
            &["worktree", "remove", "--force", tree.to_str().unwrap()],
        );
        let _ = std::fs::remove_dir_all(&repo);

        // A name too short to scan for is refused, as by the parser.
        let path = dir.join("msg");
        std::fs::write(&path, "anything").unwrap();
        let out = isolated("bash")
            .arg(".githooks/commit-msg")
            .arg(&path)
            .env("EBMAN_PROTECTED_NAMES", "abc")
            .output()
            .expect("run the hook");
        assert!(!out.status.success());
        assert!(parse_protected_names("abc").is_err());
    }

    use super::{parse_protected_names, protected_name_hits};

    /// The guard reads the list from this checkout and then the main
    /// one, as the hooks do: a linked worktree has no copy.
    #[test]
    fn the_list_is_looked_for_in_the_main_checkout_too() {
        if super::tracked_files().is_none() {
            return;
        }
        let paths = super::protected_names_paths();
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(
            paths[1].ends_with(std::path::Path::new("..").join(super::PROTECTED_NAMES_FILE)),
            "{paths:?}"
        );
    }

    /// Fixture names are made up; the real list must never appear here.
    fn names(list: &str) -> Vec<String> {
        parse_protected_names(list).expect("valid list")
    }

    #[test]
    fn a_list_parses_lines_commas_comments_and_case() {
        assert_eq!(
            names("# clients\nZorblax\n\n  quuxcorp , Wibbleco\nzorblax"),
            vec!["zorblax", "quuxcorp", "wibbleco"]
        );
    }

    #[test]
    fn a_name_too_short_to_scan_for_is_an_error_not_a_skip() {
        assert!(parse_protected_names("zorblax\nabc").is_err());
        assert!(parse_protected_names("abcd").is_ok());
    }

    /// The shapes that actually leaked: an environment name, a
    /// possessive in prose, a filename — case varying each time.
    #[test]
    fn a_name_is_found_inside_words_in_any_case() {
        let list = names("zorblax");
        let text = "fine\nZorblax-prod\nzORBLAX's setup\neval/zorblax_truth.txt\nfine";
        assert_eq!(
            protected_name_hits(text, &list),
            vec![(2, 0), (3, 0), (4, 0)]
        );
    }

    #[test]
    fn nothing_is_found_when_no_name_is_present() {
        assert!(protected_name_hits("poly-prod\npoly-batch", &names("zorblax")).is_empty());
    }

    #[test]
    fn each_name_reports_its_own_position_in_the_list() {
        let list = names("zorblax\nquuxcorp");
        assert_eq!(protected_name_hits("QuuxCorp", &list), vec![(1, 1)]);
    }
}

/// The account-ID detector, tested apart from the tree it scans.
///
/// Every fixture here is ASSEMBLED at runtime from four-digit pieces.
/// This file is itself scanned by the guard above, so a twelve-digit
/// literal written out in a test would make the guard fire on the
/// tests that prove it works.
#[cfg(test)]
mod account_ids {
    use super::{account_id_candidates, mask_account_id, AWS_DOC_ACCOUNT_IDS};

    /// Twelve digits that are not a placeholder or a repdigit.
    fn plausible() -> String {
        ["1234", "5678", "9013"].concat()
    }

    #[test]
    fn a_bare_id_is_found_with_its_line() {
        let id = plausible();
        let text = format!("first line\naccount = {id}\nlast");
        assert_eq!(account_id_candidates(&text), vec![(2, id.as_str())]);
    }

    /// The ARN half of the backlog item, which the digit rule covers
    /// without a second check: the account field sits between `:`s.
    #[test]
    fn the_account_field_of_an_arn_is_found() {
        let id = plausible();
        let arn = format!("arn:aws:iam::{id}:role/deploy");
        assert_eq!(account_id_candidates(&arn), vec![(1, id.as_str())]);
    }

    #[test]
    fn a_candidate_at_either_end_of_a_line_is_found() {
        let id = plausible();
        assert_eq!(account_id_candidates(&id), vec![(1, id.as_str())]);
        assert_eq!(
            account_id_candidates(&format!("\"{id}\"")),
            vec![(1, id.as_str())]
        );
    }

    #[test]
    fn aws_documentation_placeholders_are_not_candidates() {
        for id in AWS_DOC_ACCOUNT_IDS {
            let arn = format!("arn:aws:sts::{id}:assumed-role/x/y");
            assert!(
                account_id_candidates(&arn).is_empty(),
                "{id} is an AWS documentation placeholder"
            );
        }
    }

    #[test]
    fn repdigits_are_not_candidates() {
        for d in '0'..='9' {
            let rep: String = std::iter::repeat_n(d, 12).collect();
            assert!(account_id_candidates(&rep).is_empty(), "{rep}");
        }
    }

    /// Each of these was a live false positive, or is one shape away
    /// from one: `Cargo.lock` checksums are hex and hold twelve-digit
    /// runs between letters; `999999999999d` is a duration literal.
    #[test]
    fn digits_inside_a_larger_word_are_not_candidates() {
        let id = plausible();
        for text in [
            format!("checksum = \"ab{id}cd\""),
            format!("{id}d"),
            format!("x_{id}"),
            format!("{id}_x"),
            format!("{id}4"),     // thirteen digits
            format!("4{id}"),     // thirteen digits
            id[..11].to_string(), // eleven digits
        ] {
            assert!(account_id_candidates(&text).is_empty(), "{text}");
        }
    }

    /// The console shows an account ID as `1234-5678-9012`; pasted from
    /// there it is the same leak.
    #[test]
    fn the_consoles_dashed_form_is_found() {
        let id = plausible();
        let dashed = format!("{}-{}-{}", &id[..4], &id[4..8], &id[8..]);
        let text = format!("account: {dashed} (prod)");
        assert_eq!(account_id_candidates(&text), vec![(1, dashed.as_str())]);
        // Placeholders and repdigits, dashed, are still not candidates.
        assert!(account_id_candidates("1234-5678-9012").is_empty());
        assert!(account_id_candidates("7777-7777-7777").is_empty());
        // A longer dashed number, or one inside a word, is not one.
        for text in [
            format!("{dashed}-1111"),
            format!("2222-{dashed}"),
            format!("x{dashed}"),
            format!("{dashed}x"),
        ] {
            assert!(account_id_candidates(&text).is_empty(), "{text}");
        }
    }

    #[test]
    fn a_masked_id_locates_without_leaking() {
        let id = plausible();
        let masked = mask_account_id(&id);
        assert_eq!(masked, "12…13");
        assert!(!masked.contains(&id[2..10]));
    }

    /// Too short to mask meaningfully: reveal nothing rather than most
    /// of it. Unreachable from the detector, which only yields twelve
    /// digits — but it is a public helper and its edge is its contract.
    #[test]
    fn a_value_too_short_to_mask_reveals_nothing() {
        for short in ["", "1", "1234"] {
            assert_eq!(mask_account_id(short), "…", "{short:?}");
        }
    }
}
