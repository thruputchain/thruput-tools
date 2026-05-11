# OpenRPC Error Documentation Design

## Context

The OpenRPC artifact previously documented method parameters and
results, but not JSON-RPC errors. The wire codes already live in
`crates/api-types/src/error.rs`; leaving them out of OpenRPC forces
SDK authors to discover retry and validation behavior from server
source or runtime failures.

## Decision

`api-types` owns the canonical error catalog:

- stable component name
- numeric JSON-RPC code
- short message
- consumer-facing description

`groundhog-open-rpc-spec` projects that catalog into
`components.errors` and attaches per-method `errors` arrays containing
`$ref`s to those shared error components.

This keeps error codes in the same ownership layer as the rest of the
JSON-RPC wire surface. The proc macro does not infer backend error
behavior from method signatures; the canonical spec builder owns the
current method-to-error mapping explicitly.

## Shape

Each reusable error component is an OpenRPC error object:

```json
{
  "code": -32050,
  "message": "Backend unavailable",
  "x-name": "BackendUnavailable",
  "x-description": "The gateway could not reach a required backend..."
}
```

`x-name` and `x-description` are OpenRPC specification extensions. They
keep the artifact valid against the OpenRPC meta-schema while reserving
the standard `data` field for future typed runtime payloads.

Each method references shared errors:

```json
{
  "name": "groundhog_sendRawTransaction",
  "errors": [
    { "$ref": "#/components/errors/TxInvalid" },
    { "$ref": "#/components/errors/TxRejected" }
  ]
}
```

## Scope

The first pass documents current public error classes and attaches the
errors each method can return under the shared server/gateway model. It
does not model typed `error.data` payloads; runtime errors currently
emit no structured `data`.

If future methods introduce typed error payloads, add those payload
schemas in `api-types` and reference them from the relevant error
component's `data` shape.

## Follow-Up

`RpcServerError`, `GatewayError`, and `ERROR_INFOS` are intentionally
parallel for this first documentation pass. A future cleanup should
unify the runtime error variants and documented error catalog around a
shared error-class type so adding a new wire error cannot update one
surface without the other.
