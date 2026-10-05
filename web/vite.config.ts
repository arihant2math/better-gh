/// <reference types="vitest/config" />
import react from '@vitejs/plugin-react';
import { defineConfig, type ProxyOptions } from 'vite';
import { compression } from 'vite-plugin-compression2';
import { bghFontPreload, bghServiceWorker } from './build/plugins.ts';

const BACKEND = process.env.BGH_BACKEND ?? 'http://localhost:3000';

const proxy: Record<string, ProxyOptions> = {
  '/api': { target: BACKEND, changeOrigin: false },
  '/_bgh': { target: BACKEND, changeOrigin: false, ws: true },
  '/avatars': { target: BACKEND, changeOrigin: false },
  // git smart HTTP + raw/archive downloads: /{owner}/{repo}.git/..., /{owner}/{repo}/info/refs, ...
  '^/[^/]+/[^/]+\\.git(/.*)?$': { target: BACKEND },
  '^/[^/]+/[^/]+/(info/refs|git-upload-pack|git-receive-pack)$': { target: BACKEND },
  '^/[^/]+/[^/]+/(raw|archive)/.*': { target: BACKEND },
};

/** Modules that are needed on every page go into one long-cached vendor chunk. */
const VENDOR = /[\\/]node_modules[\\/](react|react-dom|scheduler|mobx|mobx-react-lite|idb)[\\/]/;

export default defineConfig({
  plugins: [
    react(),
    bghFontPreload(),
    bghServiceWorker(),
    compression({
      algorithms: ['gzip', 'brotliCompress'],
      include: /\.(js|css|html|svg|json|txt|map|woff2?)$/,
      exclude: /\.woff2$/,
      threshold: 512,
    }),
  ],
  build: {
    target: 'es2022',
    cssCodeSplit: true,
    manifest: true,
    sourcemap: false,
    modulePreload: { polyfill: false },
    assetsInlineLimit: 2048,
    reportCompressedSize: false,
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [{ name: 'vendor', test: VENDOR, priority: 10 }],
        },
      },
    },
  },
  server: { port: 5173, proxy },
  preview: { port: 4173, proxy },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts', 'src/**/*.test.tsx'],
    setupFiles: ['src/test/setup.ts'],
  },
});
