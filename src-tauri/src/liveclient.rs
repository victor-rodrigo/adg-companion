use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;
/// liveclient.rs — lê a LiveClient Data API (porta 2999, cert self-signed, sem auth)
/// durante a partida. Usada pra capturar o snapshot de stats finais do ARAM: Mayhem.
///
/// O ARAM Mayhem é identificado por `gameMode == "KIWI"` em `/liveclientdata/gamestats`
/// (Arena é CHERRY). A LiveClient só responde EM JOGO e só dá `championStats` completo
/// do jogador ativo (os outros 9 não têm) — por isso o snapshot é só do dono.
use std::time::Duration;

const BASE: &str = "https://127.0.0.1:2999";

fn client() -> Result<Client> {
    Client::builder()
        .danger_accept_invalid_certs(true) // cert self-signed do Riot
        .timeout(Duration::from_secs(3))
        .build()
        .context("construir cliente da LiveClient")
}

async fn get_json(path: &str) -> Result<Value> {
    let c = client()?;
    let resp = c
        .get(format!("{BASE}{path}"))
        .send()
        .await
        .context("GET LiveClient")?;
    if !resp.status().is_success() {
        anyhow::bail!("LiveClient {} em {}", resp.status(), path);
    }
    resp.json::<Value>().await.context("parsear LiveClient")
}

/// `gameMode` da partida em andamento, ou None se a LiveClient não responde (fora de jogo).
pub async fn game_mode() -> Option<String> {
    let v = get_json("/liveclientdata/gamestats").await.ok()?;
    v.get("gameMode")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
}

/// `championStats` do jogador ativo (build atual). Repassamos o objeto cru pro backend,
/// que sabe mapear os campos (attackDamage, maxHealth→healthMax, moveSpeed→movementSpeed…).
pub async fn champion_stats() -> Option<Value> {
    let v = get_json("/liveclientdata/activeplayer").await.ok()?;
    v.get("championStats").cloned()
}

/// True se a partida em andamento é ARAM: Mayhem (modo KIWI).
pub async fn is_mayhem_in_game() -> bool {
    matches!(game_mode().await.as_deref(), Some("KIWI"))
}
