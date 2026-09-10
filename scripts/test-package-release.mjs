#!/usr/bin/env node

import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const temporaryRoot = await fs.mkdtemp(path.join(os.tmpdir(), "nexus-release-test-"));
const inputDirectory = path.join(temporaryRoot, "input");
const outputDirectory = path.join(temporaryRoot, "output");
const previewOutputDirectory = path.join(temporaryRoot, "preview-output");
const notesPath = path.join(temporaryRoot, "notes.md");

try {
  await fs.mkdir(path.join(inputDirectory, "macos"), { recursive: true });
  await fs.mkdir(path.join(inputDirectory, "windows"), { recursive: true });

  const fixtures = new Map([
    ["macos/Nexus_0.1.0_universal.dmg", "fake signed dmg"],
    ["macos/Nexus.app.tar.gz", "fake updater archive"],
    ["macos/Nexus.app.tar.gz.sig", "mac-updater-signature"],
    ["windows/Nexus_0.1.0_x64-setup.exe", "fake signed exe"],
    ["windows/Nexus_0.1.0_x64-setup.exe.sig", "windows-updater-signature"],
  ]);
  await Promise.all(
    [...fixtures].map(([filename, content]) =>
      fs.writeFile(path.join(inputDirectory, filename), content),
    ),
  );
  await fs.writeFile(notesPath, "Preview changes\n\n- First public build\n");

  const result = spawnSync(
    process.execPath,
    [
      path.join(scriptDirectory, "package-release.mjs"),
      "--input",
      inputDirectory,
      "--output",
      outputDirectory,
      "--version",
      "0.1.0",
      "--base-url",
      "https://github.com/example/nexus/releases/download/v0.1.0",
      "--verified-signed",
    ],
    { encoding: "utf8" },
  );
  assert.equal(result.status, 0, result.stderr || result.stdout);

  const release = JSON.parse(
    await fs.readFile(path.join(outputDirectory, "release.json"), "utf8"),
  );
  assert.equal(release.channel, "stable");
  assert.equal(release.version, "0.1.0");
  assert.equal(release.artifacts.length, 2);
  assert.ok(release.artifacts.every((artifact) => artifact.codeSigned));
  assert.equal(release.artifacts.find((artifact) => artifact.platform === "macos").notarized, true);
  // 给了 --base-url：所有地址都挂在这一版 Release 的资产前缀下。
  assert.equal(
    release.artifacts.find((artifact) => artifact.platform === "macos").url,
    "https://github.com/example/nexus/releases/download/v0.1.0/Nexus_0.1.0_macOS_universal.dmg",
  );
  assert.equal(
    release.checksumFiles.sha256,
    "https://github.com/example/nexus/releases/download/v0.1.0/SHA256SUMS",
  );

  const latest = JSON.parse(
    await fs.readFile(path.join(outputDirectory, "latest.json"), "utf8"),
  );
  assert.equal(
    latest.platforms["windows-x86_64"].url,
    "https://github.com/example/nexus/releases/download/v0.1.0/Nexus_0.1.0_Windows_x64-setup.exe",
  );
  assert.deepEqual(Object.keys(latest.platforms).sort(), [
    "darwin-aarch64",
    "darwin-x86_64",
    "windows-x86_64",
  ]);
  assert.equal(
    latest.platforms["darwin-aarch64"].signature,
    "mac-updater-signature",
  );
  assert.equal(
    latest.platforms["windows-x86_64"].signature,
    "windows-updater-signature",
  );

  const macArtifact = release.artifacts.find((artifact) => artifact.platform === "macos");
  const copiedMac = await fs.readFile(path.join(outputDirectory, macArtifact.filename));
  assert.equal(
    macArtifact.sha256,
    crypto.createHash("sha256").update(copiedMac).digest("hex"),
  );
  assert.match(
    await fs.readFile(path.join(outputDirectory, "SHA256SUMS"), "utf8"),
    new RegExp(`${macArtifact.sha256}  ${macArtifact.filename}`),
  );

  // 未签名但带 updater 产物的预览包：照样出 latest.json，只是 codeSigned 为 false。
  const previewResult = spawnSync(
    process.execPath,
    [
      path.join(scriptDirectory, "package-release.mjs"),
      "--input",
      inputDirectory,
      "--output",
      previewOutputDirectory,
      "--version",
      "0.1.0-preview.1",
      "--notes-file",
      notesPath,
      "--base-url",
      "https://github.com/example/nexus/releases/download/v0.1.0-preview.1",
      "--preview",
    ],
    { encoding: "utf8" },
  );
  assert.equal(previewResult.status, 0, previewResult.stderr || previewResult.stdout);

  const previewRelease = JSON.parse(
    await fs.readFile(path.join(previewOutputDirectory, "release.json"), "utf8"),
  );
  assert.equal(previewRelease.channel, "preview");
  assert.equal(previewRelease.version, "0.1.0-preview.1");
  assert.equal(previewRelease.notes, "Preview changes\n\n- First public build");
  assert.equal(previewRelease.artifacts.length, 2);
  assert.ok(previewRelease.artifacts.every((artifact) => !artifact.codeSigned));
  const previewLatest = JSON.parse(
    await fs.readFile(path.join(previewOutputDirectory, "latest.json"), "utf8"),
  );
  assert.equal(previewLatest.version, "0.1.0-preview.1");

  // 只有安装包、没有 updater 产物的预览：不给 --base-url 也行，清单只写文件名，没有 latest.json。
  const installersOnly = path.join(temporaryRoot, "installers-only");
  await fs.mkdir(installersOnly, { recursive: true });
  for (const name of ["Nexus_0.1.0_universal.dmg", "Nexus_0.1.0_x64-setup.exe"]) {
    await fs.writeFile(path.join(installersOnly, name), `fake ${name}`);
  }
  const bareOutput = path.join(temporaryRoot, "bare-output");
  const bareResult = spawnSync(
    process.execPath,
    [
      path.join(scriptDirectory, "package-release.mjs"),
      "--input",
      installersOnly,
      "--output",
      bareOutput,
      "--version",
      "0.1.0-preview.2",
      "--preview",
    ],
    { encoding: "utf8" },
  );
  assert.equal(bareResult.status, 0, bareResult.stderr || bareResult.stdout);
  const bareRelease = JSON.parse(await fs.readFile(path.join(bareOutput, "release.json"), "utf8"));
  assert.equal(
    bareRelease.artifacts.find((artifact) => artifact.platform === "macos").url,
    "Nexus_0.1.0-preview.2_macOS_universal.dmg",
  );
  assert.equal(bareRelease.checksumFiles.sha256, "SHA256SUMS");
  await assert.rejects(fs.access(path.join(bareOutput, "latest.json")));
  const bareSha256Sums = await fs.readFile(path.join(bareOutput, "SHA256SUMS"), "utf8");
  assert.equal(bareSha256Sums.trim().split("\n").length, 2);
  assert.doesNotMatch(bareSha256Sums, /\.sig/);

  // 正式发布没给 --base-url 必须拒绝：updater 读到相对地址会下载失败。
  const missingBase = spawnSync(
    process.execPath,
    [
      path.join(scriptDirectory, "package-release.mjs"),
      "--input",
      inputDirectory,
      "--output",
      path.join(temporaryRoot, "missing-base"),
      "--version",
      "0.1.0",
      "--verified-signed",
    ],
    { encoding: "utf8" },
  );
  assert.notEqual(missingBase.status, 0);
  assert.match(missingBase.stderr, /--base-url/);

  console.log("release packaging regression passed");
} finally {
  await fs.rm(temporaryRoot, { recursive: true, force: true });
}
