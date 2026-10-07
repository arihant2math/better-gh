import js from '@eslint/js';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';
import tseslint from 'typescript-eslint';
import bgh from './build/eslint-rules.mjs';

// Shared `no-restricted-imports` entries; a file-specific block must repeat
// them because flat config replaces a rule's options rather than merging.
const BANNED_PATHS = [
  { name: 'lodash', message: 'Heavy dependency: write the helper or check the bundle budget first.' },
  { name: 'moment', message: 'Use Intl / ui/RelativeTime.' },
  // Heavy libs load on demand (docs/FRONTEND.md "Performance rules"); a static
  // import anywhere would pull them into whichever chunk imports it.
  { name: 'mermaid', message: 'Load mermaid with a dynamic import() (see ui/markdown/enhance.ts).', allowTypeImports: true },
  { name: 'temml', message: 'Load temml with a dynamic import() (see ui/markdown/enhance.ts).', allowTypeImports: true },
];
const BANNED_PATTERNS = [
  { group: ['**/mock/*', '!**/mock/index'], message: 'Import the mock backend only via dynamic import of mock/index.' },
];
const MARKDOWN_ONLY = [
  { name: 'marked', message: 'Markdown rendering lives in ui/markdown/ (lazy chunk); use ui/Markdown.', allowTypeImports: true },
  { name: 'dompurify', message: 'Sanitising lives in ui/markdown/ (lazy chunk); use ui/Markdown.', allowTypeImports: true },
];
const VIRTUAL_ONLY = [
  {
    name: '@tanstack/react-virtual',
    message: 'Use ui/VirtualList (or import it from a lazy page chunk).',
    allowTypeImports: true,
  },
];
const restrict = (paths, patterns = []) => ['error', { paths: [...BANNED_PATHS, ...paths], patterns: [...BANNED_PATTERNS, ...patterns] }];

export default tseslint.config(
  { ignores: ['dist', 'node_modules', 'coverage'] },
  {
    files: ['**/*.{ts,tsx}'],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      ecmaVersion: 2022,
      globals: { ...globals.browser },
      // Type-aware rules: each file is checked against the tsconfig that owns it.
      parserOptions: {
        project: ['./tsconfig.json', './tsconfig.sw.json', './tsconfig.node.json'],
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: { 'react-hooks': reactHooks, bgh },
    rules: {
      // React Compiler-era rules (v7) warn until their hits are burned down (#256).
      ...Object.fromEntries(Object.entries(reactHooks.configs['recommended-latest'].rules).map(([k, v]) => [k, v === 'error' ? 'warn' : v])),
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'warn',
      '@typescript-eslint/no-floating-promises': 'error',
      '@typescript-eslint/no-misused-promises': 'error',
      '@typescript-eslint/no-unnecessary-type-assertion': 'error',
      eqeqeq: ['error', 'smart'],
      'no-console': ['error', { allow: ['warn', 'error'] }],
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_', varsIgnorePattern: '^_' }],
      '@typescript-eslint/consistent-type-imports': ['error', { fixStyle: 'inline-type-imports' }],
      'no-restricted-imports': restrict([...MARKDOWN_ONLY, ...VIRTUAL_ONLY]),
      // REST goes through api/client (CSRF, 401 handling, rate limits).
      'no-restricted-globals': ['error', { name: 'fetch', message: 'Use api/client (or the api/transport seam); raw fetch skips CSRF and 401 handling.' }],
      'bgh/observer-reads-store': 'error',
    },
  },
  // Owners of the restricted libraries.
  { files: ['src/ui/markdown/**'], rules: { 'no-restricted-imports': restrict(VIRTUAL_ONLY) } },
  { files: ['src/ui/VirtualList.tsx', 'src/pages/**'], rules: { 'no-restricted-imports': restrict(MARKDOWN_ONLY) } },
  {
    // routes.ts is in the initial bundle: pages and REST wrappers load lazily.
    files: ['src/app/routes.ts'],
    rules: {
      'no-restricted-imports': restrict(
        [...MARKDOWN_ONLY, ...VIRTUAL_ONLY],
        [
          {
            group: ['**/api', '**/api/*', '**/pages/**'],
            allowTypeImports: true,
            message: 'routes.ts is in the initial bundle: import() pages, and put prefetchers in app/routePrefetch.ts.',
          },
        ],
      ),
    },
  },
  {
    // Legitimate raw fetch: the transport seam, the pre-session boot refresh,
    // and the service worker (no api client in its scope).
    files: ['src/api/**', 'src/main.tsx', 'src/sw.ts'],
    rules: { 'no-restricted-globals': 'off' },
  },
  {
    files: ['src/mock/**', 'src/**/*.test.ts', 'src/**/*.test.tsx'],
    rules: { 'no-restricted-imports': 'off', 'no-restricted-globals': 'off' },
  },
  {
    files: ['vite.config.ts', 'build/**/*.ts', 'build/**/*.mjs', 'scripts/**/*.mjs'],
    languageOptions: { globals: { ...globals.node } },
  },
  {
    files: ['build/**/*.mjs'],
    extends: [js.configs.recommended],
  },
  {
    files: ['scripts/**/*.mjs'],
    extends: [js.configs.recommended],
    // screenshots.mjs evaluates callbacks in the page, so browser globals too.
    languageOptions: { globals: { ...globals.node, ...globals.browser } },
  },
);
