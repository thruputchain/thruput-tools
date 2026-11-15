# Badge points

A points app for the Solana Breakpoint 2026 badge, backed by a contract on
Thruput: pick a badge near you, award it points, and see everyone's totals.

**The awards are not signed, and that is the point of the demo.** Solana OS
gives badge apps SHA-256 and signature *checks* but, on purpose, nothing to
make a key pair or a signature with. So the badge cannot sign a Thruput
transaction. Instead the contract sets its address's required signature weight
to zero, which lets anyone submit a transaction to it with no signature at
all, and it takes the sender's word for which badge is awarding. Anyone who
can reach the RPC endpoint can award points as anyone. The app says so when it
opens, before every award, and in a banner that never goes away.

Use it on a testnet, for fun. Do not copy the pattern for anything that
matters.

## What is here

| Path | What |
| --- | --- |
| `points-contract/` | The contract (Rust, Thruput SDK): `open`, `award`, `points`, `given` |
| `app/` | The badge app (Lua): `main.lua`, `app.json`, `config.lua`, `lib/thruput.lua` |
| `test/` | A desktop harness that runs the app on the badge firmware's own Lua |

`app/config.lua` points at a deployment on Futurenet,
`G9E66ASucCcXibK2dQRM67DYgKtJoDGrpUybe5JLJ7Gt`, so the app works as it is.

## How it works

- **Identity.** A badge is its id (`badge.id()`, like `7K2Q9M`). The contract
  takes it as a `u64`: the id's ASCII characters, big-endian.
- **Who to award.** The list is `mesh.nearby()`, strongest signal first, plus
  ids typed in by hand (the last six are remembered).
- **Reading.** `simulateInvocations` on `points` and `given`, ten reads per
  request so it stays under the badge's 2 KB request limit.
- **Awarding.** The app builds the transaction envelope itself
  (`lib/thruput.lua`): classic XDR for the envelope, the compact CXS encoding
  for the calldata, an empty signature list. It reads the chain tip for the
  expiry height, submits with `sendTransaction`, polls
  `getTransactionReceipt`, then re-reads the points at that block.
- **One radio.** Where Wi-Fi is joined on the mesh channel both work at once.
  Elsewhere the app pauses the mesh, waits for Wi-Fi, does its requests and
  resumes; the nearby list is kept meanwhile.
- **32-bit Lua.** The badge's integers are 32 bits, so every 64-bit value
  (heights, nonces, badge ids) is handled as a string of bytes.

The badge server makes the app's HTTPS requests for it, and only to the hosts
in `app.json` (`rpc.thruput.network`). The badge's built-in `rpc` module is
Solana-only and is not used.

## Deploy your own contract

```sh
cd points-contract
thruput build
thruput test
thruput airdrop default --profile futurenet --yes
thruput deploy --fund 10 --init open --profile futurenet --yes
```

`--init open` is what makes the contract callable without a signature; a
deployment without it rejects every award from the badge. Put the printed
address in `app/config.lua`.

The contract pays for its own storage out of the funds it was deployed with:
about 0.0024 native tokens the first time a pair of badges appears (one new
storage item each for the receiver's points and the giver's count), almost
nothing after that. Ten tokens cover roughly 4,000 first-time awards. Top it
up with `thruput airdrop <address> --profile futurenet`.

When this was written Futurenet ran release 0.1.31, which refused the v6
artifact that CLI 0.45 builds (`contractRejected: unsupportedFormat`). The
deployment above was built and deployed with the published `thruput` 0.43.0.
If your deploy is dropped the same way, do the same:

```sh
cargo install thruput --version 0.43.0 --locked --root /tmp/thruput-0.43
```

## Put the app on a badge

The app needs Solana OS 2.1.0 or later (for `crypto.sha256` and base58).

1. Open the badge site's submit page (`/apps/submit`) and drop the `app/`
   folder on it. `app.json` fills in the form; change `author` to yours.
2. **Try in emulator** or **Run on badge** (USB). `http` in a test run needs
   an account with X linked.
3. **Publish** to put it in the store. The store shows the permissions
   (`mesh`, `http`) and the host next to the app.

Keys: UP/DOWN pick a badge, LEFT/RIGHT set the amount (1 to 10), SELECT
awards, CANCEL exits.

## Test

```sh
test/run.sh
```

builds the harness (it clones the badge firmware for its Lua sources; set
`LUA_VENDOR` to an existing `solana-os/main/lua/vendor` to skip that) and
checks the encoders against fixed vectors. To also drive the whole app
against a node:

```sh
POINTS_CONTRACT=<address> POINTS_RPC=http://127.0.0.1:9545 test/run.sh
```

Leave `POINTS_RPC` out to use the endpoint in `app/config.lua`. The run sends
a handful of real awards between random badge ids.

The harness mocks the badge's modules from their documentation
(`test/mock.lua`), with the documented limits enforced: 200,000 instructions
per callback, 2 KB per request, 3.5 KB per answer, the manifest's hosts. It is
not the firmware. The app has not been run on a real badge or in the badge
emulator.
