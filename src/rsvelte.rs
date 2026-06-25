use std::fs;
use zed_extension_api::{
    self as zed, serde_json, settings::LspSettings, Architecture, DownloadedFileType,
    LanguageServerId, Os, Result,
};

struct RsvelteExtension {
    cached_binary_path: Option<String>,
}

const SERVER_NAME: &str = "rsvelte-language-server";

// The native `rsvelte-lsp` binary is published as a GitHub release on this repo.
// Pin the tag so a given extension build always pulls a known-good server; bump
// both together when releasing a new server.
const RELEASE_REPO: &str = "nmontavon/rsvelte-zed";
const SERVER_TAG: &str = "v0.1.0";

impl RsvelteExtension {
    /// Resolve the rsvelte-lsp binary, downloading the platform-appropriate
    /// release asset on first use and caching it across the session.
    fn language_server_binary_path(&mut self, id: &LanguageServerId) -> Result<String> {
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Ok(path.clone());
            }
        }

        let (platform, arch) = zed::current_platform();
        let target = release_target(platform, arch)?;

        // Versioned install dir so a tag bump triggers a fresh download and old
        // versions can be garbage-collected below.
        let version_dir = format!("rsvelte-lsp-{SERVER_TAG}");
        let binary_path = format!("{version_dir}/rsvelte-lsp");

        if !fs::metadata(&binary_path).is_ok_and(|m| m.is_file()) {
            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );

            let release = zed::github_release_by_tag_name(RELEASE_REPO, SERVER_TAG)?;
            let asset_name = format!("rsvelte-lsp-{target}.tar.gz");
            let asset = release
                .assets
                .iter()
                .find(|a| a.name == asset_name)
                .ok_or_else(|| format!("no release asset named `{asset_name}` on {SERVER_TAG}"))?;

            zed::download_file(
                &asset.download_url,
                &version_dir,
                DownloadedFileType::GzipTar,
            )
            .map_err(|e| format!("failed to download {asset_name}: {e}"))?;

            zed::make_file_executable(&binary_path)?;

            // Drop any previously-downloaded versions.
            if let Ok(entries) = fs::read_dir(".") {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("rsvelte-lsp-") && name != version_dir {
                        fs::remove_dir_all(entry.path()).ok();
                    }
                }
            }
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

/// Map Zed's platform/arch to the Rust target triple used in the release asset
/// names. Windows is intentionally unsupported: the rsvelte toolchain pulls in
/// jemalloc, which doesn't build on MSVC.
fn release_target(platform: Os, arch: Architecture) -> Result<&'static str> {
    Ok(match (platform, arch) {
        (Os::Mac, Architecture::Aarch64) => "aarch64-apple-darwin",
        (Os::Mac, Architecture::X8664) => "x86_64-apple-darwin",
        (Os::Linux, Architecture::X8664) => "x86_64-unknown-linux-gnu",
        (Os::Linux, Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
        (Os::Windows, _) => {
            return Err("rsvelte-lsp does not provide Windows binaries yet".into());
        }
        (_, arch) => {
            return Err(format!("unsupported architecture: {arch:?}"));
        }
    })
}

impl zed::Extension for RsvelteExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary = LspSettings::for_worktree(SERVER_NAME, worktree)
            .ok()
            .and_then(|s| s.binary);

        // An explicit binary path override skips the download entirely. This is
        // how you run a local build (or any custom server) — set, in settings:
        //   "lsp": { "rsvelte-language-server": { "binary": { "path": "…" } } }
        if let Some(bin) = &binary {
            if let Some(path) = &bin.path {
                return Ok(zed::Command {
                    command: path.clone(),
                    args: bin.arguments.clone().unwrap_or_default(),
                    env: bin
                        .env
                        .clone()
                        .map(|m| m.into_iter().collect())
                        .unwrap_or_default(),
                });
            }
        }

        // Otherwise download the pinned release binary for this platform.
        let binary_path = self.language_server_binary_path(id)?;
        Ok(zed::Command {
            command: binary_path,
            args: binary.and_then(|b| b.arguments).unwrap_or_default(),
            env: Default::default(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        // The native server reads its `rsvelte.*` config (format.enable,
        // lint.enable) from initializationOptions. Forward whatever the user set
        // under `lsp.rsvelte-language-server.settings`, defaulting to both on.
        let settings = LspSettings::for_worktree(SERVER_NAME, worktree)
            .ok()
            .and_then(|s| s.settings)
            .unwrap_or_else(|| {
                serde_json::json!({
                    "format": { "enable": true },
                    "lint": { "enable": true }
                })
            });

        Ok(Some(serde_json::json!({ "rsvelte": settings })))
    }
}

zed::register_extension!(RsvelteExtension);
