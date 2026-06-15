/// lib.rs — Tauri application library entry point.
///
/// Exposes `run()` which sets up shared state, registers commands, and starts
/// the background tasks (sync + live capture) before entering the Tauri event loop.
///
/// Motor de sync (sem o list endpoint do histórico — ver lcu.rs):
///   • live capture detecta a partida de Mayhem e captura stats + gameId (gameflow);
///   • no fim do jogo, snapshot e partida entram na OUTBOX persistente (disco);
///   • o dreno roda no fim do jogo, no ciclo de 20s e no startup: resolve PendingMatch
///     via /games/{gameId} (stats ricos dos 10 players) e envia tudo; só remove após 2xx.

mod adg;
mod lcu;
mod liveclient;
mod outbox;
mod store;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, State};
use tokio::sync::Mutex;

// ── Shared AppState ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct AppState {
    /// Persisted settings + dedup set.
    pub config: store::Config,

    /// Fila persistente de envios pendentes (matches + snapshots).
    pub outbox: outbox::Outbox,

    /// JWT token loaded at startup; updated after login.
    pub token: Option<String>,

    /// Last known summoner (populated by sync).
    pub summoner: Option<SummonerInfo>,

    /// Whether the LCU was reachable in the last sync attempt.
    pub lcu_connected: bool,

    /// Backend exigiu atualização (HTTP 426) — bloqueia envios até atualizar.
    pub update_required: Option<UpdateInfo>,

    /// Sessão do LCU (port:token) já backfillada — 1 backfill por sessão.
    pub backfilled_lcu: Option<String>,

    /// Last error message (cleared on success).
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummonerInfo {
    #[serde(rename = "gameName")]
    pub game_name: String,
    #[serde(rename = "tagLine")]
    pub tag_line:  String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    #[serde(rename = "minVersion")]
    pub min_version: String,
    #[serde(rename = "downloadUrl")]
    pub download_url: Option<String>,
}

/// Type alias for the Arc<Mutex<AppState>> used as Tauri managed state.
pub type SharedState = Arc<Mutex<AppState>>;

// ── Frontend-visible state snapshot ──────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct FrontendState {
    #[serde(rename = "lcuConnected")]
    lcu_connected: bool,
    summoner:      Option<SummonerInfo>,
    #[serde(rename = "loggedIn")]
    logged_in:     bool,
    #[serde(rename = "apiBase")]
    api_base:      String,
    #[serde(rename = "syncedCount")]
    synced_count:  u64,
    #[serde(rename = "lastSync")]
    last_sync:     Option<i64>,
    #[serde(rename = "lastError")]
    last_error:    Option<String>,
    #[serde(rename = "updateRequired")]
    update_required: Option<UpdateInfo>,
    version:       String,
    #[serde(rename = "pendingCount")]
    pending_count: usize,
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// Returns the current application state to the frontend.
#[tauri::command]
async fn get_state(state: State<'_, SharedState>) -> Result<FrontendState, String> {
    let s = state.lock().await;
    Ok(FrontendState {
        lcu_connected:   s.lcu_connected,
        summoner:        s.summoner.clone(),
        logged_in:       s.token.is_some(),
        api_base:        api_base(),
        synced_count:    s.config.total_synced,
        last_sync:       s.config.last_sync_ms,
        last_error:      s.last_error.clone(),
        update_required: s.update_required.clone(),
        version:         adg::APP_VERSION.to_string(),
        pending_count:   s.outbox.len(),
    })
}

/// URL base da API do ADG — assada no build via env `ADG_API_URL` (sem segredo, não
/// configurável pelo usuário). Default: localhost para desenvolvimento.
fn api_base() -> String {
    option_env!("ADG_API_URL").unwrap_or("http://localhost:3100").to_string()
}

/// Logs in to ADG and stores the JWT in the OS keychain.
#[tauri::command]
async fn login(
    email:    String,
    password: String,
    state:    State<'_, SharedState>,
) -> Result<(), String> {
    let api_base = api_base();

    let token = adg::login(&api_base, &email, &password)
        .await
        .map_err(|e| e.to_string())?;

    store::save_token(&token).map_err(|e| e.to_string())?;

    let mut s = state.lock().await;
    s.token = Some(token);
    Ok(())
}

/// Clears the session (keychain token removed, state cleared).
#[tauri::command]
async fn logout(state: State<'_, SharedState>) -> Result<(), String> {
    store::delete_token().map_err(|e| e.to_string())?;
    let mut s = state.lock().await;
    s.token = None;
    s.summoner = None;
    s.lcu_connected = false;
    Ok(())
}

/// Sync manual: backfill (1x por sessão do LCU) + dreno da outbox.
/// Returns `{ sent: u64, total: u64 }` (total = pendentes restantes na outbox).
#[tauri::command]
async fn sync_now(state: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    run_sync(state.inner().clone()).await.map_err(|e| e.to_string())
}

