# Zed rsvelte

A [Svelte](https://svelte.dev) extension for [Zed](https://zed.dev) that uses the
[`@rsvelte/language-server`](https://github.com/baseballyama/rsvelte/tree/main/apps/npm/language-server) —
the Rust port of the Svelte toolchain — instead of the official
`svelte-language-server`.

It provides:

- **Formatting** via `rsvelte-fmt` (install [`@rsvelte/fmt`](https://www.npmjs.com/package/@rsvelte/fmt)
  in your project; formatting is silently disabled if the binary isn't found).
- **Linting** via the bundled `rsvelte_lint` wasm engine (no extra install).

Type-checking is intentionally out of scope (use
[`@rsvelte/svelte-check`](https://www.npmjs.com/package/@rsvelte/svelte-check) as a batch checker).

## Settings

Configure the server from your Zed settings under `lsp.rsvelte-language-server.settings`:

```jsonc
{
  "lsp": {
    "rsvelte-language-server": {
      "settings": {
        "format": { "enable": true },
        "lint": { "enable": true },
        "rsvelteFmtPath": "" // explicit path to a rsvelte-fmt binary
      }
    }
  }
}
```

To route Svelte formatting through the language server, also set the formatter for
the language:

```jsonc
{
  "languages": {
    "Svelte": { "formatter": "language_server" }
  }
}
```

## Development

Install as a dev extension via `zed: install dev extension` and select this
directory. See the [Developing Extensions](https://zed.dev/docs/extensions/developing-extensions)
docs. Disable the official Svelte extension first, since both declare the `Svelte`
language.
