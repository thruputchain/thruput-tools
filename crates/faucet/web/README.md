# Thruput Faucet Web

Static React frontend for the Groundhog faucet service.

## Local Development

Run the Rust faucet API on `127.0.0.1:8081`, then start Vite:

```sh
pnpm install
pnpm dev
```

The Vite dev server proxies `/v1` and `/health` to `http://127.0.0.1:8081` by
default. Set `GROUNDHOG_FAUCET_PROXY_TARGET` to use another faucet API during
local development. For example, to use the hosted Devnet API:

```sh
GROUNDHOG_FAUCET_PROXY_TARGET=https://faucet.thruput.network pnpm dev
```

This override configures Vite's local proxy only; it does not configure the
built static app or Railway's Caddy proxy. For a production build, use
`VITE_GROUNDHOG_FAUCET_API_BASE` as described below.

## Railway Deployment

Create a service from the `groundhogchain/groundhog-rpc` GitHub repository with
these settings:

| Setting        | Value                                     |
| -------------- | ----------------------------------------- |
| Root Directory | `/crates/faucet/web`                      |
| Config File    | `/crates/faucet/web/railway.json`         |
| Branch         | `main` after the deployment changes merge |

For a preview before merge, select `feat/thruput-faucet-branding` instead.
Generate a public domain under **Settings → Networking** after deployment.
Railway supplies `PORT`; use that port for the generated domain.

The checked-in configuration selects Railpack, builds with pnpm, and checks
`/healthz`. Leave the start command unset: Railpack detects Vite and serves
`dist/` with the checked-in `Caddyfile`.

The build sets `VITE_GROUNDHOG_FAUCET_API_BASE=/api`. Caddy forwards `/api/*` to
`https://faucet.thruput.network`, so browser requests stay on the UI origin.
No signing key, database, volume, or private package credentials are needed.
The existing hosted faucet continues to sign and submit airdrops.

After deployment, check `/healthz`, `/api/v1/network`, and an airdrop in the
browser. The health check confirms that the web server is running; it does not
check the hosted faucet. Proxied requests can share the backend's client-IP
quota, depending on its trusted-proxy configuration.

## Production Build

Build with the deployed API base URL:

```sh
VITE_GROUNDHOG_FAUCET_API_BASE=https://faucet-api.example.com pnpm build
```

Deploy `dist/` to S3, CloudFront, Netlify, or another static host. Configure the
Rust faucet service with the static site's origin in `cors_allowed_origins`.
