# Development

```bash
cargo build
cargo test
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

See `BACKLOG.md` for in-flight and planned work. See `CLAUDE.md` for the AI-assisted-contributor rules (the project has been developed heavily with Claude Code).

## Distribution

- **Cargo**: `cargo install ebman` from crates.io; `cargo install --path .` from a checkout.
- **GitHub Releases**: tagging `v<X.Y.Z>` triggers `.github/workflows/release.yml`, which builds release binaries for `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, and `x86_64-apple-darwin` and attaches tarballs + SHA-256 checksums to a draft release.
- **Homebrew**: tap lives at [`tombaldwin/homebrew-tap`](https://github.com/tombaldwin/homebrew-tap). Per-release: bump the version + 3 platform SHAs in `Formula/ebman.rb` in both this repo (for `brew install --formula PATH`) and the tap.
