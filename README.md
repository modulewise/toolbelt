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
If your component requires capabilities from the host runtime, you can
specify those capabilities in a `.toml` file (the `exposed` flag means
they are available to tools, otherwise they are only available as
dependencies for other capabilities):

`capabilities.toml:`
```toml
[wasip2]
uri = "wasmtime:wasip2"
exposed = true

[http]
uri = "wasmtime:http"
exposed = true
```

And then define the tool component in its own `.toml` file:

`flights.toml`
```toml
[flights]
uri = "file:///path/to/flight-search.wasm"
capabilities = ["wasip2", http"]
```

Pass the capability and tool files to the server instead of direct `.wasm` files:

```sh
toolbelt -c capabilities.toml -t flights.toml
```

> [!TIP]
>
> Multiple components can be defined within a single `.toml` file, and capabilities are optional (uri is required).
> Available host runtime capabilities are: `wasip2`, `http`, `io`, `inherit-network`, and `allow-ip-name-lookup`

Wasm Components can also be registered as capabilities, and they may have their own capability dependencies.
(Notice that the lower-level capabilities are not `exposed` to tools):

`runtime-capabilities.toml`
```toml
[wasip2]
uri = "wasmtime:wasip2"

[inherit-network]
uri="wasmtime:inherit-network"
```

Those runtime capabilities are then required by a component capability:

`keyvalue.toml`
```toml
[keyvalue]
uri = "../example-components/lib/valkey-client.wasm"
capabilities = ["wasip2", "inherit-network"]
exposed = true
```

And then that higher-level capability can be composed into tool components:

`incrementor.toml`
```toml
[incrementor]
uri = "../example-components/lib/incrementor.wasm"
capabilities = ["keyvalue"]

[incrementor.config]
bucket = "things"
```

Multiple capability and tool files can be passed to the server:

```sh
toolbelt -c runtime-capabilities.toml -c keyvalue.toml -t incrementor.toml
```

This allows for various combinations of reusable capability sets and tool sets.
It also provides encapsulation and promotes separation of concerns.

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
