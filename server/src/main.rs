//! A native Language Server for Svelte, backed by the rsvelte Rust toolchain.
//!
//! It links `rsvelte_lint` and `rsvelte_formatter` directly (no Node, no wasm,
//! no shelling out) and exposes them over LSP:
//!   * push diagnostics on open / change / save (`rsvelte_lint::runner::lint_source`)
//!   * whole-document formatting (`rsvelte_formatter::format`)
//!
//! Transport is synchronous stdio via `lsp-server` — the server is fast enough
//! (microseconds per file) that no debouncing or background threading is needed.

use std::collections::HashMap;
use std::error::Error;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{Formatting, Request as _};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentFormattingParams, InitializeParams, NumberOrString,
    OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities, TextEdit,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};

use rsvelte_core::CompileOptions;
use rsvelte_core::svelte_check::diagnostic::{Diagnostic as RsDiagnostic, DiagnosticSeverity as RsSeverity};
use rsvelte_formatter::{FormatOptions, format};
use rsvelte_lint::LintConfig;
use rsvelte_lint::runner::lint_source;

/// Effective `rsvelte.*` settings, resolved once from `initializationOptions`.
#[derive(Clone)]
struct Settings {
    format: bool,
    lint: bool,
    lint_config: LintConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: true,
            lint: true,
            lint_config: LintConfig::recommended(),
        }
    }
}

impl Settings {
    /// Tolerant parse of the client's settings blob. Accepts either the flat
    /// shape (`{ "format": { "enable": false }, ... }`) or one nested under an
    /// `"rsvelte"` key, matching what the Zed extension forwards.
    fn from_options(opts: &Option<serde_json::Value>) -> Self {
        let mut s = Settings::default();
        let Some(root) = opts else { return s };
        let scope = root.get("rsvelte").unwrap_or(root);
        if let Some(b) = scope.pointer("/format/enable").and_then(|v| v.as_bool()) {
            s.format = b;
        }
        if let Some(b) = scope.pointer("/lint/enable").and_then(|v| v.as_bool()) {
            s.lint = b;
        }
        // The `lint` object doubles as a lint-config document: rsvelte's config
        // parser reads its `extends` / `rules` / `files` / `ignores` keys (and
        // ignores `enable`). This is what lets a user reconfigure individual
        // rules — e.g. turn off `svelte/no-unused-class-name` for a Tailwind
        // project, or allow TS via `"svelte/block-lang": ["error", {"script": "ts"}]`.
        if let Some(lint) = scope.get("lint") {
            if let Ok(text) = serde_json::to_string(lint) {
                if let Ok(cfg) = LintConfig::from_json_str(&text) {
                    s.lint_config = cfg;
                }
            }
        }
        s
    }
}

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    // All protocol traffic is JSON-RPC over stdio; stdout is reserved for it, so
    // any of our own logging must go to stderr (Zed surfaces it in the LSP log).
    eprintln!("rsvelte-lsp {} starting", env!("CARGO_PKG_VERSION"));

    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_formatting_provider: Some(OneOf::Left(true)),
        ..Default::default()
    })?;

    let init_value = connection.initialize(capabilities)?;
    let init: InitializeParams = serde_json::from_value(init_value)?;
    let settings = Settings::from_options(&init.initialization_options);

    main_loop(&connection, &settings)?;
    io_threads.join()?;
    eprintln!("rsvelte-lsp stopped");
    Ok(())
}

fn main_loop(
    connection: &Connection,
    settings: &Settings,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    // Full-sync server: we hold the latest text of every open document.
    let mut docs: HashMap<Url, String> = HashMap::new();

    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                if req.method == Formatting::METHOD {
                    let (id, params) = cast_req::<Formatting>(req)?;
                    let result = handle_formatting(&docs, &params, settings);
                    connection.sender.send(Message::Response(Response {
                        id,
                        result: Some(result),
                        error: None,
                    }))?;
                } else {
                    // We only advertise formatting; decline anything else cleanly.
                    connection.sender.send(Message::Response(Response {
                        id: req.id,
                        result: Some(serde_json::Value::Null),
                        error: None,
                    }))?;
                }
            }
            Message::Notification(not) => match not.method.as_str() {
                DidOpenTextDocument::METHOD => {
                    let p = cast_not::<DidOpenTextDocument>(not)?;
                    let uri = p.text_document.uri;
                    docs.insert(uri.clone(), p.text_document.text);
                    publish(connection, &uri, &docs[&uri], settings);
                }
                DidChangeTextDocument::METHOD => {
                    let mut p = cast_not::<DidChangeTextDocument>(not)?;
                    // FULL sync: the last change carries the entire new document.
                    if let Some(change) = p.content_changes.pop() {
                        let uri = p.text_document.uri;
                        docs.insert(uri.clone(), change.text);
                        publish(connection, &uri, &docs[&uri], settings);
                    }
                }
                DidSaveTextDocument::METHOD => {
                    let p = cast_not::<DidSaveTextDocument>(not)?;
                    if let Some(text) = docs.get(&p.text_document.uri) {
                        publish(connection, &p.text_document.uri, text, settings);
                    }
                }
                DidCloseTextDocument::METHOD => {
                    let p = cast_not::<DidCloseTextDocument>(not)?;
                    docs.remove(&p.text_document.uri);
                    // Clear diagnostics for the closed document.
                    publish_diagnostics(connection, &p.text_document.uri, Vec::new());
                }
                _ => {}
            },
            Message::Response(_) => {}
        }
    }
    Ok(())
}

