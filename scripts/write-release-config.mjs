#!/usr/bin/env node

import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const outputPath = process.argv[2] ? path.resolve(process.argv[2]) : "";
const updaterPublicKey = String(process.env.TAURI_UPDATER_PUBLIC_KEY || "").trim();
// 默认从 GitHub Releases 的「最新版」取更新清单；fork 或自建分发时用环境变量覆盖。
const updaterEndpoint = String(
  process.env.TAURI_UPDATER_ENDPOINT ||
    "https://github.com/roviix/nexus/releases/latest/download/latest.json",
).trim();
const windowsCertificateThumbprint = String(
  process.env.WINDOWS_CERTIFICATE_THUMBPRINT || "",
).replace(/\s/g, "");

if (!outputPath) {
  console.error("用法：node scripts/write-release-config.mjs <output.json>");
  process.exit(2);
}
if (!updaterPublicKey) {
  console.error("缺少 TAURI_UPDATER_PUBLIC_KEY；公开版本不能生成无法验证来源的更新包。");
  process.exit(1);
}
if (!updaterEndpoint.startsWith("https://")) {
  console.error("TAURI_UPDATER_ENDPOINT 必须使用 HTTPS。");
  process.exit(1);
}

const releaseConfig = {
  bundle: {
    createUpdaterArtifacts: true,
    ...(windowsCertificateThumbprint
      ? {
          windows: {
            certificateThumbprint: windowsCertificateThumbprint,
            digestAlgorithm: "sha256",
            timestampUrl: "http://timestamp.digicert.com",
          },
        }
      : {}),
  },
  plugins: {
    updater: {
      endpoints: [updaterEndpoint],
      pubkey: updaterPublicKey,
    },
  },
};

await fs.mkdir(path.dirname(outputPath), { recursive: true });
await fs.writeFile(outputPath, `${JSON.stringify(releaseConfig, null, 2)}\n`, {
  encoding: "utf8",
  mode: 0o600,
});
console.log(`release config: ${outputPath}`);
