# Groundhog Faucet Web

Static React frontend for the Groundhog faucet service.

## Local Development

Run the Rust faucet API on `127.0.0.1:8081`, then start Vite:

```sh
pnpm install
pnpm dev
```

The Vite dev server proxies `/v1` and `/health` to the local faucet API.

## Production Build

Build with the deployed API base URL:

```sh
VITE_GROUNDHOG_FAUCET_API_BASE=https://faucet-api.example.com pnpm build
```

Deploy `dist/` to S3, CloudFront, Netlify, or another static host. Configure the
Rust faucet service with the static site's origin in `cors_allowed_origins`.
