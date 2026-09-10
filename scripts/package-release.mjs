#!/usr/bin/env node

import crypto from "node:crypto";
import { createReadStream } from "node:fs";
import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

function parseArguments(argv) {
  const values = new Map();
  const flags = new Set();
  for (let index = 0; index < argv.length; index += 1) {
    const item = argv[index];
    if (!item.startsWith("--")) throw new Error(`无法识别的参数：${item}`);
    if (item === "--verified-signed" || item === "--preview") {
      flags.add(item);
      continue;
    }
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`${item} 缺少值`);
    values.set(item, value);
    index += 1;
  }
  return { values, flags };
}

async function listFiles(directory) {
  const output = [];
  for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
    const absolutePath = path.join(directory, entry.name);
    if (entry.isDirectory()) output.push(...(await listFiles(absolutePath)));
    else if (entry.isFile()) output.push(absolutePath);
  }
  return output;
}

function requireUnique(files, predicate, label) {
  const matches = files.filter(predicate);
  if (matches.length !== 1) {
    const names = matches.map((file) => path.basename(file)).join(", ") || "无";
    throw new Error(`${label} 应恰好有一个，实际 ${matches.length} 个：${names}`);
  }
  return matches[0];
}

async function digest(filename, algorithm) {
  const hash = crypto.createHash(algorithm);
  for await (const chunk of createReadStream(filename)) hash.update(chunk);
  return hash.digest("hex");
}

/**
 * 一个发行物的公开地址。`baseUrl` 是这一版 Release 的资产前缀
 * （`https://github.com/<owner>/<repo>/releases/download/<tag>`）；没给就写相对文件名，
 * 清单不绑定任何主机。
 */
function releaseFileUrl(baseUrl, filename) {
  return baseUrl ? `${baseUrl}/${encodeURIComponent(filename)}` : filename;
}

const { values, flags } = parseArguments(process.argv.slice(2));
const inputDirectory = path.resolve(values.get("--input") || "");
const outputDirectory = path.resolve(values.get("--output") || "");
const version = String(values.get("--version") || "").replace(/^v/, "");
// `--base-url` 是这一版 Release 的资产前缀。updater 的 `latest.json` 必须带绝对地址，
// 所以正式发布（--verified-signed）必填；预览包可以不给，清单里就只写文件名。
const baseUrl = String(values.get("--base-url") || "").replace(/\/+$/, "");
const notesFile = values.get("--notes-file");
const verifiedSigned = flags.has("--verified-signed");
const preview = flags.has("--preview");
const publishedAt = String(process.env.RELEASE_PUBLISHED_AT || new Date().toISOString());

if (!values.get("--input") || !values.get("--output")) {
  console.error(
    "用法：node scripts/package-release.mjs --input <dir> --output <dir> --version <semver> [--notes-file <file>] [--base-url <https://github.com/<owner>/<repo>/releases/download/<tag>>] (--preview | --verified-signed)",
  );
  process.exit(2);
}
if (preview === verifiedSigned) {
  throw new Error("必须且只能指定一种发布模式：--preview 或 --verified-signed。");
}
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error(`版本不是有效 SemVer：${version || "<empty>"}`);
}
if (verifiedSigned && !baseUrl) {
  throw new Error("正式发布必须给 --base-url：updater 清单里的下载地址得是绝对地址。");
}
if (baseUrl && !baseUrl.startsWith("https://")) {
  throw new Error("公开下载地址必须使用 HTTPS。");
}
if (!Number.isFinite(Date.parse(publishedAt))) {
  throw new Error(`RELEASE_PUBLISHED_AT 不是有效 RFC 3339 时间：${publishedAt}`);
}
if (inputDirectory === outputDirectory || outputDirectory === path.parse(outputDirectory).root) {
  throw new Error("输出目录不能等于输入目录或文件系统根目录。");
}

const inputFiles = await listFiles(inputDirectory);
const macInstaller = requireUnique(
  inputFiles,
  (file) => file.toLowerCase().endsWith(".dmg"),
  "macOS DMG",
);
const windowsInstaller = requireUnique(
  inputFiles,
  (file) => file.toLowerCase().endsWith(".exe"),
  "Windows NSIS EXE",
);
// updater 产物：正式发布必须齐全；预览包有就带上（未签名的构建照样能走应用内更新，
// 更新器验的是 Tauri 自己那对密钥的签名，不是操作系统的代码签名），没有就只出安装包。
const hasUpdaterArtifacts = inputFiles.some((file) => file.toLowerCase().endsWith(".app.tar.gz"));
const withUpdater = !preview || hasUpdaterArtifacts;
if (withUpdater && !baseUrl) {
  throw new Error("带 updater 产物的包必须给 --base-url，否则 latest.json 里是相对地址。");
}
const macUpdater = withUpdater
  ? requireUnique(
      inputFiles,
      (file) => file.toLowerCase().endsWith(".app.tar.gz"),
      "macOS updater bundle",
    )
  : null;
const macUpdaterSignature = macUpdater
  ? requireUnique(
      inputFiles,
      (file) => file === `${macUpdater}.sig`,
      "macOS updater signature",
    )
  : null;
const windowsUpdaterSignature = withUpdater
  ? requireUnique(
      inputFiles,
      (file) => file === `${windowsInstaller}.sig`,
      "Windows updater signature",
    )
  : null;

