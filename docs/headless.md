# Headless interface (`--control-socket`)

Launch ebman with `--control-socket PATH` to expose a Unix-socket interface. A second binary, `ebman ctl <op>`, is the one-shot client (defaults to `~/.cache/ebman/control.sock`).

```bash
ebman ctl state                   # JSON: mode, profile, region, account, envs, selected, ...
ebman ctl screen                  # plain-text dump of the current frame
ebman ctl key Down                # synthesise a keypress
ebman ctl key Ctrl+R              # … or a combo
ebman ctl cmd ':region eu-west-2' # run a : command
```

Useful for integration tests, screenshot capture, scripted workflows.

There's also a non-interactive path that doesn't need a running instance:

```bash
ebman envs --json                      # print env list as JSON
ebman action rebuild --env myenv --yes # dispatch a rebuild
```
