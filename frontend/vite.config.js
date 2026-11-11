import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// PAGES_BASE is set by the GitHub Pages workflow (project site subpath).
export default defineConfig({
  base: process.env.PAGES_BASE || '/',
  plugins: [react()],
  server: { host: '127.0.0.1', port: 5173, strictPort: true },
  preview: { host: '127.0.0.1', port: 4173 }
});
