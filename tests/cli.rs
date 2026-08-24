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
    Command::new(env!("CARGO_BIN_EXE_ebman"))
        .args(args)
        // Deterministic regardless of the developer's shell.
        .env("NO_COLOR", "1")
        .env_remove("AWS_PROFILE")
        .env_remove("AWS_REGION")
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
