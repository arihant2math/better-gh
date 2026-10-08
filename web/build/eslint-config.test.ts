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

  // Convention rules (#203), checked against real file paths so the per-file
  // overrides apply. Type-aware rules are skipped: only these are under test.
  const lint = async (filePath: string, code: string) => {
    const eslint = new ESLint({
      cwd: process.cwd(),
      overrideConfig: { languageOptions: { parserOptions: { project: null } } },
      ruleFilter: ({ ruleId }) => ['no-restricted-imports', 'no-restricted-globals', 'no-restricted-syntax', 'bgh/observer-reads-store'].includes(ruleId),
    });
    const [result] = await eslint.lintText(code, { filePath });
    return result.messages.map((m) => m.ruleId);
  };

  it('keeps pages and REST wrappers out of routes.ts', async () => {
    expect(await lint('src/app/routes.ts', "import { v3 } from '../api/client';\nexport const x = v3;\n")).toEqual(['no-restricted-imports']);
    expect(await lint('src/app/routes.ts', "import Page from '../pages/repo/RepoLayout';\nexport const x = Page;\n")).toEqual(['no-restricted-imports']);
    expect(await lint('src/app/routes.ts', "import type { Issue } from '../sync/models';\nexport const p = () => import('../pages/repo/RepoLayout');\nexport type I = Issue;\n")).toEqual([]);
  });

  it('keeps heavy libraries in their lazy owners', async () => {
    expect(await lint('src/ui/Button.tsx', "import { marked } from 'marked';\nexport const x = marked;\n")).toEqual(['no-restricted-imports']);
    expect(await lint('src/ui/Button.tsx', "import { useVirtualizer } from '@tanstack/react-virtual';\nexport const x = useVirtualizer;\n")).toEqual(['no-restricted-imports']);
    expect(await lint('src/ui/markdown/render.ts', "import { marked } from 'marked';\nexport const x = marked;\n")).toEqual([]);
    expect(await lint('src/ui/markdown/render.ts', "import mermaid from 'mermaid';\nexport const x = mermaid;\n")).toEqual(['no-restricted-imports']);
    expect(await lint('src/ui/markdown/enhance.ts', "export const m = () => import('mermaid');\n")).toEqual([]);
    expect(await lint('src/pages/code/CodeLines.tsx', "import { useVirtualizer } from '@tanstack/react-virtual';\nexport const x = useVirtualizer;\n")).toEqual([]);
  });

  it('allows raw fetch only in the transport seam, boot and the service worker', async () => {
    const code = "export const go = () => fetch('/x');\n";
    expect(await lint('src/sync/client.ts', code)).toEqual(['no-restricted-globals']);
    expect(await lint('src/pages/actions/log/sse.ts', code)).toEqual(['no-restricted-globals']);
    for (const ok of ['src/api/transport.ts', 'src/main.tsx', 'src/sw.ts']) expect(await lint(ok, code)).toEqual([]);
  });

  it('sends imports that climb 3+ folders through the @/ alias', async () => {
    const deep = "import { api } from '../../../api/client';\nexport const x = api;\n";
    expect(await lint('src/pages/repo/insights/Chart.tsx', deep)).toEqual(['no-restricted-syntax']);
    expect(await lint('src/pages/repo/insights/Chart.test.ts', deep)).toEqual(['no-restricted-syntax']);
    expect(await lint('src/pages/repo/insights/Chart.tsx', "export const p = () => import('../../../api/client');\n")).toEqual(['no-restricted-syntax']);
    expect(await lint('src/pages/repo/insights/Chart.tsx', "export { api } from '../../../api/client';\n")).toEqual(['no-restricted-syntax']);
    expect(await lint('src/pages/repo/insights/Chart.tsx', "import { api } from '@/api/client';\nimport { x } from '../../y';\nexport const z = [api, x];\n")).toEqual([]);
  });

  it('requires observer for components that read the store', async () => {
    const code = "import { store } from '../sync';\nexport function Who() { return <p>{store().all('user').length}</p>; }\n";
    expect(await lint('src/ui/Who.tsx', code)).toEqual(['bgh/observer-reads-store']);
  });
});