await fs.rm(outputDirectory, { recursive: true, force: true });
await fs.mkdir(outputDirectory, { recursive: true });

const installerFiles = [
  {
    source: macInstaller,
    filename: `Nexus_${version}_macOS_universal.dmg`,
  },
  {
    source: windowsInstaller,
    filename: `Nexus_${version}_Windows_x64-setup.exe`,
  },
];
const updaterFiles =
  macUpdater && macUpdaterSignature && windowsUpdaterSignature
    ? [
        {
          source: macUpdater,
          filename: `Nexus_${version}_macOS_universal.app.tar.gz`,
        },
        {
          source: macUpdaterSignature,
          filename: `Nexus_${version}_macOS_universal.app.tar.gz.sig`,
        },
        {
          source: windowsUpdaterSignature,
          filename: `Nexus_${version}_Windows_x64-setup.exe.sig`,
        },
      ]
    : [];
const releaseFiles = [...installerFiles, ...updaterFiles];

for (const releaseFile of releaseFiles) {
  await fs.copyFile(releaseFile.source, path.join(outputDirectory, releaseFile.filename));
}

const fileMetadata = new Map();
for (const releaseFile of releaseFiles) {
  const outputPath = path.join(outputDirectory, releaseFile.filename);
  const info = await fs.stat(outputPath);
  fileMetadata.set(releaseFile.filename, {
    size: info.size,
    sha256: await digest(outputPath, "sha256"),
    md5: await digest(outputPath, "md5"),
  });
}

const macInstallerName = releaseFiles[0].filename;
const windowsInstallerName = releaseFiles[1].filename;
const macUpdaterName = updaterFiles[0]?.filename;
const macUpdaterSignatureName = updaterFiles[1]?.filename;
const windowsUpdaterSignatureName = updaterFiles[2]?.filename;
const macMetadata = fileMetadata.get(macInstallerName);
const windowsMetadata = fileMetadata.get(windowsInstallerName);
const notes = notesFile ? (await fs.readFile(path.resolve(notesFile), "utf8")).trim() : "";

const releaseManifest = {
  schemaVersion: 1,
  channel: preview ? "preview" : "stable",
  version,
  publishedAt,
  ...(notes ? { notes } : {}),
  artifacts: [
    {
      platform: "macos",
      architecture: "universal",
      filename: macInstallerName,
      url: releaseFileUrl(baseUrl, macInstallerName),
      size: macMetadata.size,
      sha256: macMetadata.sha256,
      md5: macMetadata.md5,
      codeSigned: verifiedSigned,
      notarized: verifiedSigned,
      minimumSystemVersion: "macOS 10.15+",
    },
    {
      platform: "windows",
      architecture: "x86_64",
      filename: windowsInstallerName,
      url: releaseFileUrl(baseUrl, windowsInstallerName),
      size: windowsMetadata.size,
      sha256: windowsMetadata.sha256,
      md5: windowsMetadata.md5,
      codeSigned: verifiedSigned,
      minimumSystemVersion: "Windows 10+ (64-bit)",
    },
  ],
  checksumFiles: {
    sha256: releaseFileUrl(baseUrl, "SHA256SUMS"),
    md5: releaseFileUrl(baseUrl, "MD5SUMS"),
  },
};

let updaterManifest = null;
if (macUpdaterName && macUpdaterSignatureName && windowsUpdaterSignatureName) {
  const macSignature = (
    await fs.readFile(path.join(outputDirectory, macUpdaterSignatureName), "utf8")
  ).trim();
  const windowsSignature = (
    await fs.readFile(path.join(outputDirectory, windowsUpdaterSignatureName), "utf8")
  ).trim();
  if (!macSignature || !windowsSignature) {
    throw new Error("Tauri updater 签名文件为空。");
  }

  const macUpdate = {
    signature: macSignature,
    url: releaseFileUrl(baseUrl, macUpdaterName),
  };
  updaterManifest = {
    version,
    notes,
    pub_date: publishedAt,
    platforms: {
      "darwin-aarch64": macUpdate,
      "darwin-x86_64": macUpdate,
      "windows-x86_64": {
        signature: windowsSignature,
        url: releaseFileUrl(baseUrl, windowsInstallerName),
      },
    },
  };
}

const sortedFileNames = [...fileMetadata.keys()].sort();
const sha256Sums = sortedFileNames
  .map((filename) => `${fileMetadata.get(filename).sha256}  ${filename}`)
  .join("\n");
const md5Sums = sortedFileNames
  .map((filename) => `${fileMetadata.get(filename).md5}  ${filename}`)
  .join("\n");

const outputWrites = [
  fs.writeFile(
    path.join(outputDirectory, "release.json"),
    `${JSON.stringify(releaseManifest, null, 2)}\n`,
  ),
  fs.writeFile(path.join(outputDirectory, "SHA256SUMS"), `${sha256Sums}\n`),
  fs.writeFile(path.join(outputDirectory, "MD5SUMS"), `${md5Sums}\n`),
];
if (updaterManifest) {
  outputWrites.push(
    fs.writeFile(
      path.join(outputDirectory, "latest.json"),
      `${JSON.stringify(updaterManifest, null, 2)}\n`,
    ),
  );
}
await Promise.all(outputWrites);

console.log(`release ${version}: ${outputDirectory}`);
for (const artifact of releaseManifest.artifacts) {
  console.log(`- ${artifact.platform}: ${artifact.filename} (${artifact.size} bytes)`);
}
