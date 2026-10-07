import type { OutputBundle } from 'rolldown';
import { describe, expect, it } from 'vitest';
import { bghPreloadDedupe } from './plugins';

const chunk = (fileName: string, imports: string[], entry = false) => ({
  type: 'chunk' as const,
  fileName,
  imports,
  isEntry: entry,
  facadeModuleId: entry ? '/web/index.html' : null,
});

describe('bghPreloadDedupe', () => {
  const { plugin, resolveDependencies } = bghPreloadDedupe();
  const bundle = {
    'assets/index.js': chunk('assets/index.js', ['assets/vendor.js', 'assets/client.js'], true),
    'assets/vendor.js': chunk('assets/vendor.js', []),
    'assets/client.js': chunk('assets/client.js', ['assets/vendor.js']),
    'assets/Page.js': chunk('assets/Page.js', ['assets/vendor.js', 'assets/Table.js']),
    'assets/Table.js': chunk('assets/Table.js', []),
  } as unknown as OutputBundle;
  (plugin.generateBundle as (o: unknown, b: OutputBundle) => void).call({}, {}, bundle);

  it('keeps every entry dep in index.html so they download alongside the entry (#268)', () => {
    const deps = ['assets/vendor.js', 'assets/client.js'];
    expect(resolveDependencies('index.html', deps, { hostType: 'html' })).toEqual(deps);
  });

  it('drops entry deps from dynamic-import preload lists', () => {
    expect(resolveDependencies('assets/Page.js', ['assets/vendor.js', 'assets/Table.js'], { hostType: 'js' })).toEqual(['assets/Table.js']);
  });
});
