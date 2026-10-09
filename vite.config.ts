import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  server: {
    host: '127.0.0.1',
    port: 1420,
    strictPort: true,
  },
  build: {
    target: 'es2022',
    sourcemap: false,
  },
  test: {
    include: ['src/**/*.test.ts'],
    environment: 'node',
  },
});
