# Modulewise Toolbelt

A [Model Context Protocol](https://modelcontextprotocol.io/) (MCP) Server that exposes [Wasm Components](https://component-model.bytecodealliance.org) as Tools.

**This is currently an early-stage non-production prototype.**

(auth and observability are on the roadmap)

## Build

Prerequisite: a current [rust toolchain](https://www.rust-lang.org/tools/install)

Clone the [toolbelt](https://github.com/modulewise/toolbelt) project if you have not already.

Then from within the `toolbelt` directory:

```
cargo install --path .
```

That will build the binary with the `release` profile and add
it to your cargo bin directory which should be on your PATH.

## Run Simple Components

Provide the path to one or more `.wasm` files as command line arguments:

```sh
toolbelt hello.wasm calculator.wasm
```

Or you can specify OCI URIs for published Wasm Components, such as these:

```sh
toolbelt oci://ghcr.io/modulewise/demo/hello:0.2.0 \
         oci://ghcr.io/modulewise/demo/calculator:0.2.0
```

> [!TIP]
>
> If you'd like to build the Wasm Components locally, clone the
> [modulewise/demos](https://github.com/modulewise/demos) project and follow the build instructions in
> [components/README.md](https://github.com/modulewise/demos/blob/main/components/README.md)

## Run Components with Dependencies

By default, components operate in a least-privilege capability mode.
If your component requires capabilities from the host runtime, you can
specify those capabilities in a `.toml` file:

```toml
[capability.http]
type = "wasi:http"
```

And then define the tool component that imports one or more capabilities:

```toml
[component.flights]
uri = "file:///path/to/flight-search.wasm"
imports = ["http"]
```

Pass the definition file to the server instead of direct `.wasm` files:

```sh
toolbelt flights.toml
```

Wasm Components can also import other components which may have their own dependencies:

`components.toml`
```toml
[component.incrementor]
uri = "../demos/components/lib/incrementor.wasm"
imports = ["keyvalue"]

[component.incrementor.config]
bucket = "increments"

[component.keyvalue]
uri = "../demos/components/lib/valkey-client.wasm"
imports = ["wasip2"]
```

And responsibilities can be separated across multiple files:

`capabilities.toml`
```toml
[capability.wasip2]
type = "wasi:p2"
```

Now these files can be passed to the server:

```sh
toolbelt components.toml capabilities.toml
```

This allows for various combinations of host capabilities and guest components.
It also promotes responsibility-driven separation of concerns between supporting
infrastructure and domain-centric tools.

## Configure the MCP Gateway

When no `[gateway]` is configured, toolbelt starts a default MCP server on
`127.0.0.1:3001` that auto-discovers top-level components (those not imported
by other components).

For explicit control, add a `[gateway]` section with `type = "mcp"` to your
definition file.

### Discover components to expose as tools with a selector

Use `component-selector` to match components by metadata:

```toml
[gateway.mcp]
type = "mcp"
port = 3001
component-selector = "!dependents"
```

This exposes all top-level components as tools. Other selector expressions
are supported, for example `labels.domain=shopping` or `name=greeter`.

### Define tools explicitly

Use `[gateway.mcp.tool.*]` entries to map specific component functions to
named tools:

```toml
[gateway.mcp]
type = "mcp"
port = 3001

[gateway.mcp.tool.greeter]
component = "greeter"
function = "greet"
description = "Greet someone by name"

[gateway.mcp.tool.adder]
component = "calculator"
function = "operations.add"
```

The tool name comes from the TOML key (`greeter` and `adder` above).
The function should be qualified by interface if the specified function
is exported via interface, or directly if exported at the WIT world level.
The `description` field is optional but plays an important role in providing
instructions to a calling agent.

### Combine both

Selectors and explicit tools can be used together. Explicit tool definitions
take precedence on name collisions:

```toml
[gateway.mcp]
type = "mcp"
port = 3001
component-selector = "!dependents"

[gateway.mcp.tool.greeter]
component = "greeter"
function = " greet"
description = "Custom description for the greet tool"
```

### Origin validation

Toolbelt validates the `Origin` header if present on requests per the MCP spec.
When binding to a loopback address, localhost origins are allowed by default.
Otherwise, all origins are denied unless explicitly configured:

```toml
[gateway.mcp]
type = "mcp"
port = 8080
host = "0.0.0.0"
allowed-origins = ["app.example.com", "localhost"]
```

## Test with MCP Inspector

1. Run the server as described above.

2. Start the [MCP Inspector](https://github.com/modelcontextprotocol/inspector?tab=readme-ov-file#quick-start-ui-mode).

3. Ensure the `Streamable HTTP` Transport Type is selected.

4. Ensure the specified URL is `http://127.0.0.1:3001/mcp` (replace host or port if not using defaults).

5. Click `Connect` and then `List Tools`.

6. Select a Tool, provide parameter values, and click `Run Tool`.

## License

Copyright (c) 2026 Modulewise Inc and the Modulewise Toolbelt contributors.

Apache License v2.0: see [LICENSE](./LICENSE) for details.
