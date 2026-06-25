# Zed rsvelte

A [Svelte](https://svelte.dev) extension for [Zed](https://zed.dev) backed by a
**native Rust language server** built on the [rsvelte](https://github.com/baseballyama/rsvelte)
toolchain — no Node, no wasm, no external CLI.

The server (`server/`, crate `rsvelte-lsp`) links `rsvelte_lint`,
`rsvelte_formatter`, and `rsvelte_core`'s `svelte_check` directly and exposes
them over LSP:

- **Formatting** — `textDocument/formatting` via `rsvelte_formatter::format`
  (formats `.svelte` plus embedded JS/TS/CSS in-process through `oxc_formatter`).
- **Linting** — push diagnostics via `rsvelte_lint` on open / change / save,
  honoring inline `eslint-disable` / `svelte-ignore` directives.
- **Type-checking** (opt-in) — push TypeScript diagnostics via
  `rsvelte_core::svelte_check` on save, backed by **tsgo** (Microsoft's native
  Go TypeScript — *not* Node). See [Type-checking](#type-checking).

Interactive TS features (go-to-definition, hover, completion) are still out of
scope — those need a TypeScript *language service* over the svelte2tsx overlay,
which rsvelte hasn't shipped as an LSP yet. This server does type-error
diagnostics only.

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

### Configuring lint rules

By default the linter runs rsvelte's `recommended` preset — **every** rule at its
default severity. Two of those rules are noisy for a typical TypeScript +
Tailwind SvelteKit project: `svelte/block-lang` wants `<script lang="ts">` to drop
its `lang` attribute, and `svelte/no-unused-class-name` flags every Tailwind
utility class (it has no Tailwind awareness — it only matches classes against a
local `<style>` block).

The `lint` object doubles as a lint-config document: its `extends` / `rules` /
`files` / `ignores` keys follow the same shape as a `rsvelte-lint.json`. A rule
value is a severity (`"off"` / `"warn"` / `"error"`) or a `[severity, options]`
pair. So you can fix both at the source:

```jsonc
{
  "lsp": {
    "rsvelte-language-server": {
      "settings": {
        "lint": {
          "enable": true,
          "rules": {
            // Tailwind: classes live in the framework, not a <style> block.
            "svelte/no-unused-class-name": "off",
            // Allow (in fact require) lang="ts" on <script>.
            "svelte/block-lang": ["error", { "script": "ts" }]
          }
        }
      }
    }
  }
}
```

`"extends": ["none"]` flips the baseline so nothing runs unless you opt a rule
in. Inline `eslint-disable` / `svelte-ignore` comments are also honored per-file.

### Type-checking

Off by default. When enabled, the server runs rsvelte's `svelte-check` over the
workspace **on save** and pushes TypeScript diagnostics (merged with lint),
backed by **tsgo** — Microsoft's native Go TypeScript, so still no Node runtime.

```jsonc
{
  "lsp": {
    "rsvelte-language-server": {
      "settings": {
        "typeCheck": {
          "enable": true,
          // Optional: subdirectory to check, for a monorepo where the Svelte
          // app isn't at the workspace root.
          "root": "frontend",
          // Optional: explicit path to a tsgo binary. Otherwise resolved from
          // node_modules/.bin or $PATH.
          "tsgoPath": "/abs/path/to/tsgo",
          // Optional: coalescing window before a triggered check runs (default 300).
          "debounceMs": 300,
          // Optional: when to (re)run. Default ["save", "open"] — re-check on
          // save and when a document is opened. There is no "change": the
          // checker reads from disk, so it can only reflect saved content.
          "runOn": ["save", "open"]
        }
      }
    }
  }
}
```

Requirements and caveats:

- **You need a `tsgo` binary.** Easiest: `npm i -D @typescript/native-preview`
  (ships a prebuilt native `tsgo`; running it needs no Node). The server finds
  `node_modules/.bin/tsgo`, anything on `$PATH`, or `typeCheck.tsgoPath`. If none
  is found, type-checking silently no-ops.
- It materializes a **`.svelte-check/` overlay directory** in the checked
  workspace — add it to your `.gitignore`.
- Diagnostics update **on save**, not as-you-type (the checker reads from disk),
  and it type-checks the whole workspace, so on very large projects expect a
  short delay after saving.

#### Scope: per-file lint vs whole-workspace type-check

- **Lint** runs on the **single changed file**, on every change — live and cheap.
- **Type-check** runs over the **whole workspace** (necessarily — TS types are
  cross-file, so editing one component can change errors in another). The
  svelte2tsx overlay step is incremental (unchanged files are cached), but the
  tsgo pass loads the full program. This is why type diagnostics are on-save and
  can update many files at once.

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
