/// store.rs — persists JWT token (OS keychain) + config file (api_base, synced_ids).
use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "com.adg.companion";
const KEYRING_USER: &str = "jwt_token";

// ── Keychain (JWT) ────────────────────────────────────────────────────────────

/// Saves the JWT token in the OS keychain.
pub fn save_token(token: &str) -> Result<()> {
    let entry = Entry::new(KEYRING_SERVICE, KEYRING_USER).context("criar entrada no keychain")?;
    entry
        .set_password(token)
        .context("salvar token no keychain")
}

/// Retrieves the JWT token from the OS keychain. Returns None if not found.
pub fn load_token() -> Result<Option<String>> {
    let entry = Entry::new(KEYRING_SERVICE, KEYRING_USER).context("criar entrada no keychain")?;
    match entry.get_password() {
        Ok(t) => Ok(Some(t)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("erro ao ler token do keychain: {e}")),
    }
}

/// Deletes the JWT token from the OS keychain.
pub fn delete_token() -> Result<()> {
    let entry = Entry::new(KEYRING_SERVICE, KEYRING_USER).context("criar entrada no keychain")?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()), // already gone
        Err(e) => Err(anyhow::anyhow!("erro ao remover token do keychain: {e}")),
    }
}

// ── Config file ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Set of gameIds already POSTed to the backend (deduplication).
    #[serde(default)]
    pub synced_ids: HashSet<i64>,

    /// Timestamp (ms) of the last successful sync.
    #[serde(default)]
    pub last_sync_ms: Option<i64>,

    /// Counter of total partidas sent lifetime.
    #[serde(default)]
    pub total_synced: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            synced_ids: HashSet::new(),
            last_sync_ms: None,
            total_synced: 0,
        }
    }
}

fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("não foi possível determinar o diretório de configuração")?
        .join("adg-companion");
    std::fs::create_dir_all(&dir).context("criar diretório de configuração")?;
    Ok(dir.join("config.json"))
}

/// Caminho da outbox (fila persistente de envios) — ao lado do config.json.
pub fn outbox_path() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join("adg-companion").join("outbox.json"))
        .unwrap_or_else(|| PathBuf::from("adg-companion-outbox.json"))
}

pub fn load_config() -> Config {
    match (|| -> Result<Config> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(Config::default());
        }
        let raw = std::fs::read_to_string(&path).context("ler config.json")?;
        serde_json::from_str(&raw).context("parsear config.json")
    })() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[store] falha ao carregar config, usando padrão: {e}");
            Config::default()
        }
    }
}

pub fn save_config(cfg: &Config) -> Result<()> {
    let path = config_path()?;
    let raw = serde_json::to_string_pretty(cfg).context("serializar config")?;
    std::fs::write(&path, raw).context("salvar config.json")
}
