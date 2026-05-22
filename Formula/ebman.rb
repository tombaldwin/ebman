# Homebrew formula for ebman.
#
# Usage (until tap is published):
#   brew install --formula ./Formula/ebman.rb
#
# When a `v*` tag is pushed, the `release` workflow attaches per-target tarballs
# to the GitHub Release. The `url` / `sha256` fields below must be bumped to
# match each new release — `scripts/update-formula.sh` (not yet written) can
# be the home for that bumping later. The current numbers are stubs and will
# need updating before the formula resolves.
class Ebman < Formula
  desc "k9s-style TUI for AWS Elastic Beanstalk"
  homepage "https://github.com/tombaldwin/ebman"
  version "0.4.0"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "68176c4b357a7868d1453d77abb5a6a9cd92eda39f9a9ffc3244bdad35eb39ad"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "2c1b87fe194beee97a198ff5e9c32cee4cec69b4eca3c2df91a02595650103da"
    end
  elsif OS.linux?
    url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "4a5ec57e05c13eaa0e03424843e0579546c3dc5c66f64f1b8677f7b801f1b8f0"
  end

  depends_on "curl" # used by the live-log-tail S3 fetcher

  def install
    bin.install "ebman"
    prefix.install "README.md", "LICENSE-MIT", "LICENSE-APACHE"
  end

  test do
    assert_match "ebman #{version}", shell_output("#{bin}/ebman --version")
  end
end