/// Abre uma URL http(s) no navegador padrão (ex.: download da versão nova).
#[tauri::command]
async fn open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL inválida".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

// ── Sync logic (shared between command + background task) ─────────────────────

async fn run_sync(shared: SharedState) -> Result<serde_json::Value> {
    let (token, api_base) = {
        let s = shared.lock().await;
        let token = match &s.token {
            Some(t) => t.clone(),
            None    => anyhow::bail!("não logado"),
        };
        (token, api_base())
    };

    // O LCU é opcional pro dreno (payloads prontos e snapshots vão sem ele);
    // é necessário pro backfill e pra resolver PendingMatch → MatchPayload.
    let creds = lcu::discover_lcu().ok();
    {
        shared.lock().await.lcu_connected = creds.is_some();
    }

    // Identidade do invocador — 1x por ciclo, quando o LCU está de pé.
    let mut summoner: Option<lcu::Summoner> = None;
    if let Some(c) = &creds {
        match lcu::get_summoner(c).await {
            Ok(sm) => {
                {
                    let mut s = shared.lock().await;
                    s.summoner = Some(SummonerInfo {
                        game_name: sm.game_name.clone(),
                        tag_line:  sm.tag_line.clone(),
                    });
                }
                summoner = Some(sm);
            }
            Err(e) => eprintln!("[sync] summoner indisponível: {e}"),
        }
    }

    // Backfill best-effort: 1x por sessão do LCU, janela 0-19 (a mesma da UI — não
    // envenena o histórico do cliente; ver lcu.rs).
    if let (Some(c), Some(sm)) = (&creds, &summoner) {
        let session_key = format!("{}:{}", c.port, c.token);
        let needs_backfill =
            { shared.lock().await.backfilled_lcu.as_deref() != Some(session_key.as_str()) };
        if needs_backfill {
            let known = { shared.lock().await.config.synced_ids.clone() };
            match lcu::get_recent_mayhem_ids(c, &sm.puuid, &known).await {
                Ok(ids) => {
                    let mut s = shared.lock().await;
                    for id in ids {
                        if !s.outbox.has_match_for(id) {
                            s.outbox.push(outbox::OutboxItem::PendingMatch { game_id: id });
                        }
                    }
                    s.backfilled_lcu = Some(session_key);
                }
                Err(e) => eprintln!("[sync] backfill falhou (retenta no próximo ciclo): {e}"),
            }
        }
    }

    let sent = drain_outbox(&shared, &token, &api_base, creds.as_ref(), summoner.as_ref()).await;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let pending = {
        let mut s = shared.lock().await;
        s.config.last_sync_ms = Some(now_ms);
        if s.outbox.len() == 0 && s.update_required.is_none() {
            s.last_error = None;
        }
        store::save_config(&s.config).ok();
        s.outbox.len() as u64
    };

    Ok(serde_json::json!({ "sent": sent, "total": pending }))
}

/// Drena a outbox: resolve PendingMatch (precisa do LCU), envia payloads e snapshots.
/// Retorna quantas partidas o backend aceitou neste dreno. Nunca segura o mutex
/// através de um await — snapshot das entries, locks curtos pra mutação.
async fn drain_outbox(
    shared: &SharedState,
    token: &str,
    api_base: &str,
    creds: Option<&lcu::LcuCreds>,
    summoner: Option<&lcu::Summoner>,
) -> u64 {
    // 426 pendente? Não tenta nada até o app ser atualizado.
    if shared.lock().await.update_required.is_some() {
        return 0;
    }

    let entries = { shared.lock().await.outbox.entries() };
    let mut sent: u64 = 0;

    for entry in entries {
        match &entry.item {
            outbox::OutboxItem::PendingMatch { game_id } => {
                let (Some(c), Some(sm)) = (creds, summoner) else { continue };
                match lcu::get_game_detail(c, *game_id).await {
                    Ok(game) => {
                        if let Some(part) = lcu::find_my_participant(&game, &sm.puuid) {
                            let payload = lcu::build_match_payload(&game, part);
                            let mut s = shared.lock().await;
                            s.outbox.replace_item(
                                entry.id,
                                outbox::OutboxItem::MatchPayload { game_id: *game_id, payload },
                            );
                        } else {
                            // partida sem participantes — não tem o que enviar
                            let mut s = shared.lock().await;
                            s.outbox.record_permanent_failure(entry.id, "participante não encontrado no jogo");
                        }
                    }
                    Err(e) => {
                        // jogo ainda não indexado / LCU instável → fica pro próximo dreno
                        let mut s = shared.lock().await;
                        s.outbox.record_transient_failure(entry.id, &e.to_string());
                    }
                }
            }
            outbox::OutboxItem::MatchPayload { game_id, payload } => {
                // o POST /matches exige a identidade Riot (validação no backend)
                let Some(sm) = summoner else { continue };
                let result = adg::post_matches(
                    api_base, token, &sm.puuid, &sm.game_name, &sm.tag_line,
                    std::slice::from_ref(payload),
                ).await;
                let mut s = shared.lock().await;
                match result {
                    Ok(n) => {
                        s.outbox.remove(entry.id);
                        s.config.synced_ids.insert(*game_id);
                        s.config.total_synced += n;
                        sent += n;
                    }
                    Err(e) => handle_send_error(&mut s, entry.id, e),
                }
            }
            outbox::OutboxItem::Snapshot { game_id, champion_stats, peak } => {
                let result = adg::post_snapshot(api_base, token, *game_id, champion_stats, peak).await;
                let mut s = shared.lock().await;
                match result {
                    Ok(()) => {
                        s.outbox.remove(entry.id);
                        eprintln!("[outbox] snapshot enviado pra game {game_id}");
                    }
                    Err(e) => handle_send_error(&mut s, entry.id, e),
                }
            }
        }

        // 426 em qualquer envio → para o dreno inteiro (itens ficam retidos).
        if shared.lock().await.update_required.is_some() {
            break;
        }
    }

    sent
}

