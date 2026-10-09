import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

// Tauri serves the built files; `npm run dev` is for working on the UI in a browser
// with the mock backend (src/lib/mock.ts).
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    target: 'es2022',
    outDir: 'dist',
    emptyOutDir: true,
    // The main window and the overlay window (a separate, click-through window).
    rollupOptions: { input: { main: 'index.html', overlay: 'overlay.html' } },
  },
  test: { environment: 'node', include: ['src/**/*.test.ts'] },
});
