# Zed rsvelte

A [Svelte](https://svelte.dev) extension for [Zed](https://zed.dev) backed by a
**native Rust language server** built on the [rsvelte](https://github.com/baseballyama/rsvelte)
toolchain — no Node, no wasm, no external CLI.

The server (`server/`, crate `rsvelte-lsp`) links `rsvelte_lint` and
`rsvelte_formatter` directly and exposes them over LSP:

- **Formatting** — `textDocument/formatting` via `rsvelte_formatter::format`
  (formats `.svelte` plus embedded JS/TS/CSS in-process through `oxc_formatter`).
- **Linting** — push diagnostics via `rsvelte_lint` on open / change / save,
  honoring inline `eslint-disable` / `svelte-ignore` directives.

Type-checking is out of scope (rsvelte exposes it only as the batch CLI
[`@rsvelte/svelte-check`](https://www.npmjs.com/package/@rsvelte/svelte-check),
not over LSP).

The extension downloads the prebuilt server binary for your platform from this
repo's GitHub releases (pinned by `SERVER_TAG` in `src/rsvelte.rs`) — the same
model as the `rust-analyzer` / `gopls` extensions.

## Supported platforms

`aarch64`/`x86_64` macOS and Linux. Windows is not supported yet: the rsvelte
toolchain pulls in jemalloc, which doesn't build on MSVC.

## Settings

Configure the server under `lsp.rsvelte-language-server.settings` (forwarded to
the server as `initializationOptions`):

```jsonc
{
  "lsp": {
    "rsvelte-language-server": {
      "settings": {
        "format": { "enable": true },
        "lint": { "enable": true }
      }
    }
  }
}
```

To route Svelte formatting through the server, set the formatter for the
language (otherwise Zed's default `auto` formatter prefers Prettier):

```jsonc
{ "languages": { "Svelte": { "formatter": "language_server" } } }
```

## Development

### The extension (wasm)

```sh
cargo build --release --target wasm32-wasip1
```

Install via `zed: install dev extension` and select this directory. Disable the
official Svelte extension first — both declare the `Svelte` language.

### The native server (`server/`)

```sh
cd server
./scripts/vendor-rsvelte.sh      # clone rsvelte at the pinned rev (no submodules)
cargo build --release            # → server/target/release/rsvelte-lsp
```

The server is consumed as **path deps** against the cloned tree in
`server/.rsvelte` (gitignored). Git deps don't work because cargo recursively
fetches rsvelte's SSH/private submodules; we clone without submodules instead.

To test the server against a dev extension build **without cutting a release**,
point the extension at your local binary in your Zed settings — this overrides
the download path:

```jsonc
{
  "lsp": {
    "rsvelte-language-server": {
      "binary": {
        "path": "/absolute/path/to/server/target/release/rsvelte-lsp"
      }
    }
  }
}
```

## Releasing the server

Bump `version` in `server/Cargo.toml` and `SERVER_TAG` in `src/rsvelte.rs`, then
push a matching tag:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release-server.yml` builds all platform binaries and attaches
`rsvelte-lsp-<target>.tar.gz` assets to the release. To rebuild for an existing
tag, run the workflow manually with the tag as input.
