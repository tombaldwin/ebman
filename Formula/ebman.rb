# Homebrew formula for ebman — k9s-style TUI for AWS Elastic Beanstalk.
#
# This in-repo copy is for `brew install --formula ./Formula/ebman.rb`
# (local-checkout install). The canonical user-facing install path is
# the tap at https://github.com/tombaldwin/homebrew-tap:
#   brew tap tombaldwin/tap
#   brew install ebman
#
# Bumping for a new release: run `scripts/update-formula.sh vX.Y.Z` —
# it computes SHA-256s from the GitHub Release tarballs and writes
# both this file and the tap's Formula/ebman.rb in one go.
class Ebman < Formula
  desc "k9s-style TUI for AWS Elastic Beanstalk"
  homepage "https://github.com/tombaldwin/ebman"
  version "0.36.0"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "30cd1dabf0f9543ff756bf20905949282fc2a0dc47590f3ff0f47f046ff75b63"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "5c00081db47ad83f2881db1560eeef4b89017cfb00efa9727ce8483a13d6870f"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "e618cdfea0fba25659d341a106590c5b888f50d2969060b94dc7acef649c0c8d"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "2ebb9b3ef1ef149246251f0f71096646b729b025521be5fbdde363891f06bd20"
    end
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
