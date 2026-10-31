import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  base: '/app/',
  plugins: [svelte()],
  resolve: { conditions: ['browser'] },
  server: { proxy: { '/api': 'http://127.0.0.1:8090' } },
  build: { sourcemap: false, assetsInlineLimit: 0 },
  test: { environment: 'jsdom', include: ['src/**/*.test.ts'], restoreMocks: true },
});
