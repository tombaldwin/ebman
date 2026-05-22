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
      sha256 "8f9a4852a9a4cfcfb2eb364768e4828ad631d33ab8bdeef587ea6f2f6ef459f4"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "69ffd2ba7957eb5aff281797fb538d827a37ed08a1101c17ae579c00dfc432df"
    end
  elsif OS.linux?
    url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "69e6b3514c99a1d52ae755b3fe77ca88128f5919d02d46dfada040bfd74c33be"
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
