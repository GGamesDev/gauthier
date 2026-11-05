import { defineConfig } from 'vite';

// Tauri expects a fixed port and no screen clearing.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: 'es2022', outDir: 'dist', emptyOutDir: true, chunkSizeWarningLimit: 1500 },
});
