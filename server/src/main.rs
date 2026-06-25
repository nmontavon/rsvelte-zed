//! A native Language Server for Svelte, backed by the rsvelte Rust toolchain.
//!
//! It links rsvelte directly (no Node, no wasm) and exposes three things over LSP:
//!   * **lint** diagnostics on open/change/save (`rsvelte_lint::runner::lint_source`)
//!   * **type-check** diagnostics (`rsvelte_core::svelte_check::run`, backed by
//!     `tsgo` — a native Go binary, so still no Node), run on a worker thread on
//!     open/save
//!   * whole-document **formatting** (`rsvelte_formatter::format`)
//!
//! Lint runs inline (microseconds per file). Type-checking spawns `tsgo` over an
//! svelte2tsx overlay of the whole workspace, so it runs on a background thread,
//! debounced, triggered on save — and its results are merged with the per-file
//! lint diagnostics before publishing.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, select};
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
use rsvelte_core::svelte_check::diagnostic::{
    Diagnostic as RsDiagnostic, DiagnosticSeverity as RsSeverity,
};
use rsvelte_core::svelte_check::{RunOptions, run as run_svelte_check};
use rsvelte_formatter::{FormatOptions, format};
use rsvelte_lint::LintConfig;
use rsvelte_lint::runner::lint_source;

/// Per-file diagnostics from the type-check worker, already converted to LSP.
type TypeResults = HashMap<Url, Vec<Diagnostic>>;

/// Effective `rsvelte.*` settings, resolved once from `initializationOptions`.
#[derive(Clone)]
struct Settings {
    format: bool,
    lint: bool,
    lint_config: LintConfig,
    type_check: bool,
    /// Optional workspace subdirectory to type-check (e.g. `"frontend"` in a
    /// monorepo). Relative paths resolve against the LSP root.
    type_check_root: Option<String>,
    /// Coalescing window before a triggered type-check actually runs.
    type_check_debounce_ms: u64,
    /// Re-run the type-check on save.
    type_check_on_save: bool,
    /// Re-run the type-check when a document is opened.
    type_check_on_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: true,
            lint: true,
            lint_config: LintConfig::recommended(),
            // Off by default: it needs a `tsgo`/`tsc` binary and materializes a
            // `.svelte-check/` overlay dir in the workspace, so it's opt-in.
            type_check: false,
            type_check_root: None,
            type_check_debounce_ms: 300,
            type_check_on_save: true,
            type_check_on_open: true,
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
        if let Some(b) = scope.pointer("/typeCheck/enable").and_then(|v| v.as_bool()) {
            s.type_check = b;
        }
        s.type_check_root = scope
            .pointer("/typeCheck/root")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(n) = scope.pointer("/typeCheck/debounceMs").and_then(|v| v.as_u64()) {
            s.type_check_debounce_ms = n;
        }
        // `runOn` is a list of triggers, e.g. ["save", "open"]. Absent → save + open.
        if let Some(arr) = scope.pointer("/typeCheck/runOn").and_then(|v| v.as_array()) {
            let has = |k: &str| arr.iter().any(|v| v.as_str() == Some(k));
            s.type_check_on_save = has("save");
            s.type_check_on_open = has("open");
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
    // our own logging must go to stderr (Zed surfaces it in the LSP log).
    eprintln!("rsvelte-lsp {} starting", env!("CARGO_PKG_VERSION"));

    // Honor an explicit tsgo path before any worker threads exist (env mutation
    // is only sound while single-threaded).
    if let Ok(path) = std::env::var("RSVELTE_TSGO_PATH") {
        if !path.is_empty() {
            // SAFETY: still single-threaded here — no other thread reads the env.
            unsafe { std::env::set_var("TSGO_BIN", path) };
        }
    }

    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_formatting_provider: Some(OneOf::Left(true)),
        ..Default::default()
    })?;

    let init_value = connection.initialize(capabilities)?;
    let init: InitializeParams = serde_json::from_value(init_value)?;
    let settings = Settings::from_options(&init.initialization_options);
    let workspace = workspace_root(&init, &settings);

    main_loop(&connection, &settings, workspace)?;
    io_threads.join()?;
    eprintln!("rsvelte-lsp stopped");
    Ok(())
}

/// Resolve the workspace root to type-check: the `typeCheck.root` setting if
/// given (joined onto the LSP root when relative), else the LSP root itself.
fn workspace_root(init: &InitializeParams, settings: &Settings) -> Option<PathBuf> {
    let lsp_root = init
        .workspace_folders
        .as_ref()
        .and_then(|f| f.first())
        .map(|f| &f.uri)
        .or(init.root_uri.as_ref())
        .and_then(|uri| uri.to_file_path().ok());

    match &settings.type_check_root {
        Some(sub) => {
            let p = PathBuf::from(sub);
            if p.is_absolute() {
                Some(p)
            } else {
                lsp_root.map(|root| root.join(sub))
            }
        }
        None => lsp_root,
    }
}

