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
  version "0.47.1"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "83051436de53d4fc52ad5452272d448791641d83109404402e26bef31fd57b10"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "23d18f2bb95d2c6c4ece84aa672720b871165ee2d1130a2bd6eab364757c8d0a"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "ab295a4e025e56c2832b9d1fb1ee0d93c4ec0d0a01d49f6774e6c289e5b8941f"
    else
      url "https://github.com/tombaldwin/ebman/releases/download/v#{version}/ebman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "077ec7feb4b64f324d96e3a386857fd6c5f43539195bb5ef6504ac1eee7c5179"
    end
  end

  depends_on "curl" # used by the live-log-tail S3 fetcher

  def install
    bin.install "ebman"
    generate_completions_from_executable(bin/"ebman", "completions")
    prefix.install "README.md", "LICENSE-MIT", "LICENSE-APACHE"
  end

  test do
    assert_match "ebman #{version}", shell_output("#{bin}/ebman --version")
  end
end
