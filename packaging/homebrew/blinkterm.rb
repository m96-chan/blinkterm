# The canonical copy of the Homebrew formula. The one in the tap
# (https://github.com/m96-chan/homebrew-tap, Formula/blinkterm.rb) is a copy
# of this file: a change is made here, built by .github/workflows/homebrew.yml
# before it is merged, and copied over on a release (RELEASING.md, "Homebrew").
class Blinkterm < Formula
  desc "Real browser in a terminal: headless Chromium over the Kitty graphics protocol"
  homepage "https://github.com/m96-chan/blinkterm"
  # GitHub's archive of the tag; RELEASING.md ("Homebrew") says how both lines
  # are made on each release. `head` stays below, so `--HEAD` keeps installing
  # main.
  url "https://github.com/m96-chan/blinkterm/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "84cbea74248e2ec64ae57b8c06f181032c8528ea87c166ee6167b4236b64feef"
  license "MIT"
  head "https://github.com/m96-chan/blinkterm.git", branch: "main"

  # No platform requirement: Linux and macOS both, the engine tests having
  # run on each (ci.yml's `engine` and `mac` jobs), and homebrew.yml builds
  # and tests this formula on both.
  depends_on "rust" => :build

  def install
    # std_cargo_args is `--jobs N --locked --root=#{prefix} --path=.`.
    # --locked matters more here than in most crates: every dependency but
    # libc is a git revision of tOS and Cargo.lock is the only record of which
    # tree was built. cargo fetches those from github.com during the build;
    # Homebrew allows a build network access unless a formula says otherwise,
    # under Linux's Landlock sandbox as on macOS, and this one does not.
    system "cargo", "install", *std_cargo_args
  end

  def caveats
    <<~EOS
      blinkterm does not ship a browser engine. It looks at $BLINKTERM_ENGINE
      first, then on PATH for chrome-headless-shell, chromium, chromium-browser,
      google-chrome and chromium-shell, in that order, and on macOS then for
      Google Chrome or Chromium in /Applications or ~/Applications. The one it
      is tested against is chrome-headless-shell from Chrome for Testing:

        https://googlechromelabs.github.io/chrome-for-testing/

      Unzip the build for your platform (linux64, mac-arm64, mac-x64)
      somewhere and point at it:

        export BLINKTERM_ENGINE=/opt/chrome-headless-shell-linux64/chrome-headless-shell
        export BLINKTERM_ENGINE=~/engine/chrome-headless-shell-mac-arm64/chrome-headless-shell

      On macOS, a zip saved by a browser rather than fetched with curl is
      quarantined and the shell will not start until the attribute is removed:

        xattr -dr com.apple.quarantine chrome-headless-shell-mac-arm64

      The README's "Installing" section has the exact download and the
      libraries it wants. Debian's chromium-shell package is Chromium's
      content_shell, not a headless shell: it keeps a DevTools port open beside
      the pipe and does not close when asked, so a profile is never flushed.
      It is last on the list for that reason.

      A terminal that speaks the Kitty graphics protocol, the Kitty keyboard
      protocol and SGR mouse reporting is needed: Kitty, WezTerm, Ghostty or a
      tOS pane.
    EOS
  end

  test do
    # Neither needs an engine or a terminal: --version and --help are answered
    # before either is looked for. A regex rather than `version.to_s`, which is
    # "HEAD" under --HEAD.
    assert_match(/^blinkterm \d+\.\d+\.\d+/, shell_output("#{bin}/blinkterm --version"))
    assert_match "--profile", shell_output("#{bin}/blinkterm --help")
  end
end
