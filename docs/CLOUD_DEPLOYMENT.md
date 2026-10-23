# Nexus Mesh 云端中继协调器 (Broker) AWS EC2 部署指南

> **目标**：在 AWS EC2 上部署高可用、免运维的 `nexus-broker` 协调中继服务，绑定自定义子域名 `nexus.roviix.com`，提供端到端自动 SSL、设备配对码解析以及加密盲中继数据转发。

---

## 一、架构全景与职责划分

```text
┌────────────────────────────────┐                             ┌────────────────────────────────┐
│   节点 A (家中台式机 / 提供者)   │                             │   节点 B (出差笔记本 / 使用者)   │
│   • 运行 Nexus 客户端           │                             │   • 运行 Nexus 客户端           │
│   • 住宅网络 IP 出口            │                             │   • 输入配对码 NX-8839          │
└──────────────┬─────────────────┘                             └───────────────┬────────────────┘
               │                                                               │
               │   1. 注册配对码 NX-8839           2. 查询配对码 NX-8839         │
               │   (HTTPS: nexus.roviix.com)       (HTTPS: nexus.roviix.com)   │
               ▼                                                               ▼
       ┌───────────────────────────────────────────────────────────────────────────────┐
       │              AWS EC2 云端节点 (nexus.roviix.com / 固定弹性 IP)                  │
       │                                                                               │
       │   • Caddy (端口 80 / 443): 自动向 Let's Encrypt 申请并续期泛域名/单域名 SSL    │
       │   • nexus-broker (端口 8790): 临时配对码注册表 (10min TTL) 与心跳探测           │
       │   • 盲中继隧道 (TCP 端口 8791): 对等加密数据穿透盲转发 (零解密、零存储)       │
       └───────────────────────────────────────────────────────────────────────────────┘
               │                                                               │
               ▲═══════════════════════════════════════════════════════════════▲
                                  3. 优先尝试 UDP 打洞 P2P 直连
                           (打洞失败时无感走 EC2 盲中继隧道穿透)
```

### 零知识保证 (Zero-Knowledge Invariant)
* EC2 云端服务器**仅充当信令配对协调器与密文流转发器**；
* 所有流量在离开 A/B 节点前均已完成 Ed25519 协商与端到端加密；
* EC2 内存与磁盘**无任何 Token、无明文 Prompt、无代码泄露**。

---

## 二、AWS EC2 选型与网络配置

### 2.1 推荐实例规格
* **实例类型**：`t4g.small` (AWS Graviton3, 推荐) 或 `t3.micro` / `t3.small`
  * 纯 Rust 异步 IO 架构（Tokio + Axum），内存占用仅约 **15MB~25MB**；
  * 月成本极低（甚至在 AWS 免费层或每月仅需数美元）。
* **操作系统**：`Ubuntu 22.04 LTS` 或 `Ubuntu 24.04 LTS`。
* **网络与 IP**：分配一个 **Elastic IP (弹性公网 IP)** 并绑定到该 EC2 实例。

### 2.2 AWS 安全组 (Security Group) 入站规则

在 AWS EC2 控制台的 **Security Groups -> Edit inbound rules**，开放以下端口：

| 协议 | 端口范围 | 来源 (Source) | 说明 |
| :--- | :--- | :--- | :--- |
| **TCP** | `22` | 您的管理 IP (或 0.0.0.0/0) | SSH 远程登录 |
| **TCP** | `80` | `0.0.0.0/0` | HTTP (Caddy 自动申请 Let's Encrypt 证书验证) |
| **TCP** | `443` | `0.0.0.0/0` | HTTPS (nexus.roviix.com API 与配对接口) |
| **TCP** | `8791` | `0.0.0.0/0` | 对等节点反向加密盲中继数据隧道 |
| **UDP** | `3478` | `0.0.0.0/0` | STUN 协议 NAT 打洞穿透探测 |

---

## 三、域名 DNS 解析配置

前往您的域名解析控制台（如 Cloudflare、Route 53 或阿里云/腾讯云 DNS）：

1. 添加一条 **A 记录**：
   * **主机记录 (Name)**：`nexus`（或者如果您用 `mesh.nexus`，则填 `mesh.nexus`）
   * **记录类型 (Type)**：`A`
   * **记录值 (Value)**：您的 AWS EC2 弹性公网 IP（例如 `18.163.x.x`）
   * **TTL**：`Auto` 或 `60 秒`
   * **代理状态 (Proxy Status)**：**仅 DNS (DNS Only)**（关闭 Cloudflare CDN 代理，以允许 TCP 隧道直通与 WebSocket 全双工流）。

---

## 四、一键极速部署（推荐方案）

### 4.1 登录 EC2 服务器
```bash
ssh -i ~/.ssh/your-key.pem ubuntu@<您的EC2公网IP>
```

### 4.2 执行自动化部署脚本
```bash
# 1. 下载或克隆仓库脚本
git clone https://github.com/roviix/nexus.git /opt/nexus-repo || true
cd /opt/nexus-repo

# 2. 执行一键部署 (传入您的域名，例如 nexus.roviix.com)
sudo bash deploy/setup-ec2.sh nexus.roviix.com
```

### 4.3 编译或放入独立服务端二进制
在 EC2 服务器本地编译（或者在本地交叉编译后上传）：
```bash
# 在 EC2 上编译 nexus-broker
cd /opt/nexus-repo
cargo build --release -p nexus-mesh --bin nexus-broker

# 复制到标准部署路径
sudo cp target/release/nexus-broker /opt/nexus/nexus-broker
sudo chmod +x /opt/nexus/nexus-broker

# 启动并设置开机自启
sudo systemctl enable --now nexus-broker
```

---

## 五、验证部署状态

在您的本地电脑终端运行健康检查：

```bash
curl -i https://nexus.roviix.com/v1/mesh/health
```

正常返回输出：
```json
HTTP/2 200
content-type: application/json

{
  "service": "nexus-broker",
  "status": "ok",
  "version": "0.9.0",
  "timestamp": 1773289000,
  "zero_knowledge": true
}
```

测试配对码注册接口：
```bash
curl -X POST https://nexus.roviix.com/v1/mesh/pair/register \
  -H "Content-Type: application/json" \
  -d '{"code":"NX-8839","node_id":"node_mac_01","device_name":"Mac Studio","channels":["claude","ollama"]}'
```

---

## 六、桌面客户端接入与全局联动

当 EC2 服务端启动后，所有 Nexus 客户端自动以此域名作为协调核心：

1. **客户端默认中继地址**：
   在 `crates/nexus-mesh/src/types.rs` 的默认配置中：
   ```rust
   broker_url: "https://nexus.roviix.com".to_string(),
   ```
2. **两台设备互通流程**：
   * 台式机点击「生成连接码」-> 本地生成 `NX-8839` 并同步上报给 `https://nexus.roviix.com/v1/mesh/pair/register`；
   * 笔记本在任何网络输入 `NX-8839` -> 客户端向 `https://nexus.roviix.com/v1/mesh/pair/resolve` 查询对端信息；
   * 双方首先尝试 UDP 直连打洞；若遇严格对称型 NAT，自动无缝降级通过 `tcp://nexus.roviix.com:8791` 进行端到端密文盲转发！
