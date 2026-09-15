#!/usr/bin/env bash
# 打一份 macOS DMG（本机签名，自用）。
#
#   scripts/build-dmg.sh              # 构建 + 签名 + 校验，打印产物路径
#   scripts/build-dmg.sh --install    # 再把 Nexus.app 装进 /Applications（会先退出正在跑的 Nexus）
#   scripts/build-dmg.sh --open       # 构建完在访达里选中 dmg
#   NEXUS_NO_SIGN=1 scripts/build-dmg.sh   # 不签名（只为排障；不签的包拿不到「App 管理」权限）
#
# 做的事：前端 typecheck + vite build（tauri 的 beforeBuildCommand）→ cargo release 编译 →
# 用本机签名身份（scripts/macos-signing-identity.sh 挑：Developer ID > Apple Development > 自签）
# 给 .app 签名 → 出 app + dmg（不出 nsis/msi，省时间）→ 校验签名并打印 designated requirement。
# 产物在 target/release/bundle/dmg/。首次或清过 target 后要编全量 Rust，十来分钟；
# 之后是增量，一两分钟。
#
# 「签名」在这里解决的是一件具体的事：macOS「App 管理」权限（Sand 补丁改 Cursor.app 要用）
# 绑在申请方的 designated requirement 上，没签名的包算不出它，弹窗点了允许也存不住。用证书签
# 之后 requirement 跨版本稳定，授权一次以后更新照旧有效。不做公证：Gatekeeper 对本机构建、
# 本机安装的包不设卡；拷去别的机器仍会被拦，对外发布走 .github/workflows/release.yml 那条路。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
desktop="$(cd "$here/.." && pwd)"
app="$desktop/apps/desktop"
install=0
open_after=0
for arg in "$@"; do
  case "$arg" in
    --install) install=1 ;;
    --open) open_after=1 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "不认识的参数：$arg" >&2; exit 2 ;;
  esac
done

need() { command -v "$1" >/dev/null 2>&1 || { echo "缺 $1" >&2; exit 2; }; }
need node
need npm
need cargo

# 磁盘：看 cargo 真正写的那卷（CARGO_TARGET_DIR 或 desktop/target，含 symlink），
# 不是仓库所在盘。release 增量一般要 2–3 GB 余量，低于 4 GB 先提醒。
target_dir="${CARGO_TARGET_DIR:-$desktop/target}"
mkdir -p "$target_dir"
target_vol=$(cd "$target_dir" && pwd -P)
avail_kb=$(df -k "$target_vol" | awk 'NR==2 {print $4}')
if [ "$avail_kb" -lt $((4 * 1024 * 1024)) ]; then
  echo "构建盘（$target_vol）只剩 $((avail_kb / 1024 / 1024)) GB，release 构建可能中途失败；先清 target/debug 或把 CARGO_TARGET_DIR 指到空盘再来。" >&2
  exit 3
fi

[ -d "$app/node_modules" ] || (cd "$app" && npm install)

# 签名身份。Tauri 的 bundler 读 APPLE_SIGNING_IDENTITY 决定要不要 codesign；没有 APPLE_ID /
# APPLE_API_KEY 这些就不会去公证，这正是我们要的。
identity=""
if [ "${NEXUS_NO_SIGN:-}" != "1" ]; then
  identity="$("$here/macos-signing-identity.sh")"
  export APPLE_SIGNING_IDENTITY="$identity"
  echo "· 签名身份：$identity"
else
  echo "· NEXUS_NO_SIGN=1：不签名（这份包拿不到「App 管理」权限）"
fi

# 上一版 DMG 还挂着（双击打开过）会让 bundle_dmg.sh 撞同名卷而失败，先卸掉；
# 中断过的构建会留下 rw.*.dmg 临时镜像，一并清掉。
if [ -d /Volumes/Nexus ]; then
  echo "· 卸载残留的 /Volumes/Nexus"
  hdiutil detach /Volumes/Nexus -force >/dev/null 2>&1 || true
fi
rm -f "$desktop"/target/release/bundle/macos/rw.*.dmg

# 要 app 和 dmg 两种：只要 dmg 的话 Tauri 打完会把中间的 Nexus.app 清掉，
# 后面的签名校验和 --install 都要用它。
echo "▶ tauri build --bundles app dmg（$(date '+%H:%M:%S')）"
(cd "$app" && npm run build -- --bundles app dmg)

dmg=$(ls -t "$desktop"/target/release/bundle/dmg/*.dmg 2>/dev/null | head -1 || true)
bundle="$desktop/target/release/bundle/macos/Nexus.app"
if [ -z "$dmg" ] || [ ! -d "$bundle" ]; then
  echo "没找到 dmg / app 产物，看上面的构建输出。" >&2
  exit 1
fi

echo
echo "✔ $dmg"
echo "  $(du -h "$dmg" | cut -f1)，$(date -r "$dmg" '+%Y-%m-%d %H:%M:%S')"

if [ -n "$identity" ]; then
  echo "▶ 校验签名"
  if ! codesign --verify --deep --strict --verbose=2 "$bundle" 2>&1 | sed 's/^/  /'; then
    echo "签名校验没过：这份包装上去会拿不到「App 管理」权限。" >&2
    exit 1
  fi
  echo "  designated requirement："
  codesign -d -r- "$bundle" 2>&1 | sed -n 's/^designated => /    /p'
  echo "  （requirement 不带 cdhash、绑到证书 / Team ID，才是跨版本稳定的）"
else
  echo "  未签名：首次打开若被 Gatekeeper 拦，右键 → 打开，或 xattr -dr com.apple.quarantine /Applications/Nexus.app"
fi

if [ "$install" = 1 ]; then
  echo "▶ 安装到 /Applications"
  if pgrep -f 'Nexus.app/Contents/MacOS/' >/dev/null 2>&1; then
    echo "  退出正在跑的 Nexus"
    osascript -e 'tell application "Nexus" to quit' >/dev/null 2>&1 || true
    for _ in $(seq 1 20); do
      pgrep -f 'Nexus.app/Contents/MacOS/' >/dev/null 2>&1 || break
      sleep 1
    done
    pkill -f 'Nexus.app/Contents/MacOS/' >/dev/null 2>&1 || true
  fi
  # ditto 保留签名与扩展属性；先拷到旁边再整体换，别在半路留一个损坏的包。
  staging="/Applications/.Nexus.app.new"
  rm -rf "$staging"
  ditto "$bundle" "$staging"
  rm -rf /Applications/Nexus.app
  mv "$staging" /Applications/Nexus.app
  xattr -dr com.apple.quarantine /Applications/Nexus.app 2>/dev/null || true
  codesign --verify --deep --strict /Applications/Nexus.app
  echo "  ✔ /Applications/Nexus.app（$(defaults read /Applications/Nexus.app/Contents/Info.plist CFBundleShortVersionString 2>/dev/null || echo '?')）"
  echo "  打开：open -a Nexus"
fi

if [ "$open_after" = 1 ]; then
  open -R "$dmg"
fi
