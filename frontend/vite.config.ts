import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  server: { port: 5198, strictPort: true, proxy: { '/api': 'http://localhost:3120' } },
  build: { target: 'es2022', sourcemap: false, outDir: 'dist' },
})
