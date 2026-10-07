import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { minify, transformWithOxc, type Plugin } from 'vite';

/**
 * Inject `<link rel="preload">` for the latin variable Inter font so the
 * first paint uses the right font without a FOUT/layout shift.
 */
export function bghFontPreload(match = /inter-latin-wght-normal-[\w-]+\.woff2$/): Plugin {
  return {
    name: 'bgh-font-preload',
    apply: 'build',
    transformIndexHtml: {
      order: 'post',
      handler(_html, ctx) {
        const file = Object.keys(ctx.bundle ?? {}).find((f) => match.test(f));
        if (!file) return [];
        return [
          {
            tag: 'link',
            attrs: { rel: 'preload', href: `/${file}`, as: 'font', type: 'font/woff2', crossorigin: '' },
            injectTo: 'head-prepend',
          },
        ];
      },
    },
  };
}

/**
 * Compile `src/sw.ts` and emit it as `/sw.js` with the precache manifest
 * (every hashed JS/CSS asset + the latin font) inlined.
 */
export function bghServiceWorker(): Plugin {
  const src = fileURLToPath(new URL('../src/sw.ts', import.meta.url));
  return {
    name: 'bgh-service-worker',
    apply: 'build',
    async generateBundle(_opts, bundle) {
      const precache = Object.keys(bundle)
        .filter((f) => /\.(js|css)$/.test(f) || /inter-latin-wght-normal.*\.woff2$/.test(f))
        .filter((f) => f.startsWith('assets/'))
        // The mock backend is dev/demo-only: fetched on demand, never precached.
        .filter((f) => !/\/mock-[\w-]+\.js$/.test(f))
        .sort()
        .map((f) => `/${f}`);
      const version = createHash('sha256').update(precache.join('\n')).digest('hex').slice(0, 12);
      const ts = await readFile(src, 'utf8');
      const js = await transformWithOxc(ts, 'sw.ts', { lang: 'ts' });
      const header = `self.__BGH_PRECACHE__=${JSON.stringify(precache)};self.__BGH_VERSION__=${JSON.stringify(version)};\n`;
      const out = await minify('sw.js', header + js.code.replace(/^export\s*\{\s*\};?\s*$/m, ''));
      this.emitFile({ type: 'asset', fileName: 'sw.js', source: out.code });
    },
  };
}

/**
 * Drop chunks the entry already loads (its static import graph: vendor,
 * runtime, shared UI) from every dynamic import's modulepreload list. They
 * are in memory before any lazy route runs, so listing them only bloats the
 * preload table in the entry chunk. `index.html` itself keeps its full
 * list: those `<link rel="modulepreload">` tags let the browser fetch vendor
 * and the other entry deps in parallel with the entry chunk instead of one
 * round trip later (#268). Returns the plugin (computes the entry graph in
 * `generateBundle`, before Vite rewrites the preload calls) and the
 * `build.modulePreload.resolveDependencies` hook that applies it. If the
 * graph is unknown the hook keeps every dependency.
 */
export function bghPreloadDedupe(): {
  plugin: Plugin;
  resolveDependencies: (file: string, deps: string[], ctx: { hostType: 'html' | 'js' }) => string[];
} {
  const initial = new Set<string>();
  const plugin: Plugin = {
    name: 'bgh-preload-dedupe',
    apply: 'build',
    generateBundle(_opts, bundle) {
      initial.clear();
      const visit = (file: string) => {
        const chunk = bundle[file];
        if (!chunk || chunk.type !== 'chunk' || initial.has(file)) return;
        initial.add(file);
        chunk.imports.forEach(visit);
      };
      for (const chunk of Object.values(bundle)) if (chunk.type === 'chunk' && chunk.isEntry && chunk.facadeModuleId?.endsWith('/index.html')) visit(chunk.fileName);
    },
  };
  return { plugin, resolveDependencies: (_file, deps, ctx) => (ctx.hostType === 'html' ? deps : deps.filter((d) => !initial.has(d))) };
}