fn main_loop(
    connection: &Connection,
    settings: &Settings,
    workspace: Option<PathBuf>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    // Full-sync server: we hold the latest text of every open document.
    let mut docs: HashMap<Url, String> = HashMap::new();
    // Diagnostics are kept per-source so the two streams can be merged: a
    // `publishDiagnostics` replaces ALL diagnostics for a URI, so we must union
    // lint + type results before sending.
    let mut lint_diags: HashMap<Url, Vec<Diagnostic>> = HashMap::new();
    let mut type_diags: TypeResults = HashMap::new();

    // Spin up the type-check worker only when enabled and we know a workspace.
    let (trigger, results): (Option<Sender<()>>, Option<Receiver<TypeResults>>) =
        match (settings.type_check, workspace) {
            (true, Some(ws)) => {
                let (req_tx, req_rx) = crossbeam_channel::unbounded::<()>();
                let (res_tx, res_rx) = crossbeam_channel::unbounded::<TypeResults>();
                spawn_type_check_worker(ws, settings.type_check_debounce_ms, req_rx, res_tx);
                // Kick off an initial check so diagnostics appear without an edit.
                let _ = req_tx.send(());
                (Some(req_tx), Some(res_rx))
            }
            _ => (None, None),
        };

    loop {
        // When type-checking is off there's only the LSP channel to watch.
        let Some(results) = &results else {
            match connection.receiver.recv() {
                Ok(msg) => {
                    if handle_message(connection, msg, &mut docs, &mut lint_diags, &type_diags, &trigger, settings)? {
                        return Ok(());
                    }
                }
                Err(_) => return Ok(()),
            }
            continue;
        };

        select! {
            recv(connection.receiver) -> msg => match msg {
                Ok(msg) => {
                    if handle_message(connection, msg, &mut docs, &mut lint_diags, &type_diags, &trigger, settings)? {
                        return Ok(());
                    }
                }
                Err(_) => return Ok(()),
            },
            recv(results) -> res => {
                if let Ok(new_type) = res {
                    apply_type_results(connection, new_type, &lint_diags, &mut type_diags);
                }
            },
        }
    }
}

