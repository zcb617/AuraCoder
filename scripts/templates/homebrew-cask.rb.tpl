cask "auracoder" do
  version "__VERSION__"
  sha256 "__SHA256__"

  url "__URL__"
  name "AuraCoder"
  desc "Local-first cockpit for AI-assisted coding"
  homepage "https://github.com/wygoralves/auracoder"

  app "AuraCoder.app"

  postflight do
    # Best-effort friction reduction for unsigned / unnotarized builds.
    system_command "/usr/bin/xattr",
      args: ["-dr", "com.apple.quarantine", "#{appdir}/AuraCoder.app"]
  end

  zap trash: [
    "~/.agent-workspace",
  ]
end
