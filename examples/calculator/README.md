# Calculator Example

A simple calculator component exposing add, subtract, multiply, and divide as MCP tools.

## Prerequisites

- `wasm-tools` (`cargo install wasm-tools`)

## Run

1. Compile the component:

```sh
wasm-tools parse calculator.wat -o calculator.wasm
```

2. Start toolbelt:

```sh
cargo run -- config.toml calculator.wasm
```

3. Test with the [MCP Inspector](https://github.com/modelcontextprotocol/inspector) or the example curl scripts:

```sh
../curl/initialize.sh
../curl/list_tools.sh
../curl/call_tool.sh calculator.add a=4 b=3
../curl/call_tool.sh calculator.divide a=99 b=11
```
