#!/usr/bin/env node

import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const desktopDirectory = path.resolve(scriptDirectory, "..");
const expectedVersion = String(process.argv[2] || "").replace(/^v/, "");

if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(expectedVersion)) {
  console.error("用法：node scripts/check-release-version.mjs <semver>");
  process.exit(2);
}

const tauriConfig = JSON.parse(
  await fs.readFile(path.join(desktopDirectory, "apps/desktop/src-tauri/tauri.conf.json"), "utf8"),
);
const packageJson = JSON.parse(
  await fs.readFile(path.join(desktopDirectory, "apps/desktop/package.json"), "utf8"),
);
const cargoToml = await fs.readFile(path.join(desktopDirectory, "Cargo.toml"), "utf8");
const cargoVersion = /^\s*version\s*=\s*"([^"]+)"/m.exec(
  cargoToml.split("[workspace.package]")[1] || "",
)?.[1];

const versions = new Map([
  ["Cargo.toml", cargoVersion],
  ["apps/desktop/package.json", packageJson.version],
  ["apps/desktop/src-tauri/tauri.conf.json", tauriConfig.version],
]);

let valid = true;
for (const [source, version] of versions) {
  if (version !== expectedVersion) {
    valid = false;
    console.error(`${source}: ${version || "<missing>"}（期望 ${expectedVersion}）`);
  }
}
if (!valid) process.exit(1);

console.log(`版本一致：${expectedVersion}`);
