#!/usr/bin/env node
/**
 * Пакування десктопного застосову (Task 31):
 * 1. `cargo build --release -p fond-engine`;
 * 2. копіює бінарник у `apps/desktop/src-tauri/binaries/` (externalBin Tauri);
 * 3. `pnpm --filter desktop tauri build` → інсталятор NSIS.
 * Будь-яка помилка завершує скрипт ненульовим кодом.
 */
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const shell = process.platform === 'win32';
const pnpm = shell ? 'pnpm.cmd' : 'pnpm';

function run(command, args) {
  console.log(`> ${command} ${args.join(' ')}`);
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: 'inherit',
    shell,
  });
  if (result.error) {
    console.error(`Не вдалося запустити ${command}: ${result.error.message}`);
    process.exit(1);
  }
  if (result.status !== 0) {
    console.error(`Команда завершилася з кодом ${result.status ?? 1}`);
    process.exit(result.status ?? 1);
  }
}

// 1. Release-білд рушія.
run('cargo', ['build', '--release', '-p', 'fond-engine']);

// 2. Бінарник поруч із бандлом з іменем цілі Triple.
const hostTriple = (() => {
  if (process.platform === 'win32') {
    return 'x86_64-pc-windows-msvc';
  }
  return process.arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin';
})();
const source = path.join(root, 'target', 'release', 'fond-engine.exe');
const target = path.join(
  root,
  'apps',
  'desktop',
  'src-tauri',
  'binaries',
  `fond-engine-${hostTriple}.exe`,
);
mkdirSync(path.dirname(target), { recursive: true });
copyFileSync(source, target);
console.log(`Скопійовано: ${source} → ${target}`);

// 3. Бандл Tauri (NSIS за tauri.conf.json - bundle.targets).
run(pnpm, ['--filter', 'desktop', 'tauri', 'build']);

console.log('Пакування завершено. Інсталятор: target/release/bundle/nsis/.');
