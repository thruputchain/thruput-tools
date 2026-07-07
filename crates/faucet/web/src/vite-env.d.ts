/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_GROUNDHOG_FAUCET_API_BASE?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
