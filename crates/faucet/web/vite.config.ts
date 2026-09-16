import { resolve } from "node:path"

import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig, loadEnv } from "vite"

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "GROUNDHOG_")
  const faucetTarget =
    env.GROUNDHOG_FAUCET_PROXY_TARGET || "http://127.0.0.1:8081"

  return {
    plugins: [react(), tailwindcss()],
    server: {
      proxy: {
        "/health": { target: faucetTarget, changeOrigin: true },
        "/v1": { target: faucetTarget, changeOrigin: true },
      },
    },
    resolve: {
      alias: {
        "@": resolve(__dirname, "src"),
      },
    },
    build: {
      outDir: "dist",
      emptyOutDir: true,
    },
  }
})
