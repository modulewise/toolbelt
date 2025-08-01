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
toolbelt oci://ghcr.io/modulewise/hello:0.1.0 oci://ghcr.io/modulewise/calculator:0.1.0
```

> [!TIP]
>
> If you'd like to build the Wasm Components locally, clone the
> [example-components](https://github.com/modulewise/example-components)
> project and run the build script as described in its README.

## Run Components with Dependencies

By default, components operate in a least-privilege capability mode.
If your component requires features from the host runtime, you can
specify those features in a `.toml` file. The `enables` property
indicates the scope within which they will be available:

```toml
[wasip2]
uri = "wasmtime:wasip2"
enables = "any"

[http]
uri = "wasmtime:http"
enables = "any"
```

And then define the "exposed" tool component that "expects" those features:

```toml
[flights]
uri = "file:///path/to/flight-search.wasm"
expects = ["wasip2", "http"]
exposed = true
```

Pass the definition file to the server instead of direct `.wasm` files:

```sh
toolbelt flights.toml
```

Wasm Components can also be defined to enable other components, and they may have their own dependencies.
Notice this time the runtime features are not directly available to exposed "tool" components, but only
to the *internal* components that are enabling exposed components:

`runtime-features.toml`
```toml
[wasip2]
uri = "wasmtime:wasip2"
enables = "unexposed"

[inherit-network]
uri = "wasmtime:inherit-network"
enables = "unexposed"
```

Those runtime features are then expected by an enabling component:

`keyvalue.toml`
```toml
[keyvalue]
uri = "../example-components/lib/valkey-client.wasm"
expects = ["wasip2", "inherit-network"]
enables = "exposed"
```

Finally, that component can be composed into exposed "tool" components.
In this case, the exposed component will also be composed with config:

`incrementor.toml`
```toml
[incrementor]
uri = "../example-components/lib/incrementor.wasm"
expects = ["keyvalue"]
exposed = true

[incrementor.config]
bucket = "increments"
```

Now these files can all be passed to the server:

```sh
toolbelt runtime-features.toml keyvalue.toml incrementor.toml
```

This allows for various combinations of runtime features, enabling components,
and components that will be exposed as tools. It also promotes responsibility-driven
separation of concerns between supporting infrastructure and exposed functionality.

## Test with MCP Inspector

1. Run the server as described above.

2. Start the [MCP Inspector](https://github.com/modelcontextprotocol/inspector?tab=readme-ov-file#quick-start-ui-mode).

3. Ensure the `Streamable HTTP` Transport Type is selected.

4. Ensure the specified URL is `http://127.0.0.1:3001/mcp` (replace host or port if not using defaults).

5. Click `Connect` and then `List Tools`.

6. Select a Tool, provide parameter values, and click `Run Tool`.

## License

Copyright (c) 2025 Modulewise Inc and the Modulewise Toolbelt contributors.

Apache License v2.0: see [LICENSE](./LICENSE) for details.
