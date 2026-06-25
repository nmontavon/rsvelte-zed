# Zed rsvelte

A [Svelte](https://svelte.dev) extension for [Zed](https://zed.dev) that uses the
[`@rsvelte/language-server`](https://github.com/baseballyama/rsvelte/tree/main/apps/npm/language-server) —
the Rust port of the Svelte toolchain — instead of the official
`svelte-language-server`.

It provides:

- **Linting** via the `rsvelte_lint` engine, which is compiled to wasm and
  bundled inside the language server package — **works out of the box, no extra
  install.**
- **Formatting** via `rsvelte-fmt`, a **separate native binary** that the server
  shells out to (it formats `.svelte` and delegates embedded JS/TS/CSS to
  `oxfmt`). It is *not* bundled, so you must install it yourself — see
  [Formatting setup](#formatting-setup) below. If the binary isn't found,
  formatting is silently disabled (linting still works).

Type-checking is intentionally out of scope (use
[`@rsvelte/svelte-check`](https://www.npmjs.com/package/@rsvelte/svelte-check) as a batch checker).

## Formatting setup

The language server resolves the `rsvelte-fmt` binary in this order:

1. The explicit `rsvelteFmtPath` setting (see below), if set.
2. Your project's `node_modules/.bin/rsvelte-fmt`.

Pick one:

- **Per project (recommended by upstream):** add the formatter as a dev
  dependency so it lands in `node_modules/.bin`:

  ```sh
  npm i -D @rsvelte/fmt   # or: pnpm add -D @rsvelte/fmt
  ```

- **Global / shared binary:** install `@rsvelte/fmt` anywhere and point the
  server at the binary explicitly via `rsvelteFmtPath` (no per-project install
  needed). See the example in [Settings](#settings).

Then tell Zed to route Svelte formatting through the language server (otherwise
Zed's default `auto` formatter still prefers Prettier):

```jsonc
{
  "languages": {
    "Svelte": { "formatter": "language_server" }
  }
}
```

## Settings

Configure the server from your Zed settings under `lsp.rsvelte-language-server.settings`:

```jsonc
{
  "lsp": {
    "rsvelte-language-server": {
      "settings": {
        "format": { "enable": true },
        "lint": { "enable": true },
        "rsvelteFmtPath": "/abs/path/to/rsvelte-fmt" // optional; overrides node_modules resolution
      }
    }
  }
}
```

See [Formatting setup](#formatting-setup) for how `rsvelteFmtPath` fits in.

## Development

Install as a dev extension via `zed: install dev extension` and select this
directory. See the [Developing Extensions](https://zed.dev/docs/extensions/developing-extensions)
docs. Disable the official Svelte extension first, since both declare the `Svelte`
language.
