/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly DEV: boolean
  readonly PROD: boolean
  readonly VITE_GROUNDHOG_FAUCET_API_BASE?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
