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
  version "0.30.2"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "f85eff19a667931020b05ca53a0c40283971a92642e1cce5f44e4bb13bec1bf1"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "71416803d4448beccf65dc281c0732af1827d6da91b89af4f3e6af6d1a6fcb30"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a4378c23a2f8035be5dc108767f127f4559e5e9beef97df7495301e68db3a7c1"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b04ec5a752e4da9bf741c785e8f0073f4ec6329d3ec75f59cae92eec9775befd"
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
