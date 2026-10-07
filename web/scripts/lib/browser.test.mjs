// Guards the single, pinned Playwright entry point (scripts/lib/browser.mjs).
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { chromium, request } from './browser.mjs';

const web = fileURLToPath(new URL('../../', import.meta.url));
const pkg = JSON.parse(readFileSync(`${web}package.json`, 'utf8'));

describe('scripts/lib/browser.mjs', () => {
  it('pins playwright-core to an exact version', () => {
    expect(pkg.devDependencies['playwright-core']).toMatch(/^\d+\.\d+\.\d+$/);
  });

  it('exports chromium and the request API', () => {
    expect(typeof chromium.launch).toBe('function');
    expect(typeof request.newContext).toBe('function');
  });

  it('is the only way scripts load Playwright', () => {
    const offenders = readdirSync(`${web}scripts`, { recursive: true })
      .filter((f) => f.endsWith('.mjs') && !f.startsWith('lib'))
      .filter((f) => /require\(\s*['"]playwright|lib\/node_modules\/playwright|from ['"]playwright/.test(readFileSync(`${web}scripts/${f}`, 'utf8')));
    expect(offenders).toEqual([]);
  });
});
