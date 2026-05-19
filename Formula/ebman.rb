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
  version "0.1.1"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "186e3652c80d958d0ee21dfe97f57654fcd044216cbb1a6a7dfb4103381ad181"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "6f3018418eaefc692199d135c765da0b79886ff7463e4635d18af7092bf90225"
    end
  elsif OS.linux?
    url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "9ca316fa892346a05e90ba509edb32368de72a77bc209dabdc7f11045ea8f4e4"
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
