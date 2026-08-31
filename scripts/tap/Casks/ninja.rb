# 由 scripts/package_dmg.sh 生成——不要手改（version/sha256/url 钉 DMG 实物）。
# 本地验证链：cp -R scripts/tap <tap 仓库目录> && git init+commit &&
#   brew tap ninja/local <tap 仓库目录> && brew install --cask ninja
# （url 走 file:// 本地路径——DMG 公开托管是后续决定，见 DISTRIBUTION.md；
#   Gatekeeper/隔离语义见本 tap 的 README.md 与 DISTRIBUTION.md。）
cask "ninja" do
  version "0.1.0"
  sha256 "20d9a212dbec2411149abaae14d0d0cefb6d7572967c48414ca55051ecc90944"

  url "file:///Users/jal/my_repos/ninja/dist/Ninja-0.1.0-arm64.dmg"
  name "Ninja"
  desc "ADE plugin host terminal on vendored libghostty"
  homepage "https://example.invalid/ninja-not-public"

  app "Ninja.app"
end
