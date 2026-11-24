import path from 'node:path'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'
import { viteSingleFile } from 'vite-plugin-singlefile'

// The panel is built into one HTML file that the server embeds in its binary.
// In development, API calls go to a Trailhound server on TRAILHOUND_URL.
export default defineConfig({
  base: './',
  plugins: [react(), tailwindcss(), viteSingleFile()],
  resolve: { alias: { '@': path.resolve(import.meta.dirname, './src') } },
  server: {
    proxy: {
      '/_admin/api': process.env.TRAILHOUND_URL ?? 'http://localhost:7700',
      '/indexes': process.env.TRAILHOUND_URL ?? 'http://localhost:7700',
    },
  },
})
