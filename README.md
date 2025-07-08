# Modulewise Toolbelt

A [Model Context Protocol](https://modelcontextprotocol.io/) (MCP) Server that exposes [Wasm Components](https://component-model.bytecodealliance.org) as Tools.

**This is currently an early-stage non-production prototype.**

(auth, observability, and OCI support are on the roadmap)

## Run

One or more `.wasm` files may be provided as command line arguments (see [example-components](https://github.com/modulewise/example-components)):

```sh
cargo run -- hello.wasm calculator.wasm
```

By default, components operate in a least-privilege capability mode.
If your component requires capabilities from the host runtime, use a `.toml` file:

```
[flights]
uri="file:///path/to/flight-search.wasm"
capabilities=["http"]
```

Then pass that to the server instead of a direct `.wasm` file:

```sh
cargo run -- flights.toml
```

> [!TIP]
>
> Multiple components can be defined within a single `.toml` file, and capabilities are optional (uri is required).
> Currently supported capabilities are: `http`, `inherit-network`, and `allow-ip-name-lookup`

## Test with MCP Inspector

1. Run the server as described above.

2. Start the [MCP Inspector](https://github.com/modelcontextprotocol/inspector?tab=readme-ov-file#quick-start-ui-mode).

3. Ensure the `SSE` Transport Type is selected.

4. Ensure the specified URL is `http://127.0.0.1:3001/sse` (or replace host and port if not using defaults).

5. Click `Connect` and then `List Tools`.

6. Select a Tool, provide parameter values, and click `Run Tool`.

## License

Copyright (c) 2025 Modulewise Inc and the Modulewise Toolbelt contributors.

Apache License v2.0: see [LICENSE](./LICENSE) for details.
