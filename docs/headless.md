# Headless interface

ebman ships two scriptable surfaces: a **control socket** for driving a running TUI, and **non-interactive subcommands** that don't need a running instance.

## `ebman ctl` — drive a running TUI

Launch ebman with `--control-socket PATH` to expose a Unix-socket interface. The `ebman ctl <op>` subcommand is the one-shot client (defaults to `~/.cache/ebman/control.sock`; override with `--socket PATH`).

```bash
ebman ctl state                   # JSON: mode, profile, region, account, envs, selected, ...
ebman ctl screen                  # plain-text dump of the current frame
ebman ctl key Down                # synthesise a keypress
ebman ctl key Ctrl+R              # … or a combo
ebman ctl cmd ':region eu-west-2' # run a : command (leading : optional)
```

Useful for integration tests, screenshot capture, scripted workflows.

## Non-interactive subcommands

These don't need a running TUI — they connect to AWS, do their thing, and exit. CI-friendly.

```bash
ebman envs --json                                                          # print env list as JSON
ebman action rebuild --env myenv --yes                                     # dispatch a rebuild
ebman action rollout --version LABEL --env NAME --regions r1,r2,r3 --yes   # sequential cross-region deploy
ebman lint   [--env NAME] [--regions r1,r2,r3] [--json]                    # rule-engine diagnostics; exit 3 on issues
ebman drift  [--env NAME] [--regions r1,r2,r3] [--tfstate PATH] [--json]   # terraform drift report; exit 3 on drift
```

Exit-code convention (CI scripts can branch on these): `0` clean, `1` AWS-layer error, `2` usage error, `3` issues / drift found.
