# Template rendered by packaging/homebrew/render.sh into the tap
# (<owner>/homebrew-portico). Do not edit the copy in the tap by hand: the
# release workflow overwrites it. Placeholders: @VERSION@, @REPO_URL@, @SHA_*@.
class Portico < Formula
  desc "Local-first, keyboard-first SSH session manager (terminal UI)"
  homepage "@REPO_URL@"
  license any_of: ["MIT", "Apache-2.0"]

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    on_arm do
      url "@REPO_URL@/releases/download/v@VERSION@/portico-@VERSION@-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA_AARCH64_APPLE_DARWIN@"
    end
    on_intel do
      url "@REPO_URL@/releases/download/v@VERSION@/portico-@VERSION@-x86_64-apple-darwin.tar.gz"
      sha256 "@SHA_X86_64_APPLE_DARWIN@"
    end
  end

  on_linux do
    on_arm do
      url "@REPO_URL@/releases/download/v@VERSION@/portico-@VERSION@-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA_AARCH64_UNKNOWN_LINUX_GNU@"
    end
    on_intel do
      url "@REPO_URL@/releases/download/v@VERSION@/portico-@VERSION@-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA_X86_64_UNKNOWN_LINUX_GNU@"
    end
  end

  def install
    bin.install "portico"
  end

  test do
    assert_match "portico #{version}", shell_output("#{bin}/portico --version")
  end
end
