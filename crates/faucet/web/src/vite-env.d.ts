/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly DEV: boolean
  readonly PROD: boolean
  /** JSON-RPC gateway serving `faucet_requestAirdrop`; defaults to public Devnet. */
  readonly VITE_THRUPUT_RPC_URL?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
