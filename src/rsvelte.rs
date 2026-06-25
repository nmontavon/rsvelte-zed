use std::{collections::HashSet, env, path::PathBuf};
use zed_extension_api::{
    self as zed, serde_json, settings::LspSettings, LanguageServerId, Result,
};

struct RsvelteExtension {
    installed: HashSet<String>,
}

const SERVER_NAME: &str = "rsvelte-language-server";
const PACKAGE_NAME: &str = "@rsvelte/language-server";
const SERVER_ENTRY: &str = "dist/server.mjs";

fn get_package_path(package_name: &str) -> Result<PathBuf> {
    let path = env::current_dir()
        .map_err(|e| e.to_string())?
        .join("node_modules")
        .join(package_name);
    Ok(path)
}

impl RsvelteExtension {
    fn install_package_if_needed(
        &mut self,
        id: &LanguageServerId,
        package_name: &str,
    ) -> Result<()> {
        let installed_version = zed::npm_package_installed_version(package_name)?;

        // If package is already installed in this session, then we won't reinstall it
        if installed_version.is_some() && self.installed.contains(package_name) {
            return Ok(());
        }

        zed::set_language_server_installation_status(
            id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let latest_version = zed::npm_package_latest_version(package_name)?;

        if installed_version.as_ref() != Some(&latest_version) {
            println!("Installing {package_name}@{latest_version}...");

            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );

            if let Err(error) = zed::npm_install_package(package_name, &latest_version) {
                // If installation failed, we don't want to error but rather reuse existing version
                if installed_version.is_none() {
                    Err(error)?;
                }
            }
        } else {
            println!("Found {package_name}@{latest_version} installed");
        }

        self.installed.insert(package_name.into());
        Ok(())
    }
}

impl zed::Extension for RsvelteExtension {
    fn new() -> Self {
        Self {
            installed: HashSet::new(),
        }
    }

    fn language_server_command(
        &mut self,
        id: &LanguageServerId,
        _: &zed::Worktree,
    ) -> Result<zed::Command> {
        self.install_package_if_needed(id, PACKAGE_NAME)?;

        let path = get_package_path(PACKAGE_NAME)?
            .join(SERVER_ENTRY)
            .to_string_lossy()
            .to_string();

        Ok(zed::Command {
            command: zed::node_binary_path()?,
            args: vec![path, "--stdio".to_string()],
            env: Default::default(),
        })
    }

    fn language_server_workspace_configuration(
        &mut self,
        _: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        // The server reads `rsvelte.*` (format.enable, lint.enable, rsvelteFmtPath)
        // over `workspace/configuration`. Forward whatever the user put under
        // `lsp.rsvelte-language-server.settings` in their Zed settings, defaulting
        // to formatting + linting enabled.
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
