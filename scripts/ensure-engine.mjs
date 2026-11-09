#!/usr/bin/env node
/**
 * Гарантує наявність sidecar-бінарника `fond-engine-*-x86_64-pc-windows-msvc.exe`
 * у `apps/desktop/src-tauri/binaries/`: build-скрипт Tauri перевіряє шлях
 * під час будь-якої cargo-збірки fond-desktop (clippy, test, dev).
 * Джерело: release-бінарник або debug-бінарник; за їхньої відсутності —
 * порожня заготовка (для збірки вистачає існування файлу, виконавчий бінарник
 * зʼявиться з `scripts/package.mjs`).
 */
import { copyFileSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const binariesDir = path.join(root, 'apps', 'desktop', 'src-tauri', 'binaries');
const target = path.join(binariesDir, 'fond-engine-x86_64-pc-windows-msvc.exe');

if (!existsSync(target)) {
  mkdirSync(binariesDir, { recursive: true });
  const candidates = [
    path.join(root, 'target', 'release', 'fond-engine.exe'),
    path.join(root, 'target', 'debug', 'fond-engine.exe'),
  ];
  const available = candidates.find(existsSync);
  if (available) {
    copyFileSync(available, target);
    console.log(`Скопійовано рушій: ${available} → ${target}`);
  } else {
    writeFileSync(target, '');
    console.log(`Заготовка sidecar: ${target} (порожня).`);
  }
}
