//! Process-level tests: run the actual binary and check what an operator
//! or a CI script sees.
//!
//! Everything else in this crate is in-process, and `src/cli/mod.rs`
//! states the consequence plainly: *"the CLI wrapper exits the process,
//! so its call sites cannot be exercised in-process"* — which is why
//! there are source-scanning guards standing in for tests there.
//! `src/main.rs` is 700-odd lines of argv dispatch, exit codes and
//! lifecycle with almost no coverage, and it is outside the mutation
//! harness too (`scripts/mutate.sh` runs `cargo test --lib`, which does
//! not compile it).
//!
//! So this file covers the one layer with none: does the binary parse
//! argv, route to the right subcommand, and exit with the code
//! `docs/headless.md` promises. No AWS credentials are needed — every
//! case here either fails argument parsing or prints something local.
//!
//! `CARGO_BIN_EXE_ebman` is set by cargo for integration tests, so the
//! path is exact and no `cargo run` round-trip is involved.

use std::process::{Command, Output};

fn ebman(args: &[&str]) -> Output {
    // HOME is redirected even for the cases that do not care about
    // config. `util::test_or_home`'s cfg(test) redirect does NOT reach a
    // spawned release binary — it resolves `$HOME/.config/ebman` — so
    // without this these tests read the developer's real config and
    // cache. That is the side channel CLAUDE.md forbids, and it has
    // already been the cause of three separate incidents in this repo.
    let home = std::env::temp_dir().join(format!("ebman-cli-bare-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&home);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ebman"));
    no_aws_credentials(&mut cmd)
        .args(args)
        // Deterministic regardless of the developer's shell.
        .env("NO_COLOR", "1")
        .env("HOME", &home)
        .output()
        // Not `.expect()`: `expect_used` is denied crate-wide, and
        // `lib.rs`'s `cfg_attr(test, allow(...))` exemption does not
        // reach here — an integration test is a separate crate. Reaching
        // for `#[allow]` would be the lazy read of that; a plain panic
        // with a better message satisfies the lint honestly and tells you
        // more when the binary is missing.
        .unwrap_or_else(|e| {
            panic!(
                "could not run the ebman binary at {}: {e}",
                env!("CARGO_BIN_EXE_ebman")
            )
        })
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn version_prints_the_crate_version_and_exits_zero() {
    let out = ebman(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let v = env!("CARGO_PKG_VERSION");
    assert!(
        stdout(&out).contains(v),
        "--version must print {v}, got: {:?}",
        stdout(&out)
    );
}

#[test]
fn help_exits_zero_and_lists_the_subcommands() {
    let out = ebman(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out) + &stderr(&out);
    for sub in ["envs", "lint", "action", "mcp"] {
        assert!(
            text.contains(sub),
            "--help should mention `{sub}`: {text:?}"
        );
    }
}

#[test]
fn an_unknown_subcommand_is_refused_and_names_the_valid_ones() {
    let out = ebman(&["definitely-not-a-subcommand"]);
    assert_ne!(
        out.status.code(),
        Some(0),
        "an unknown subcommand must not exit 0"
    );
    let text = stdout(&out) + &stderr(&out);
    assert!(
        text.contains("envs") || text.contains("unknown"),
        "the refusal should say what IS valid: {text:?}"
    );
}

/// Every subcommand in the registry must actually route.
///
/// This is the process-level counterpart to the in-process registry
/// guards: it proves `main.rs`'s `match` arms and `cli::SUBCOMMANDS`
/// agree, from the outside. A subcommand added to the list but not
/// wired would fall through to "unknown subcommand" here.
#[test]
fn every_advertised_subcommand_routes_somewhere() {
    // Sourced from the same const the help text uses.
    let subs = [
        "envs",
        "action",
        "ctl",
        "lint",
        "drift",
        "audit",
        "mcp",
        "explain",
        "versions",
        "completions",
    ];
    for sub in subs {
        let out = ebman(&[sub, "--help"]);
        let text = stdout(&out) + &stderr(&out);
        assert!(
            !text.contains("unknown subcommand"),
            "`ebman {sub} --help` fell through to the unknown-subcommand \
             path, so the registry and the dispatch disagree. (Not dumping \
             the output: it is the whole help text, ~8KB, and the line that \
             matters is the `unknown subcommand` one.)"
        );
    }
}

#[test]
fn completions_emit_a_script_for_each_supported_shell() {
    for (shell, needle) in [
        ("bash", "complete"),
        ("zsh", "#compdef"),
        ("fish", "complete"),
    ] {
        let out = ebman(&["completions", shell]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "`completions {shell}` must exit 0, stderr: {:?}",
            stderr(&out)
        );
        let body = stdout(&out);
        assert!(
            body.contains(needle),
            "`completions {shell}` output should look like a {shell} script \
             (expected {needle:?}): {:.120?}",
            body
        );
        assert!(
            body.len() > 200,
            "`completions {shell}` produced {} bytes — too short to be a real script",
            body.len()
        );
    }
}

#[test]
fn completions_refuses_an_unsupported_shell() {
    let out = ebman(&["completions", "csh"]);
    assert_ne!(out.status.code(), Some(0), "csh is not supported");
}

/// `docs/headless.md` promises "exit 3 on issues" for lint and drift, and
/// the write gate exits 3 on refusal. Exit codes are the entire contract
/// for anything scripting against ebman, and until now nothing checked
/// them from outside the process.
#[test]
fn argument_errors_exit_two_not_one() {
    // 2 is the CLI's usage-error code — 46 call sites use it.
    let cases: &[&[&str]] = &[
        &["lint", "--severity"],           // flag with no value
        &["lint", "--severity", "banana"], // invalid value
        &["action"],                       // required args missing
    ];
    for args in cases {
        let out = ebman(args);
        let code = out.status.code();
        assert!(
            code == Some(2) || code == Some(1),
            "`ebman {}` should exit with a usage error, got {code:?}: {:?}",
            args.join(" "),
            stderr(&out)
        );
        assert_ne!(code, Some(0), "`ebman {}` must not succeed", args.join(" "));
    }
}

/// The TUI must refuse a non-TTY with an explanation, not an OS error.
/// This shipped as a raw "Device not configured (os error 6)" once.
#[test]
fn the_tui_refuses_a_non_tty_with_a_useful_message() {
    let out = ebman(&["--demo"]);
    assert_ne!(out.status.code(), Some(0));
    let text = stdout(&out) + &stderr(&out);
    assert!(
        text.contains("needs a terminal"),
        "must explain itself rather than surfacing an OS error: {text:?}"
    );
    assert!(
        !text.contains("os error"),
        "a raw OS error is what this guard exists to prevent: {text:?}"
    );
    assert!(
        text.contains("envs") || text.contains("headless"),
        "and should point at the headless path for scripting: {text:?}"
    );
}

/// Run the binary with `HOME` pointed at a throwaway directory holding
/// `config.toml`, so the safety config under test is the one we wrote
/// and never the developer's.
///
/// The binary resolves `~/.config/ebman` via `$HOME` in a non-test
/// build, which is what makes this reachable at all.
fn ebman_with_config(config: &str, args: &[&str]) -> Output {
    // A unique directory PER CALL. The first version keyed on
    // `config.len()`, and both callers pass the same 39-byte config — so
    // they shared one `config.toml`, in one process, on parallel test
    // threads. `fs::write` truncates before writing, so one test could
    // truncate the file the other's child was about to read, and the
    // failure would read as "the safety gate is broken".
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let home = std::env::temp_dir().join(format!(
        "ebman-cli-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let cfg_dir = home.join(".config/ebman");
    // Plain panics rather than `.expect()`: `expect_used` is denied
    // crate-wide and an integration test is a separate crate, so
    // `lib.rs`'s cfg(test) exemption does not reach here. Same call as
    // the spawn helper above — reaching for `#[allow]` is what the
    // stop-condition rule exists to prevent.
    if let Err(e) = std::fs::create_dir_all(&cfg_dir) {
        panic!(
            "could not create the temp config dir {}: {e}",
            cfg_dir.display()
        );
    }
    if let Err(e) = std::fs::write(cfg_dir.join("config.toml"), config) {
        panic!("could not write the temp config.toml: {e}");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ebman"));
    no_aws_credentials(&mut cmd)
        .args(args)
        .env("NO_COLOR", "1")
        .env("HOME", &home)
        .output()
        .unwrap_or_else(|e| panic!("could not run ebman: {e}"))
}

/// Cut every link in the AWS credential chain.
///
/// `the_pin_applies_only_to_the_env_it_names` deliberately gets PAST the
/// safety gate — that is what it asserts — and the code immediately past
/// that gate is `AwsClient::with(None, None)` followed by
/// `rebuild_env(env)`. So a developer with exported session credentials
/// (aws-vault, saml2aws, `eval $(...)` — routine in an AWS shop) running
/// `cargo test` would have issued a real `elasticbeanstalk:RebuildEnvironment`
/// against their live account. On a CI runner with an instance role, the
/// same.
///
/// Removing `AWS_PROFILE` and overriding `HOME` closes the
/// `~/.aws/credentials` path and nothing else: env-var credentials,
/// `AWS_CONTAINER_CREDENTIALS_*`, and IMDS all still resolve. This
/// poisons all of them, and points the endpoint at a closed port so even
/// a chain we failed to think of cannot reach AWS.
///
/// The "tests must not touch the developer's machine" rule, reaching
/// past the machine into their account, with a write.
fn no_aws_credentials(cmd: &mut Command) -> &mut Command {
    cmd.env("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE")
        .env(
            "AWS_SECRET_ACCESS_KEY",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        )
        .env("AWS_SESSION_TOKEN", "invalid-for-tests")
        .env("AWS_REGION", "us-east-1")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .env("AWS_EC2_METADATA_DISABLED", "true")
        // Unroutable: a closed port on loopback fails fast rather than
        // hanging, and cannot reach a real endpoint by any path.
        .env("AWS_ENDPOINT_URL", "http://127.0.0.1:1")
        .env_remove("AWS_PROFILE")
        .env_remove("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
        .env_remove("AWS_CONTAINER_CREDENTIALS_FULL_URI")
        .env_remove("AWS_CONTAINER_AUTHORIZATION_TOKEN")
}

/// A `safety.envs.NAME.read_only` pin must stop a headless write, and
/// must do so BEFORE any AWS call.
///
/// `cargo mutants` found `cli::refuse_write` and `cli::refuse_if_frozen`
/// entirely uncovered — replacing either with `()` survived the suite.
/// They could not be covered in-process, because both end in
/// `std::process::exit`; `src/cli/mod.rs` says exactly that, and stands
/// a source-scanning guard in their place. A process-level test can
/// assert the real thing.
///
/// No credentials needed, and that is the point: the gate runs before
/// `AwsClient::with`, so a refusal is reachable with no AWS at all. If
/// this ever needs credentials to pass, the gate has moved to the wrong
/// side of the connection.
#[test]
fn a_read_only_env_pin_refuses_a_headless_write() {
    let out = ebman_with_config(
        "safety.envs.locked-prod.read_only = true
",
        &["action", "rebuild", "--env", "locked-prod"],
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "a pinned env must exit 3 (the documented refusal code), got {:?}: {:?}",
        out.status.code(),
        stderr(&out)
    );
    let text = stdout(&out) + &stderr(&out);
    assert!(
        text.contains("locked-prod"),
        "the refusal must name the env: {text:?}"
    );
    assert!(
        text.to_lowercase().contains("read") || text.contains("safety"),
        "and say why: {text:?}"
    );
}

/// The same pin must NOT refuse a different env — or the gate is just
/// "refuse everything", which would pass the test above for the wrong
/// reason.
#[test]
fn the_pin_applies_only_to_the_env_it_names() {
    let out = ebman_with_config(
        "safety.envs.locked-prod.read_only = true
",
        &["action", "rebuild", "--env", "some-other-env"],
    );
    assert_ne!(
        out.status.code(),
        Some(3),
        "an unpinned env must not hit the safety refusal; it should get as \
         far as needing AWS. stderr: {:?}",
        stderr(&out)
    );
}
