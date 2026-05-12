# composable:mcp

MCP client support, including a WIT definition and a Wasm Component.

## The `composable:mcp/client` Interface

Functions:

- `initialize(server-url, option<initialize-request>)` -> `result<initialize-result, string>`
- `list-tools(server-url, session-id, request-id, option<list-tools-request>)` -> `result<list-tools-result, string>`
- `call-tool(server-url, session-id, request-id, call-tool-request)` -> `result<call-tool-result, string>`

The `initialize-result` includes the session-id, protocol version, capabilities, server info, and optional usage instructions. Subsequent calls pass the relevant session-id back to the server.

The `list-tools-result` includes `next-cursor` when the server has more tools beyond the current page.

The `call-tool-result` includes a list of content blocks, an `is-error` flag, and optional `structured-content` conforming to the tool's output schema.

See [`wit/package.wit`](wit/package.wit) for the full type definitions.

## The `mcp-client` World

- exports `composable:mcp/client`
- imports `composable:http/client`
- imports `wasi:logging/logging`

The `composable:http/client` import can be satisfied by the [`http-client`](https://github.com/modulewise/composable-runtime/tree/main/components/http) component. The `wasi:logging` import can be satisfied by any component exporting that interface.

## The `mcp-client` Component

Implementation of the `mcp-client` world, with source code in the [client](client/) sub-directory.
