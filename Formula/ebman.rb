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
  version "0.29.1"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "73cd3b8d25cd0a4235da04472367b764321b574d284d521262a22ae56284e909"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "ba8d4e0aeb736f056dd6ec1d2c47a4b012a1dbb51545e17e5f962aba6daf8aba"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "afbefc7b4a77d8f89b8e000cf3f560f5126e53efe2a5b80dc96d90e0ab4b0948"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5fb778ce03c50af5cb0c41be362e8c06f193c4eb35e53b41ca994422c8425467"
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
