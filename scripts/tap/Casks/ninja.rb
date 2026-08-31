# 由 scripts/package_dmg.sh 生成——不要手改（version/sha256/url 钉 DMG 实物）。
# 本地验证链：cp -R scripts/tap <tap 仓库目录> && git init+commit &&
#   brew tap ninja/local <tap 仓库目录> && brew install --cask ninja
# （url 走 file:// 本地路径——DMG 公开托管是后续决定，见 DISTRIBUTION.md；
#   Gatekeeper/隔离语义见本 tap 的 README.md 与 DISTRIBUTION.md。）
cask "ninja" do
  version "0.1.0"
  sha256 "2b7aee79abefba652cfb787554f494fcc55738e1bd5ed87fa72274a1279444bc"

  url "file:///Users/jal/my_repos/ninja/dist/Ninja-0.1.0-arm64.dmg"
  name "Ninja"
  desc "ADE plugin host terminal on vendored libghostty"
  homepage "https://example.invalid/ninja-not-public"

  app "Ninja.app"
end
