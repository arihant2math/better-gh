import type { ESLint } from 'eslint';

declare const plugin: ESLint.Plugin & { rules: Record<string, import('eslint').Rule.RuleModule> };
export default plugin;
