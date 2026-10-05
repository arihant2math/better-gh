import js from '@eslint/js';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['dist', 'node_modules', 'coverage'] },
  {
    files: ['**/*.{ts,tsx}'],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      ecmaVersion: 2022,
      globals: { ...globals.browser },
    },
    plugins: { 'react-hooks': reactHooks },
    rules: {
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'warn',
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_', varsIgnorePattern: '^_' }],
      '@typescript-eslint/consistent-type-imports': ['error', { fixStyle: 'inline-type-imports' }],
      'no-restricted-imports': [
        'error',
        {
          paths: [
            { name: 'lodash', message: 'Heavy dependency: write the helper or check the bundle budget first.' },
            { name: 'moment', message: 'Use Intl / ui/RelativeTime.' },
          ],
          patterns: [{ group: ['**/mock/*', '!**/mock/index'], message: 'Import the mock backend only via dynamic import of mock/index.' }],
        },
      ],
    },
  },
  {
    files: ['src/mock/**', 'src/**/*.test.ts', 'src/**/*.test.tsx'],
    rules: { 'no-restricted-imports': 'off' },
  },
  {
    files: ['vite.config.ts', 'build/**/*.ts', 'scripts/**/*.mjs'],
    languageOptions: { globals: { ...globals.node } },
  },
  {
    files: ['scripts/**/*.mjs'],
    extends: [js.configs.recommended],
    languageOptions: { globals: { ...globals.node } },
  },
);
