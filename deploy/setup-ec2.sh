#!/usr/bin/env bash
# ==============================================================================
# Nexus Mesh Cloud Coordinator · AWS EC2 一键自动化部署脚本
# 适用系统: Ubuntu 22.04 / 24.04 LTS (x86_64 / arm64 Graviton)
# ==============================================================================

set -euo pipefail

DOMAIN="${1:-nexus.roviix.com}"
APP_DIR="/opt/nexus"

echo "========================================================"
echo "🚀 开始在 AWS EC2 部署 Nexus Mesh Coordinator"
echo "🌐 绑定域名: ${DOMAIN}"
echo "========================================================"

# 1. 检查 root 权限
if [ "$EUID" -ne 0 ]; then
  echo "❌ 请使用 sudo 权限运行此脚本: sudo bash $0 ${DOMAIN}"
  exit 1
fi

# 2. 系统依赖更新
echo "📦 正在安装系统基础依赖与 Caddy..."
apt-get update -y
apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl git ufw

# 3. 安装 Caddy (用于全自动申请并续期 Let's Encrypt SSL 证书)
if ! command -v caddy &> /dev/null; then
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | tee /etc/apt/sources.list.d/caddy-stable.list
  apt-get update -y
  apt-get install -y caddy
fi

# 4. 创建部署目录与服务文件
mkdir -p "${APP_DIR}"
mkdir -p /var/log/caddy

# 5. 配置 Caddyfile 反向代理与自动 HTTPS
cat <<EOF > /etc/caddy/Caddyfile
${DOMAIN} {
    encode gzip zstd

    # 反向代理至本地 nexus-broker 端口 8790
    reverse_proxy 127.0.0.1:8790 {
        header_up Host {host}
        header_up X-Real-IP {remote_host}
        header_up X-Forwarded-For {remote_host}
        header_up X-Forwarded-Proto {scheme}
    }

    log {
        output file /var/log/caddy/nexus.log
        format json
    }
}
EOF

# 6. 配置 systemd 守护进程
cat <<EOF > /etc/systemd/system/nexus-broker.service
[Unit]
Description=Nexus Mesh Cloud Coordinator Service
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=${APP_DIR}
ExecStart=${APP_DIR}/nexus-broker
Restart=always
RestartSec=3
Environment=NEXUS_PORT_HTTP=8790
Environment=NEXUS_PORT_TUNNEL=8791
Environment=NEXUS_BIND_ADDR=0.0.0.0
Environment=RUST_LOG=info,nexus_mesh=info
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl restart caddy

echo "========================================================"
echo "✅ Caddy 与系统守护进程已就绪！"
echo ""
echo "📋 接下来的步骤："
echo "1. 请将编译好的 nexus-broker 独立二进制文件放置到: ${APP_DIR}/nexus-broker"
echo "   chmod +x ${APP_DIR}/nexus-broker"
echo "2. 启动服务:"
echo "   systemctl enable --now nexus-broker"
echo "3. 检查运行状态:"
echo "   systemctl status nexus-broker"
echo "   curl -I https://${DOMAIN}/v1/mesh/health"
echo ""
echo "⚠️  请确保 AWS EC2 安全组 (Security Group) 放行以下端口:"
echo "   - TCP 80   (HTTP 证书校验)"
echo "   - TCP 443  (HTTPS API 与配对接口)"
echo "   - TCP 8791 (对等节点盲中继加密数据隧道)"
echo "   - UDP 3478 (STUN 打洞穿透)"
echo "========================================================"