fn cast_req<R: lsp_types::request::Request>(
    req: Request,
) -> Result<(RequestId, R::Params), Box<dyn Error + Sync + Send>> {
    req.extract(R::METHOD).map_err(|e| Box::new(e) as _)
}

fn cast_not<N: lsp_types::notification::Notification>(
    not: Notification,
) -> Result<N::Params, Box<dyn Error + Sync + Send>> {
    not.extract(N::METHOD).map_err(|e| Box::new(e) as _)
}

/// Map an LSP document URI to a filesystem path. The path drives filename-gated
/// lint rules (e.g. SvelteKit route detection); for non-`file:` URIs we fall
/// back to the URI's path component so the file name is still meaningful.
fn uri_to_path(uri: &Url) -> PathBuf {
    uri.to_file_path().unwrap_or_else(|_| PathBuf::from(uri.path()))
}

fn handle_formatting(
    docs: &HashMap<Url, String>,
    params: &DocumentFormattingParams,
    settings: &Settings,
) -> serde_json::Value {
    if !settings.format {
        return serde_json::Value::Null;
    }
    let Some(text) = docs.get(&params.text_document.uri) else {
        return serde_json::Value::Null;
    };

    let formatted = catch_unwind(AssertUnwindSafe(|| format(text, &FormatOptions::new())));
    match formatted {
        Ok(Ok(out)) if &out != text => {
            let edit = TextEdit { range: full_range(text), new_text: out };
            serde_json::to_value(vec![edit]).unwrap_or(serde_json::Value::Null)
        }
        // No change, formatter error, or a panic on malformed input: edit nothing.
        _ => serde_json::Value::Null,
    }
}

fn publish(connection: &Connection, uri: &Url, text: &str, settings: &Settings) {
    let diagnostics = if settings.lint {
        compute_diagnostics(text, &uri_to_path(uri), &settings.lint_config)
    } else {
        Vec::new()
    };
    publish_diagnostics(connection, uri, diagnostics);
}

fn publish_diagnostics(connection: &Connection, uri: &Url, diagnostics: Vec<Diagnostic>) {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics,
        version: None,
    };
    let _ = connection.sender.send(Message::Notification(Notification {
        method: PublishDiagnostics::METHOD.to_string(),
        params: serde_json::to_value(params).unwrap_or(serde_json::Value::Null),
    }));
}

fn compute_diagnostics(text: &str, path: &Path, config: &LintConfig) -> Vec<Diagnostic> {
    let options = CompileOptions::default();
    // rsvelte's lint pass parses + analyzes; guard against a panic on input the
    // parser can't handle so a single bad keystroke doesn't crash the session.
    let result = catch_unwind(AssertUnwindSafe(|| lint_source(text, path, &options, config)));
    match result {
        Ok(diags) => diags.iter().map(to_lsp_diagnostic).collect(),
        Err(_) => Vec::new(),
    }
}

fn to_lsp_diagnostic(d: &RsDiagnostic) -> Diagnostic {
    // rsvelte positions: line is 1-based, column is 0-based in UTF-16 code units.
    // LSP wants 0-based line and 0-based UTF-16 character — so subtract 1 from
    // the line and pass the column through unchanged.
    let range = d
        .range
        .map(|r| Range {
            start: Position::new(r.start.line.saturating_sub(1), r.start.column),
            end: Position::new(r.end.line.saturating_sub(1), r.end.column),
        })
        .unwrap_or_else(|| Range::new(Position::new(0, 0), Position::new(0, 0)));

    let severity = Some(match d.severity {
        RsSeverity::Error => DiagnosticSeverity::ERROR,
        RsSeverity::Warning => DiagnosticSeverity::WARNING,
        RsSeverity::Info => DiagnosticSeverity::INFORMATION,
        RsSeverity::Hint => DiagnosticSeverity::HINT,
    });

    Diagnostic {
        range,
        severity,
        code: d.code.clone().map(NumberOrString::String),
        source: Some(d.source.to_string()),
        message: d.message.clone(),
        ..Default::default()
    }
}

/// The range spanning the entire document, in UTF-16 positions — used to emit a
/// single whole-document replacement edit on format.
fn full_range(text: &str) -> Range {
    let mut line: u32 = 0;
    let mut last_line_start = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            line += 1;
            last_line_start = i + 1;
        }
    }
    let character: u32 = text[last_line_start..]
        .chars()
        .map(|c| c.len_utf16() as u32)
        .sum();
    Range::new(Position::new(0, 0), Position::new(line, character))
}