/// Aplica a semântica de retry do AdgError num item da outbox.
fn handle_send_error(s: &mut AppState, entry_id: u64, e: adg::AdgError) {
    match e {
        adg::AdgError::UpdateRequired { min_version, download_url } => {
            s.last_error = Some(format!("Atualização obrigatória (mínima {min_version})"));
            s.update_required = Some(UpdateInfo { min_version, download_url });
            // o item fica intacto na outbox — sobe depois do update
        }
        adg::AdgError::Permanent(msg) => {
            s.last_error = Some(msg.clone());
            s.outbox.record_permanent_failure(entry_id, &msg);
        }
        adg::AdgError::Transient(msg) => {
            s.last_error = Some(msg.clone());
            s.outbox.record_transient_failure(entry_id, &msg);
        }
    }
}

// ── Background auto-sync task ─────────────────────────────────────────────────

fn spawn_background_sync(shared: SharedState) {
    // Usa o runtime do Tauri (tokio) — `tokio::spawn` direto no setup pode dar panic
    // por não haver runtime no contexto síncrono.
    tauri::async_runtime::spawn(async move {
        // 20s: o dreno é barato (não toca o list endpoint do histórico) e o retry de
        // envios pendentes fica ágil. O sync principal dispara no fim do jogo.
        let interval = Duration::from_secs(20);
        loop {
            tokio::time::sleep(interval).await;

            // Only sync when logged in
            let logged_in = {
                let s = shared.lock().await;
                s.token.is_some()
            };

            if !logged_in {
                continue;
            }

            match run_sync(shared.clone()).await {
                Ok(result) => {
                    let sent = result.get("sent").and_then(|v| v.as_u64()).unwrap_or(0);
                    if sent > 0 {
                        eprintln!("[auto-sync] {} partidas novas enviadas", sent);
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if !msg.contains("não logado") {
                        eprintln!("[auto-sync] erro: {}", msg);
                        let mut s = shared.lock().await;
                        s.last_error = Some(msg);
                    }
                }
            }
        }
    });
}

// ── Captura ao vivo do snapshot (LiveClient :2999) ────────────────────────────

/// Loop separado do auto-sync: enquanto há uma partida de ARAM: Mayhem em andamento,
/// guarda o championStats atual do jogador; quando a partida acaba, snapshot E partida
/// entram na outbox (persistente) e o dreno roda em seguida. A captura acontece mesmo
/// deslogado — só o ENVIO exige login.
fn spawn_live_capture(shared: SharedState) {
    tauri::async_runtime::spawn(async move {
        // stats de build cujo PICO (máximo durante a partida) vira recorde "Maior X".
        const PEAK_FIELDS: [&str; 9] = [
            "attackDamage", "abilityPower", "armor", "magicResist", "maxHealth",
            "attackSpeed", "moveSpeed", "lifeSteal", "omnivamp",
        ];

        let poll = Duration::from_secs(3); // poll curto pra pegar picos momentâneos melhor
        let mut last_stats: Option<serde_json::Value> = None;
        let mut peak: std::collections::HashMap<&'static str, f64> = std::collections::HashMap::new();
        let mut game_id: Option<i64> = None;
        let mut was_in_game = false;
        loop {
            tokio::time::sleep(poll).await;

            if liveclient::is_mayhem_in_game().await {
                if let Some(cs) = liveclient::champion_stats().await {
                    // atualiza o pico de cada stat de build
                    for &f in &PEAK_FIELDS {
                        if let Some(v) = cs.get(f).and_then(|x| x.as_f64()) {
                            let e = peak.entry(f).or_insert(f64::MIN);
                            if v > *e { *e = v; }
                        }
                    }
                    last_stats = Some(cs); // o último vira o "build final"
                    was_in_game = true;
                }
                // Pega o gameId EXATO uma vez, do gameflow do LCU, enquanto ainda está em jogo
                // (não depende de a partida estar indexada no histórico depois).
                if game_id.is_none() {
                    if let Ok(creds) = lcu::discover_lcu() {
                        if let Ok(Some(id)) = lcu::get_current_game_id(&creds).await {
                            game_id = Some(id);
                        }
                    }
                }
            } else if was_in_game {
                // Partida acabou → tudo pra OUTBOX (sobrevive a crash/server fora) e
                // dreno imediato. Nada se perde se o envio falhar.
                let resolved_id = match game_id.take() {
                    Some(id) => Some(id),
                    None => resolve_game_id_pos_jogo().await, // raro: LCU caiu durante o jogo
                };
                match (last_stats.take(), resolved_id) {
                    (Some(cs), Some(gid)) => {
                        let mut peak_obj = serde_json::Map::new();
                        for (k, v) in &peak {
                            peak_obj.insert((*k).to_string(), serde_json::json!(v));
                        }
                        {
                            let mut s = shared.lock().await;
                            if !s.outbox.has_snapshot_for(gid) {
                                s.outbox.push(outbox::OutboxItem::Snapshot {
                                    game_id:        gid,
                                    champion_stats: cs,
                                    peak:           serde_json::Value::Object(peak_obj),
                                });
                            }
                            if !s.config.synced_ids.contains(&gid) && !s.outbox.has_match_for(gid) {
                                s.outbox.push(outbox::OutboxItem::PendingMatch { game_id: gid });
                            }
                        }
                        // dreno imediato (best-effort; o ciclo de 20s cobre falhas)
                        if let Err(e) = run_sync(shared.clone()).await {
                            eprintln!("[live-capture] dreno pós-jogo adiado: {e}");
                        }
                    }
                    (_, None) => {
                        eprintln!("[live-capture] gameId não resolvido — snapshot perdido (raro)");
                    }
                    (None, _) => {}
                }
                was_in_game = false;
                game_id = None;
                peak.clear();
            }
        }
    });
}

/// Fallback raro: o LCU caiu durante a partida e não pegamos o gameId do gameflow.
/// Espera o jogo indexar no histórico (janela 0-19, a mesma da UI) e pega o Mayhem
/// mais novo.
async fn resolve_game_id_pos_jogo() -> Option<i64> {
    let creds = lcu::discover_lcu().ok()?;
    let summoner = lcu::get_summoner(&creds).await.ok()?;
    for _ in 0..6 {
        if let Ok(Some(id)) = lcu::get_latest_mayhem_game_id(&creds, &summoner.puuid).await {
            return Some(id);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    None
}

// ── Application entry point ───────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Load persisted config + outbox
    let config = store::load_config();
    let outbox = outbox::Outbox::load(store::outbox_path());

    // Load token from keychain (if any)
    let token = store::load_token().unwrap_or(None);

    let initial_state = AppState {
        config,
        outbox,
        token,
        summoner:        None,
        lcu_connected:   false,
        update_required: None,
        backfilled_lcu:  None,
        last_error:      None,
    };

    let shared: SharedState = Arc::new(Mutex::new(initial_state));
    let shared_for_bg = shared.clone();
    let shared_for_live = shared.clone();
    let shared_for_boot = shared.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(shared)
        .invoke_handler(tauri::generate_handler![
            get_state,
            login,
            logout,
            sync_now,
            open_external,
        ])
        .setup(move |app| {
            spawn_background_sync(shared_for_bg);
            spawn_live_capture(shared_for_live);

            // Dreno de startup: sobe o que ficou parado na outbox (app fechado no
            // meio do envio, server fora no fim do jogo etc).
            tauri::async_runtime::spawn(async move {
                if let Err(e) = run_sync(shared_for_boot).await {
                    let msg = e.to_string();
                    if !msg.contains("não logado") {
                        eprintln!("[startup-sync] {msg}");
                    }
                }
            });

            // Bandeja (system tray): clique esquerdo mostra a janela; menu com Mostrar/Sair.
            let show = MenuItem::with_id(app, "show", "Mostrar", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Sair", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("ADG Companion")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.set_focus(); }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.set_focus(); }
                    }
                })
                .build(app)?;

            Ok(())
        })
        // Fechar (X) esconde pra bandeja em vez de sair.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("erro ao iniciar ADG Companion");
}
