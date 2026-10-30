# Thruput Faucet Web

Static React frontend for the Thruput Devnet faucet. It requests airdrops with
`@thruputchain/client`, through the JSON-RPC gateway's `faucet_requestAirdrop`
method; the gateway forwards them to the Rust faucet service, which signs and
submits.

## Local Development

All dependencies, including `@thruputchain/*`, are public on npmjs. No registry
token is needed:

```sh
pnpm install
pnpm dev
```

IBM Plex Sans and Mono font notices are in `public/font-licenses/` and are
copied into production builds alongside the font assets.

The page uses the public Devnet gateway by default. Set `VITE_THRUPUT_RPC_URL`
to use another gateway, for example a local `thruput node`:

```sh
VITE_THRUPUT_RPC_URL=http://127.0.0.1:9545 pnpm dev
```

## Checks

CI runs these from this directory:

```sh
pnpm format:check
pnpm build
```

## Railway Deployment

Create a service from the `thruputchain/thruput-tools` GitHub repository with
these settings:

| Setting        | Value                                     |
| -------------- | ----------------------------------------- |
| Root Directory | `/crates/faucet/web`                      |
| Config File    | `/crates/faucet/web/railway.json`         |
| Branch         | `main` after the deployment changes merge |

Generate a public domain under **Settings → Networking** after deployment.
Railway supplies `PORT`; use that port for the generated domain.

The checked-in configuration selects Railpack, builds with pnpm, and checks
`/healthz`. Leave the start command unset: Railpack detects Vite and serves
`dist/` with the checked-in `Caddyfile`.

Use `pnpm install --frozen-lockfile` to install the public dependencies.
The page talks to the public
Devnet gateway directly, which allows cross-origin requests; no signing key,
database or volume is needed. The hosted faucet continues to sign and submit
airdrops.

After deployment, check `/healthz` and an airdrop in the browser. The health
check confirms that the web server is running; it does not check the gateway or
the faucet.

## Production Build

```sh
pnpm install --frozen-lockfile
VITE_THRUPUT_RPC_URL=https://rpc.example.com pnpm build
```

Leave `VITE_THRUPUT_RPC_URL` unset to use the public Devnet gateway. Deploy
`dist/` to any static host.
