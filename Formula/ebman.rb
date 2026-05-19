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
  version "0.1.0"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "a4615b581aacf5e26ebdf0925458adaa27c618530e9957fec0733aca53ccbc74"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "1d16db93bd1007a4f90c9c0b6a8ce2affad43c5d2ea98f8a486c030e9d18d239"
    end
  elsif OS.linux?
    url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "5ae19b1c15e6b7dd6d6fbe04c63c6d22ff43405e5368d5b5a98cc6e179ee816b"
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
