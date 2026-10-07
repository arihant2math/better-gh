// Guards the type-aware lint baseline from #256: these rules must stay on
// (as errors) for app code, with type information available to them.
import { ESLint } from 'eslint';
import { describe, expect, it } from 'vitest';

const severity = (v: unknown) => (Array.isArray(v) ? v[0] : v);

describe('eslint.config.js', () => {
  it('enables the type-aware and zero-hit rules as errors for src/', async () => {
    const config = (await new ESLint({ cwd: process.cwd() }).calculateConfigForFile('src/main.tsx')) as {
      rules: Record<string, unknown>;
      languageOptions: { parserOptions?: { project?: unknown } };
    };
    expect(config.languageOptions.parserOptions?.project).toBeTruthy();
    for (const rule of [
      '@typescript-eslint/no-floating-promises',
      '@typescript-eslint/no-misused-promises',
      '@typescript-eslint/no-unnecessary-type-assertion',
      'eqeqeq',
      'no-console',
    ]) {
      expect([rule, severity(config.rules[rule])]).toEqual([rule, 2]);
    }
    // React Compiler-era hooks rules are on, as warnings until burned down.
    expect(severity(config.rules['react-hooks/refs'])).toBe(1);
    expect(severity(config.rules['react-hooks/rules-of-hooks'])).toBe(2);
  });
});
