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
  version "0.3.1"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "1b97e600a45bb8693c6965ee6a0aab4ab81ee4330894e7fd2b58fb713b05eff0"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "0d2145fba64280d9612019f970cb0dca6a8f26ba3d9da2960bc5bbcea7b90e16"
    end
  elsif OS.linux?
    url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "898f94cddf459fd8606f5d26c88694428086851f53582610f1e34a13712bdea9"
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
