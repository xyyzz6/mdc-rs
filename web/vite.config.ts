import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// 开发时前端 5173，/api 代理到 Rust 服务 9208
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': 'http://127.0.0.1:9208',
    },
  },
  build: {
    outDir: 'dist',
  },
});
