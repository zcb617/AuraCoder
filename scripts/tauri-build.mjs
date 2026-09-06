#!/usr/bin/env node
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const keyPath = resolve(projectRoot, '.tauri-signing', 'auracoder-updater.key');
const passwordPath = resolve(projectRoot, '.tauri-signing', 'auracoder-updater.key.password');

if (process.env.TAURI_SIGNING_PRIVATE_KEY) {
  console.log('[tauri-build] TAURI_SIGNING_PRIVATE_KEY 已注入，跳过本地 key 文件读取（CI 模式）');
} else {
  const missing = [];
  if (!existsSync(keyPath)) missing.push(keyPath);
  if (!existsSync(passwordPath)) missing.push(passwordPath);
  if (missing.length > 0) {
    console.error('[tauri-build] 缺少本地签名文件: ' + missing.join(', '));
    process.exit(1);
  }
  process.env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(keyPath, 'utf8');
  process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = readFileSync(passwordPath, 'utf8');
  console.log('[tauri-build] 已从 .tauri-signing 读取 updater 签名 key 与密码（本地模式）');
}

const result = spawnSync('node scripts/check-tauri-architecture-references.mjs && tauri build', {
  cwd: projectRoot,
  shell: true,
  stdio: 'inherit',
  env: process.env,
});

if (result.error) {
  console.error('[tauri-build] 执行失败: ' + result.error.message);
  process.exit(1);
}
process.exit(result.status ?? 1);
