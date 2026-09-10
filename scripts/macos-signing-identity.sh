#!/usr/bin/env bash
# 本机代码签名身份，给 build-dmg.sh 用。打印一个 codesign 认的身份名；没有就造一张自签证书。
#
#   scripts/macos-signing-identity.sh            # 找 / 造，打印身份名
#   scripts/macos-signing-identity.sh --show     # 只找，不造；没有则退出码 1
#
# 选择顺序：
#   1. 环境变量 NEXUS_SIGNING_IDENTITY（显式指定）
#   2. 钥匙串里的 "Developer ID Application: …"（真正能对外分发的那种）
#   3. 钥匙串里的 "Apple Development: …"（Xcode 登录 Apple ID 后自动有的；本机自用够了）
#   4. 自签的 "Nexus Local Signing"（没有就现造）
#
# 为什么必须签（2026-09-07 的教训）：
#
# - Tauri 打出来的 Nexus.app 若不签名，bundle 里没有 _CodeSignature，只有链接器给可执行文件
#   加的 `adhoc, linker-signed`。macOS 的「App 管理」（Sand 补丁要改 Cursor.app 用的）把授权
#   绑在申请方的 designated requirement 上；没签名就算不出这个东西，弹窗点了允许也存不住，
#   于是「一直申请、一直没权限」。
# - 手动 `codesign --sign -`（ad-hoc）能算出 requirement，但它是 cdhash → 每次重新构建都变，
#   每次更新都要重新授权。
# - 用证书签，requirement 绑到证书 / Team ID，跨版本稳定：授权一次，以后更新照旧有效。
#
# 代价说清：Gatekeeper 只认 Developer ID + 公证。Apple Development 与自签证书打的包，本机构建、
# 本机安装时没有隔离属性，不会被拦；拷到别的机器上会被拦。对外分发走发布手册里那条路。
set -euo pipefail

self_signed="Nexus Local Signing"
keychain="$HOME/Library/Keychains/login.keychain-db"

# `security find-identity -v -p codesigning` 的输出形如：
#   1) 82AA…24 "Apple Development: shangwu zhong (HKL79WN5P6)"
identities() {
  security find-identity -v -p codesigning 2>/dev/null | sed -n 's/^ *[0-9]*) [0-9A-F]* "\(.*\)"$/\1/p'
}

pick() {
  local all
  all="$(identities)"
  if [ -n "${NEXUS_SIGNING_IDENTITY:-}" ]; then
    if printf '%s\n' "$all" | grep -Fx -- "$NEXUS_SIGNING_IDENTITY" >/dev/null; then
      echo "$NEXUS_SIGNING_IDENTITY"
      return 0
    fi
    echo "钥匙串里没有 NEXUS_SIGNING_IDENTITY 指定的身份「$NEXUS_SIGNING_IDENTITY」" >&2
    return 1
  fi
  local hit
  for prefix in "Developer ID Application:" "Apple Development:"; do
    hit="$(printf '%s\n' "$all" | grep -F -- "$prefix" | head -1 || true)"
    if [ -n "$hit" ]; then
      echo "$hit"
      return 0
    fi
  done
  if printf '%s\n' "$all" | grep -Fx -- "$self_signed" >/dev/null; then
    echo "$self_signed"
    return 0
  fi
  return 1
}

if id="$(pick)"; then
  echo "$id"
  exit 0
fi
if [ "${1:-}" = "--show" ]; then
  echo "没有可用的代码签名身份" >&2
  exit 1
fi

for bin in openssl security; do
  command -v "$bin" >/dev/null 2>&1 || { echo "缺 $bin" >&2; exit 2; }
done

echo "· 钥匙串里没有可用的签名身份，造一张自签的「$self_signed」" >&2
tmp="$(mktemp -d "${TMPDIR:-/tmp}/nexus-sign.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

# Code Signing 证书：EKU 必须是 codeSigning，否则 codesign 找不到它；十年有效，到期重跑本脚本。
cat > "$tmp/openssl.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = v3
prompt = no
[dn]
CN = $self_signed
O = Roviix
[v3]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
EOF
openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
  -keyout "$tmp/key.pem" -out "$tmp/cert.pem" -config "$tmp/openssl.cnf" >/dev/null 2>&1

# 导出成 .p12 再导入：security import 只认这种形态带私钥。OpenSSL 3 默认的 PKCS12 加密
# （AES + PBKDF2）macOS 读不了（报 "MAC verification failed"），要 -legacy；老 openssl 没这个开关。
if ! openssl pkcs12 -export -legacy -inkey "$tmp/key.pem" -in "$tmp/cert.pem" -out "$tmp/identity.p12" \
  -name "$self_signed" -passout pass:nexus-local >/dev/null 2>&1; then
  openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" -out "$tmp/identity.p12" \
    -name "$self_signed" -passout pass:nexus-local >/dev/null 2>&1
fi

# -T：允许 codesign 直接用这把私钥；否则每次签名系统都会弹「是否允许访问钥匙串」。
security import "$tmp/identity.p12" -k "$keychain" -P nexus-local \
  -T /usr/bin/codesign -T /usr/bin/security -T /usr/bin/productbuild >/dev/null

# 自签证书要被信任为可签代码，codesign 才认它是有效身份（用户级信任设置，不需要 sudo；
# 系统可能弹一次要密码的窗）。
if ! security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$tmp/cert.pem" 2>/dev/null; then
  echo "· 信任设置没写成（可能被取消了）。在「钥匙串访问」里找到「$self_signed」→ 显示简介 → 信任 → 代码签名：始终信任。" >&2
fi

if id="$(pick)"; then
  echo "$id"
else
  echo "导入了证书，但 codesign 还看不到身份「$self_signed」。检查「钥匙串访问 → 登录 → 我的证书」。" >&2
  exit 1
fi