/// Handle one LSP message. Returns `Ok(true)` when the server should stop.
#[allow(clippy::too_many_arguments)]
fn handle_message(
    connection: &Connection,
    msg: Message,
    docs: &mut HashMap<Url, String>,
    lint_diags: &mut HashMap<Url, Vec<Diagnostic>>,
    type_diags: &TypeResults,
    trigger: &Option<Sender<()>>,
    settings: &Settings,
) -> Result<bool, Box<dyn Error + Sync + Send>> {
    match msg {
        Message::Request(req) => {
            if connection.handle_shutdown(&req)? {
                return Ok(true);
            }
            if req.method == Formatting::METHOD {
                let (id, params) = cast_req::<Formatting>(req)?;
                let result = handle_formatting(docs, &params, settings);
                connection.sender.send(Message::Response(Response {
                    id,
                    result: Some(result),
                    error: None,
                }))?;
            } else {
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
                update_lint(connection, &uri, &docs[&uri], lint_diags, type_diags, settings);
                if settings.type_check_on_open {
                    if let Some(trigger) = trigger {
                        let _ = trigger.send(());
                    }
                }
            }
            DidChangeTextDocument::METHOD => {
                let mut p = cast_not::<DidChangeTextDocument>(not)?;
                if let Some(change) = p.content_changes.pop() {
                    let uri = p.text_document.uri;
                    docs.insert(uri.clone(), change.text);
                    update_lint(connection, &uri, &docs[&uri], lint_diags, type_diags, settings);
                }
            }
            DidSaveTextDocument::METHOD => {
                let p = cast_not::<DidSaveTextDocument>(not)?;
                if let Some(text) = docs.get(&p.text_document.uri) {
                    update_lint(connection, &p.text_document.uri, text, lint_diags, type_diags, settings);
                }
                // Type-checking reads from disk, so save is its natural trigger.
                if settings.type_check_on_save {
                    if let Some(trigger) = trigger {
                        let _ = trigger.send(());
                    }
                }
            }
            DidCloseTextDocument::METHOD => {
                let p = cast_not::<DidCloseTextDocument>(not)?;
                docs.remove(&p.text_document.uri);
                lint_diags.remove(&p.text_document.uri);
                publish_diagnostics(connection, &p.text_document.uri, Vec::new());
            }
            _ => {}
        },
        Message::Response(_) => {}
    }
    Ok(false)
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

/// Map an LSP document URI to a filesystem path (drives filename-gated lint
/// rules; falls back to the URI path for non-`file:` URIs).
fn uri_to_path(uri: &Url) -> PathBuf {
    uri.to_file_path().unwrap_or_else(|_| PathBuf::from(uri.path()))
}

/// Recompute lint diagnostics for one document and publish them merged with
/// that document's current type diagnostics (a publish replaces ALL diagnostics
/// for a URI, so the two streams must be unioned every time either changes).
fn update_lint(
    connection: &Connection,
    uri: &Url,
    text: &str,
    lint_diags: &mut HashMap<Url, Vec<Diagnostic>>,
    type_diags: &TypeResults,
    settings: &Settings,
) {
    let diags = if settings.lint {
        compute_lint(text, &uri_to_path(uri), &settings.lint_config)
    } else {
        Vec::new()
    };
    lint_diags.insert(uri.clone(), diags);

    let mut merged = lint_diags.get(uri).cloned().unwrap_or_default();
    merged.extend(type_diags.get(uri).cloned().unwrap_or_default());
    publish_diagnostics(connection, uri, merged);
}

/// Apply a fresh whole-workspace type-check result: replace the type-diagnostic
/// map and re-publish every URI that gained or lost type diagnostics, merged
/// with that URI's current lint diagnostics.
fn apply_type_results(
    connection: &Connection,
    new_type: TypeResults,
    lint_diags: &HashMap<Url, Vec<Diagnostic>>,
    type_diags: &mut TypeResults,
) {
    let affected: HashSet<Url> = type_diags.keys().chain(new_type.keys()).cloned().collect();
    *type_diags = new_type;
    for uri in affected {
        let mut merged = lint_diags.get(&uri).cloned().unwrap_or_default();
        merged.extend(type_diags.get(&uri).cloned().unwrap_or_default());
        publish_diagnostics(connection, &uri, merged);
    }
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

fn compute_lint(text: &str, path: &Path, config: &LintConfig) -> Vec<Diagnostic> {
    let options = CompileOptions::default();
    // rsvelte's lint pass parses + analyzes; guard against a panic on input the
    // parser can't handle so a single bad keystroke doesn't crash the session.
    let result = catch_unwind(AssertUnwindSafe(|| lint_source(text, path, &options, config)));
    match result {
        Ok(diags) => diags.iter().map(to_lsp_diagnostic).collect(),
        Err(_) => Vec::new(),
    }
}

/// Background worker: on each trigger, debounce briefly, coalesce queued
/// triggers, run a whole-workspace type-check via tsgo, and send back per-file
/// LSP diagnostics. Runs serially, so saves while a check is in flight queue up
/// and collapse into one follow-up run.
fn spawn_type_check_worker(
    workspace: PathBuf,
    debounce_ms: u64,
    req_rx: Receiver<()>,
    res_tx: Sender<TypeResults>,
) {
    std::thread::spawn(move || {
        while req_rx.recv().is_ok() {
            // Debounce a burst of saves, then drain anything else queued.
            std::thread::sleep(Duration::from_millis(debounce_ms));
            while req_rx.try_recv().is_ok() {}

            let result = catch_unwind(AssertUnwindSafe(|| run_type_check(&workspace)));
            let by_uri = result.unwrap_or_default();
            if res_tx.send(by_uri).is_err() {
                break; // main loop gone
            }
        }
    });
}

/// Run rsvelte's svelte-check over the workspace with the native tsgo backend
/// and group the mapped diagnostics by document URI. Diagnostics inside the
/// generated `.svelte-check/` overlay are dropped — only real source files
/// surface to the editor.
fn run_type_check(workspace: &Path) -> TypeResults {
    let options = RunOptions {
        workspace: workspace.to_path_buf(),
        type_check: true,
        prefer_tsgo: true,
        incremental: true,
        ..RunOptions::default()
    };
    let result = run_svelte_check(&options);

    let mut by_uri: TypeResults = HashMap::new();
    for d in &result.diagnostics {
        if d.file
            .components()
            .any(|c| c.as_os_str() == ".svelte-check")
        {
            continue;
        }
        let Ok(uri) = Url::from_file_path(&d.file) else {
            continue;
        };
        by_uri.entry(uri).or_default().push(to_lsp_diagnostic(d));
    }
    by_uri
}

fn to_lsp_diagnostic(d: &RsDiagnostic) -> Diagnostic {
    // rsvelte positions: line is 1-based, column is 0-based in UTF-16 code units
    // (true for both the lint path's LineIndex and the svelte-check mapper). LSP
    // wants 0-based line and 0-based UTF-16 character — so subtract 1 from line
    // and pass the column through unchanged.
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
