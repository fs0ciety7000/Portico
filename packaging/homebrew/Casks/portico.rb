# Template rendered by packaging/homebrew/render.sh into the tap
# (<owner>/homebrew-portico). Do not edit the copy in the tap by hand: the
# release workflow overwrites it. Placeholders: @VERSION@, @REPO_URL@,
# @DMG_SUFFIX@, @SHA_DMG@; the @CAVEATS@ line is replaced (or removed).
cask "portico" do
  version "@VERSION@"
  sha256 "@SHA_DMG@"

  url "@REPO_URL@/releases/download/v#{version}/Portico_#{version}_universal@DMG_SUFFIX@.dmg"
  name "Portico"
  desc "Local-first, keyboard-first SSH session manager"
  homepage "@REPO_URL@"

  livecheck do
    url :url
    strategy :github_latest
  end

  # minimumSystemVersion 13.0 in tauri.conf.json (a bare symbol means "this
  # version or later" for Homebrew).
  depends_on macos: :ventura

  app "Portico.app"

  # Portico's own files (hosts.toml, settings.toml, snippets, encrypted vault,
  # logs): directories::ProjectDirs::from("dev", "Portico", "portico"), shared
  # with the `portico` formula. WebKit and AppKit state use the bundle
  # identifier dev.portico.app. ~/.ssh is never touched, and secrets stored in
  # the login keychain are not removed (Keychain Access, service "dev.portico").
  zap trash: [
    "~/Library/Application Support/dev.Portico.portico",
    "~/Library/Caches/dev.portico.app",
    "~/Library/HTTPStorages/dev.portico.app",
    "~/Library/Preferences/dev.portico.app.plist",
    "~/Library/Saved Application State/dev.portico.app.savedState",
    "~/Library/WebKit/dev.portico.app",
  ]
  # @CAVEATS@
end
