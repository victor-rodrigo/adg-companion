/// lcu.rs — discovers and communicates with the League Client (LCU) on Windows.
///
/// Strategy: scan running processes for `LeagueClientUx.exe`, parse its
/// command-line for `--app-port` and `--remoting-auth-token`, then build a
/// reqwest client that speaks HTTPS to `127.0.0.1:{port}` with HTTP Basic auth
/// and self-signed cert acceptance.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use reqwest::{Client, header};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use sysinfo::System;

const MAYHEM_QUEUE_ID: i64 = 2400;
// Janela 0-19: a MESMA que a UI do cliente usa. O list endpoint do LCU tem cache
// compartilhada com a UI do cliente do LoL — pedir janelas custom (ex.: 0-0) "envenena"
// o que o jogador vê (bug do histórico com 1 partida), e com a cache quente os
// parâmetros são ignorados (paginação profunda nem funciona). Por isso NUNCA
// paginamos nem pedimos outra janela; o sync de verdade é por gameId no fim do jogo.
const HISTORY_WINDOW_END: u32 = 19;

// ── LCU credentials ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LcuCreds {
    pub port:  u16,
    pub token: String,
}

/// Walk all running processes looking for `LeagueClientUx.exe` (case-insensitive)
/// and extract `--app-port` + `--remoting-auth-token` from its cmdline.
pub fn discover_lcu() -> Result<LcuCreds> {
    let mut sys = System::new_all();
    sys.refresh_all();

    for (_pid, proc) in sys.processes() {
        let name = proc.name().to_string_lossy();
        if !name.eq_ignore_ascii_case("LeagueClientUx.exe")
            && !name.eq_ignore_ascii_case("LeagueClientUx")
        {
            continue;
        }

        let cmd: Vec<String> = proc.cmd()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();

        let mut port:  Option<u16>   = None;
        let mut token: Option<String> = None;

        for arg in &cmd {
            if let Some(v) = arg.strip_prefix("--app-port=") {
                port = v.parse().ok();
            } else if let Some(v) = arg.strip_prefix("--remoting-auth-token=") {
                token = Some(v.to_string());
            }
        }

        match (port, token) {
            (Some(p), Some(t)) => return Ok(LcuCreds { port: p, token: t }),
            _ => {
                // Found the process but couldn't parse args — log and keep looking
                eprintln!("[lcu] processo LeagueClientUx encontrado mas sem port/token. cmd: {cmd:?}");
            }
        }
    }

    bail!("LeagueClientUx não encontrado — abra o cliente do LoL")
}

// ── reqwest client ────────────────────────────────────────────────────────────

/// Build a reqwest Client that accepts the LCU's self-signed certificate and
/// carries the HTTP Basic `riot:{token}` header on every request.
fn build_lcu_client(creds: &LcuCreds) -> Result<(Client, String)> {
    let basic = B64.encode(format!("riot:{}", creds.token));
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_str(&format!("Basic {basic}"))
            .context("header de autenticação inválido")?,
    );

    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .default_headers(headers)
        .build()
        .context("construir cliente HTTP do LCU")?;

    let base = format!("https://127.0.0.1:{}", creds.port);
    Ok((client, base))
}

// ── LCU data types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Summoner {
    pub puuid:    String,
    #[serde(rename = "gameName", default)]
    pub game_name: String,
    #[serde(rename = "tagLine", default)]
    pub tag_line:  String,
}

