/// adg.rs — HTTP calls to the ADG backend (login + ingest + snapshot).
///
/// Todo request leva o header `x-companion-version` (versão do Cargo.toml) — o
/// backend bloqueia versões abaixo da mínima com HTTP 426. Os erros saem tipados
/// (`AdgError`) pra outbox decidir o retry: 426 trava tudo até atualizar, 4xx
/// descarta após N tentativas, 5xx/rede retenta pra sempre.
use reqwest::Client;
use serde_json::Value;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const VERSION_HEADER: &str = "x-companion-version";

/// Erro de transporte com semântica de retry pra outbox.
#[derive(Debug, thiserror::Error)]
pub enum AdgError {
    #[error("Atualização obrigatória do companion (mínima {min_version})")]
    UpdateRequired {
        min_version: String,
        download_url: Option<String>,
    },
    /// 4xx — o payload/credencial está errado; retentar igual não resolve.
    #[error("{0}")]
    Permanent(String),
    /// 5xx / falha de rede — retentar resolve.
    #[error("{0}")]
    Transient(String),
}

fn transient(context: &str) -> impl Fn(reqwest::Error) -> AdgError + '_ {
    move |e| AdgError::Transient(format!("{context}: {e}"))
}

/// Converte uma resposta NÃO-2xx em AdgError (consome o corpo pra extrair a mensagem).
async fn error_from(resp: reqwest::Response) -> AdgError {
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_default();
    if status.as_u16() == 426 {
        return AdgError::UpdateRequired {
            min_version: body
                .get("minVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            download_url: body
                .get("downloadUrl")
                .and_then(|v| v.as_str())
                .map(String::from),
        };
    }
    let msg = body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("erro desconhecido")
        .to_string();
    if status.is_client_error() {
        AdgError::Permanent(format!("{status}: {msg}"))
    } else {
        AdgError::Transient(format!("{status}: {msg}"))
    }
}

/// Calls `POST {api_base}/api/auth/login` and returns the JWT token string.
pub async fn login(api_base: &str, email: &str, password: &str) -> Result<String, AdgError> {
    let url = format!("{api_base}/api/auth/login");
    let resp = Client::new()
        .post(&url)
        .header(VERSION_HEADER, APP_VERSION)
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()
        .await
        .map_err(transient("POST /api/auth/login"))?;

    if !resp.status().is_success() {
        return Err(error_from(resp).await);
    }
    let body: Value = resp
        .json()
        .await
        .map_err(transient("parsear resposta de login"))?;
    body.get("token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| AdgError::Permanent("campo 'token' ausente na resposta de login".into()))
}

/// Calls `POST {api_base}/api/aram-mayhem/matches` with the given payload.
/// Returns the number of rows inserted as reported by the backend.
pub async fn post_matches(
    api_base: &str,
    jwt_token: &str,
    puuid: &str,
    game_name: &str,
    tag_line: &str,
    matches: &[Value],
) -> Result<u64, AdgError> {
    let url = format!("{api_base}/api/aram-mayhem/matches");
    let payload = serde_json::json!({
        "puuid":    puuid,
        "gameName": game_name,
        "tagLine":  tag_line,
        "matches":  matches,
    });

    let resp = Client::new()
        .post(&url)
        .bearer_auth(jwt_token)
        .header(VERSION_HEADER, APP_VERSION)
        .json(&payload)
        .send()
        .await
        .map_err(transient("POST /api/aram-mayhem/matches"))?;

    if !resp.status().is_success() {
        return Err(error_from(resp).await);
    }
    let body: Value = resp
        .json()
        .await
        .map_err(transient("parsear resposta do ingest"))?;
    Ok(body.get("inserted").and_then(|v| v.as_u64()).unwrap_or(0))
}

/// Calls `POST {api_base}/api/aram-mayhem/snapshot` com o championStats cru da LiveClient.
pub async fn post_snapshot(
    api_base: &str,
    jwt_token: &str,
    game_id: i64,
    champion_stats: &Value,
    peak: &Value,
) -> Result<(), AdgError> {
    let url = format!("{api_base}/api/aram-mayhem/snapshot");
    let resp = Client::new()
        .post(&url)
        .bearer_auth(jwt_token)
        .header(VERSION_HEADER, APP_VERSION)
        .json(&serde_json::json!({ "gameId": game_id, "championStats": champion_stats, "peak": peak }))
        .send()
        .await
        .map_err(transient("POST /api/aram-mayhem/snapshot"))?;

    if !resp.status().is_success() {
        return Err(error_from(resp).await);
    }
    Ok(())
}