/// Full game detail from `/lol-match-history/v1/games/{gameId}`.
#[derive(Debug, Clone, Deserialize)]
pub struct GameDetail {
    #[serde(rename = "gameId")]
    pub game_id: i64,
    #[serde(rename = "gameCreation", default)]
    pub game_creation: i64,
    #[serde(rename = "gameDuration", default)]
    pub game_duration: i64,
    #[serde(rename = "participantIdentities", default)]
    pub participant_identities: Vec<ParticipantIdentity>,
    #[serde(rename = "participants", default)]
    pub participants: Vec<Participant>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParticipantIdentity {
    #[serde(rename = "participantId", default)]
    pub participant_id: i64,
    pub player: Option<PlayerIdentity>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlayerIdentity {
    #[serde(default)]
    pub puuid: String,
    #[serde(rename = "gameName", default)]
    pub game_name: String,
    #[serde(rename = "tagLine", default)]
    pub tag_line: String,
    #[serde(rename = "summonerName", default)]
    pub summoner_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Participant {
    #[serde(rename = "participantId", default)]
    pub participant_id: i64,
    #[serde(rename = "championId", default)]
    pub champion_id: i64,
    pub stats: Option<Value>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Returns the current summoner's info.
pub async fn get_summoner(creds: &LcuCreds) -> Result<Summoner> {
    let (client, base) = build_lcu_client(creds)?;
    let resp = client
        .get(format!("{base}/lol-summoner/v1/current-summoner"))
        .send()
        .await
        .context("GET /lol-summoner/v1/current-summoner")?;

    if !resp.status().is_success() {
        bail!("LCU retornou {} em /current-summoner", resp.status());
    }
    resp.json::<Summoner>().await.context("parsear summoner")
}

/// Busca a janela 0-19 do histórico (a mesma da UI), tentando o caminho canônico
/// (`current-summoner`) e caindo pro by-puuid se ele falhar — SEM sonda 0-0.
async fn fetch_default_window(client: &Client, base: &str, puuid: &str) -> Result<Vec<Value>> {
    match fetch_history_page(client, base, "current-summoner", 0, HISTORY_WINDOW_END).await {
        Ok(g) => Ok(g),
        Err(_) => fetch_history_page(client, base, puuid, 0, HISTORY_WINDOW_END).await,
    }
}

/// Backfill best-effort: ids de ARAM: Mayhem (fila 2400) da janela 0-19 que ainda não
/// foram sincronizados. Cobre jogos jogados com o app fechado; o caminho principal de
/// sync é por gameId no fim do jogo (não passa por aqui).
pub async fn get_recent_mayhem_ids(
    creds: &LcuCreds,
    puuid: &str,
    known: &HashSet<i64>,
) -> Result<Vec<i64>> {
    let (client, base) = build_lcu_client(creds)?;
    let games = fetch_default_window(&client, &base, puuid).await?;
    Ok(games
        .iter()
        .filter(|g| g.get("queueId").and_then(|v| v.as_i64()).unwrap_or(0) == MAYHEM_QUEUE_ID)
        .filter_map(|g| g.get("gameId").and_then(|v| v.as_i64()))
        .filter(|id| !known.contains(id))
        .collect())
}

/// gameId da partida EM ANDAMENTO, lido do gameflow do LCU (disponível durante o jogo).
/// É o jeito certo de casar o snapshot: pega o id exato enquanto joga, sem depender de
/// a partida já ter sido indexada no histórico (evita pegar a partida anterior por engano).
pub async fn get_current_game_id(creds: &LcuCreds) -> Result<Option<i64>> {
    let (client, base) = build_lcu_client(creds)?;
    let resp = client
        .get(format!("{base}/lol-gameflow/v1/session"))
        .send()
        .await
        .context("GET /lol-gameflow/v1/session")?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let v: Value = resp.json().await.context("parsear gameflow session")?;
    Ok(v.get("gameData")
        .and_then(|g| g.get("gameId"))
        .and_then(|id| id.as_i64())
        .filter(|&id| id > 0))
}

/// gameId da partida de ARAM: Mayhem mais recente no histórico do LCU.
/// Usado pela captura ao vivo: no fim do jogo, a partida recém-jogada já aparece aqui,
/// e a LiveClient não expõe o gameId — então casamos por "a mais nova de Mayhem".
pub async fn get_latest_mayhem_game_id(creds: &LcuCreds, puuid: &str) -> Result<Option<i64>> {
    let (client, base) = build_lcu_client(creds)?;
    let games = fetch_default_window(&client, &base, puuid).await?;
    // Preferimos a fila 2400; mas como a LiveClient não expõe o queueId pra confirmar,
    // caímos pra partida mais nova se não achar 2400 (logo após um jogo KIWI ela é a certa).
    for g in &games {
        if g.get("queueId").and_then(|v| v.as_i64()).unwrap_or(0) == MAYHEM_QUEUE_ID {
            if let Some(id) = g.get("gameId").and_then(|v| v.as_i64()) {
                return Ok(Some(id));
            }
        }
    }
    Ok(games.first().and_then(|g| g.get("gameId").and_then(|v| v.as_i64())))
}

/// Busca um lote [beg, end] do histórico e devolve o array de jogos (lida com os 2 formatos de resposta).
async fn fetch_history_page(client: &Client, base: &str, path: &str, beg: u32, end: u32) -> Result<Vec<Value>> {
    let url = format!("{base}/lol-match-history/v1/products/lol/{path}/matches?begIndex={beg}&endIndex={end}");
    let resp = client.get(&url).send().await.context("GET match history")?;
    if !resp.status().is_success() {
        bail!("LCU retornou {} em match history", resp.status());
    }
    let raw: Value = resp.json().await.context("parsear match history")?;
    let gv = raw.get("games").unwrap_or(&Value::Null);
    let arr = if let Some(inner) = gv.get("games") { inner } else { gv };
    Ok(arr.as_array().cloned().unwrap_or_default())
}

/// Fetches the full detail for a single game.
pub async fn get_game_detail(creds: &LcuCreds, game_id: i64) -> Result<GameDetail> {
    let (client, base) = build_lcu_client(creds)?;
    let resp = client
        .get(format!("{base}/lol-match-history/v1/games/{game_id}"))
        .send()
        .await
        .context("GET game detail")?;

    if !resp.status().is_success() {
        bail!("LCU retornou {} em /games/{game_id}", resp.status());
    }
    resp.json::<GameDetail>().await.context("parsear game detail")
}

// ── Stats → ADG payload mapping ───────────────────────────────────────────────

/// Extracts an integer field from a serde_json Value, returning 0 if missing.
fn opt_int(stats: &Value, key: &str) -> i64 {
    stats.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

/// Monta o payload de UM participante (pro time completo da partida).
/// Inclui o Riot ID (gameName#tagLine) pra o backend casar com usuários do ADG, e o
/// bloco RICO completo: o `/games/{gameId}` traz os ~118 campos de stats pros 10
/// players, então qualquer membro do time sobe os stats ricos de todo mundo. O blob
/// `raw` por participante é o marcador de "payload rico" no backend (hasRich).
fn build_participant(p: &Participant, ident: Option<&PlayerIdentity>) -> Value {
    let stats = p.stats.as_ref().cloned().unwrap_or(Value::Object(Default::default()));
    let win: bool = match stats.get("win") {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) == 1,
        _ => false,
    };
    let items: Vec<i64> = (0..=6)
        .map(|i| opt_int(&stats, &format!("item{i}")))
        .filter(|&v| v != 0)
        .collect();
    let augments: Vec<i64> = (1..=6)
        .map(|i| opt_int(&stats, &format!("playerAugment{i}")))
        .filter(|&v| v != 0)
        .collect();
    let (puuid, game_name, tag_line) = match ident {
        Some(pi) => (
            pi.puuid.clone(),
            if !pi.game_name.is_empty() { pi.game_name.clone() } else { pi.summoner_name.clone() },
            pi.tag_line.clone(),
        ),
        None => (String::new(), String::new(), String::new()),
    };
    serde_json::json!({
        "participantId":          p.participant_id,
        "puuid":                  puuid,
        "gameName":               game_name,
        "tagLine":                tag_line,
        "championId":             p.champion_id,
        "win":                    win,
        "kills":                  opt_int(&stats, "kills"),
        "deaths":                 opt_int(&stats, "deaths"),
        "assists":                opt_int(&stats, "assists"),
        "totalDamageToChampions": opt_int(&stats, "totalDamageDealtToChampions"),
        "totalHeal":              opt_int(&stats, "totalHeal"),
        "damageSelfMitigated":    opt_int(&stats, "damageSelfMitigated"),
        "gold":                   opt_int(&stats, "goldEarned"),
        "augments":               augments,
        "items":                  items,
        // ricos — mesmos mapeamentos LCU→backend do payload de nível superior
        "doubleKills":            opt_int(&stats, "doubleKills"),
        "tripleKills":            opt_int(&stats, "tripleKills"),
        "quadraKills":            opt_int(&stats, "quadraKills"),
        "pentaKills":             opt_int(&stats, "pentaKills"),
        "damageDealt":            opt_int(&stats, "totalDamageDealtToChampions"),
        "damageTaken":            opt_int(&stats, "totalDamageTaken"),
        "totalDamageDealt":       opt_int(&stats, "totalDamageDealt"),
        "physicalDmgChamps":      opt_int(&stats, "physicalDamageDealtToChampions"),
        "magicDmgChamps":         opt_int(&stats, "magicDamageDealtToChampions"),
        "trueDmgChamps":          opt_int(&stats, "trueDamageDealtToChampions"),
        "totalUnitsHealed":       opt_int(&stats, "totalUnitsHealed"),
        "goldSpent":              opt_int(&stats, "goldSpent"),
        "largestMultiKill":       opt_int(&stats, "largestMultiKill"),
        "largestCriticalStrike":  opt_int(&stats, "largestCriticalStrike"),
        "killingSpree":           opt_int(&stats, "largestKillingSpree"),
        "killingSprees":          opt_int(&stats, "killingSprees"),
        "longestTimeLiving":      opt_int(&stats, "longestTimeSpentLiving"),
        "timeCcing":              opt_int(&stats, "timeCCingOthers"),
        "totalCcDealt":           opt_int(&stats, "totalTimeCrowdControlDealt"),
        "visionScore":            opt_int(&stats, "visionScore"),
        "totalMinions":           opt_int(&stats, "totalMinionsKilled"),
        "neutralMinions":         opt_int(&stats, "neutralMinionsKilled"),
        "champLevel":             opt_int(&stats, "champLevel"),
        "raw":                    stats,
    })
}

/// Build the match payload object that exactly satisfies `parseRow` in
/// `mayhem.parse.ts`. Field names must match the TypeScript interface.
///
/// LCU stats key → backend field:
///   totalDamageDealtToChampions → damageDealt (legacy "damageDealt" expected by the backend as total dmg to champs)
///   totalDamageTaken            → damageTaken
///   goldEarned                  → gold
///   totalHeal                   → totalHeal
///   largestKillingSpree         → killingSpree
///   item0..item6                → items[]
///   playerAugment1..6 (non-zero)→ augments[]
///   damageSelfMitigated         → damageSelfMitigated
///   totalDamageDealt            → totalDamageDealt
///   totalDamageDealtToChampions → totalDamageToChampions
///   physicalDamageDealtToChampions → physicalDmgChamps
///   magicDamageDealtToChampions → magicDmgChamps
///   trueDamageDealtToChampions  → trueDmgChamps
///   totalUnitsHealed            → totalUnitsHealed
///   goldSpent                   → goldSpent
///   largestMultiKill            → largestMultiKill
///   largestCriticalStrike       → largestCriticalStrike
///   killingSprees               → killingSprees
///   longestTimeSpentLiving      → longestTimeLiving
///   timeCCingOthers             → timeCcing
///   totalTimeCrowdControlDealt  → totalCcDealt
///   visionScore                 → visionScore
///   totalMinionsKilled          → totalMinions
///   neutralMinionsKilled        → neutralMinions
///   champLevel                  → champLevel
pub fn build_match_payload(
    game: &GameDetail,
    participant: &Participant,
) -> Value {
    let stats = participant.stats.as_ref().cloned().unwrap_or(Value::Object(Default::default()));

    let win_val = stats.get("win");
    let win: bool = match win_val {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) == 1,
        _ => false,
    };

    // items array (7 slots: item0..item6)
    let items: Vec<i64> = (0..=6)
        .map(|i| opt_int(&stats, &format!("item{i}")))
        .collect();

    // augments: playerAugment1..6, drop zeros
    let augments: Vec<i64> = (1..=6)
        .map(|i| opt_int(&stats, &format!("playerAugment{i}")))
        .filter(|&v| v != 0)
        .collect();

    // Time completo: todos os participantes da partida (com identidade).
    let participants: Vec<Value> = game.participants.iter().map(|p| {
        let ident = game.participant_identities.iter()
            .find(|pi| pi.participant_id == p.participant_id)
            .and_then(|pi| pi.player.as_ref());
        build_participant(p, ident)
    }).collect();

    serde_json::json!({
        "gameId":       game.game_id,
        "participants": participants,
        "championId":   participant.champion_id,
        "win":          win,
        "kills":        opt_int(&stats, "kills"),
        "deaths":       opt_int(&stats, "deaths"),
        "assists":      opt_int(&stats, "assists"),
        "doubleKills":  opt_int(&stats, "doubleKills"),
        "tripleKills":  opt_int(&stats, "tripleKills"),
        "quadraKills":  opt_int(&stats, "quadraKills"),
        "pentaKills":   opt_int(&stats, "pentaKills"),
        // legacy: backend field "damageDealt" = totalDamageDealtToChampions
        "damageDealt":  opt_int(&stats, "totalDamageDealtToChampions"),
        "damageTaken":  opt_int(&stats, "totalDamageTaken"),
        "gold":         opt_int(&stats, "goldEarned"),
        "totalHeal":    opt_int(&stats, "totalHeal"),
        "killingSpree": opt_int(&stats, "largestKillingSpree"),
        "items":        items,
        "augments":     augments,
        "gameCreation": game.game_creation,
        "gameDuration": game.game_duration,
        // rich fields
        "damageSelfMitigated":    opt_int(&stats, "damageSelfMitigated"),
        "totalDamageDealt":       opt_int(&stats, "totalDamageDealt"),
        "totalDamageToChampions": opt_int(&stats, "totalDamageDealtToChampions"),
        "physicalDmgChamps":      opt_int(&stats, "physicalDamageDealtToChampions"),
        "magicDmgChamps":         opt_int(&stats, "magicDamageDealtToChampions"),
        "trueDmgChamps":          opt_int(&stats, "trueDamageDealtToChampions"),
        "totalUnitsHealed":       opt_int(&stats, "totalUnitsHealed"),
        "goldSpent":              opt_int(&stats, "goldSpent"),
        "largestMultiKill":       opt_int(&stats, "largestMultiKill"),
        "largestCriticalStrike":  opt_int(&stats, "largestCriticalStrike"),
        "killingSprees":          opt_int(&stats, "killingSprees"),
        "longestTimeLiving":      opt_int(&stats, "longestTimeSpentLiving"),
        "timeCcing":              opt_int(&stats, "timeCCingOthers"),
        "totalCcDealt":           opt_int(&stats, "totalTimeCrowdControlDealt"),
        "visionScore":            opt_int(&stats, "visionScore"),
        "totalMinions":           opt_int(&stats, "totalMinionsKilled"),
        "neutralMinions":         opt_int(&stats, "neutralMinionsKilled"),
        "champLevel":             opt_int(&stats, "champLevel"),
        // raw stats blob for backend storage
        "raw": stats,
    })
}

/// Finds the participant belonging to the local player (by puuid) in a GameDetail.
/// Falls back to the first participant if identity lookup fails.
pub fn find_my_participant<'a>(game: &'a GameDetail, my_puuid: &str) -> Option<&'a Participant> {
    // Try to resolve via participantIdentities
    let pid = game
        .participant_identities
        .iter()
        .find(|pi| {
            pi.player
                .as_ref()
                .map(|p| p.puuid == my_puuid)
                .unwrap_or(false)
        })
        .map(|pi| pi.participant_id);

    if let Some(id) = pid {
        if let Some(p) = game.participants.iter().find(|p| p.participant_id == id) {
            return Some(p);
        }
    }

    // Fallback: first participant
    game.participants.first()
}
