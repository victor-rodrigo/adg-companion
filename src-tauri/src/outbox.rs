/// outbox.rs — fila persistente de envios pro ADG (matches e snapshots).
///
/// Todo dado capturado vai pra cá ANTES de tentar enviar e só sai após 2xx.
/// Sobrevive a crash/fechamento do app e a server fora do ar. Semântica de retry:
///   • Transient (5xx/rede/LCU indisponível): retenta pra sempre.
///   • Permanent (4xx): descarta após MAX_PERMANENT_FAILURES.
///   • UpdateRequired (426): NUNCA descarta — fica retido até o app ser atualizado.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub const MAX_PERMANENT_FAILURES: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboxItem {
    /// gameId visto no fim de jogo/backfill; vira MatchPayload quando o LCU responder o detail.
    PendingMatch { game_id: i64 },
    /// Payload completo da partida (10 players ricos), pronto pra POST /matches.
    MatchPayload { game_id: i64, payload: Value },
    /// Snapshot AD/AP/HP (final + pico) do dono, pronto pra POST /snapshot.
    Snapshot {
        game_id: i64,
        champion_stats: Value,
        peak: Value,
    },
}

impl OutboxItem {
    pub fn game_id(&self) -> i64 {
        match self {
            OutboxItem::PendingMatch { game_id }
            | OutboxItem::MatchPayload { game_id, .. }
            | OutboxItem::Snapshot { game_id, .. } => *game_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub id: u64,
    pub item: OutboxItem,
    #[serde(default)]
    pub permanent_failures: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OutboxFile {
    next_id: u64,
    entries: Vec<OutboxEntry>,
}

#[derive(Debug)]
pub struct Outbox {
    file: OutboxFile,
    path: PathBuf,
}

impl Outbox {
    /// Carrega do disco; arquivo ausente/corrompido vira fila vazia (nunca derruba o app).
    pub fn load(path: PathBuf) -> Self {
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self { file, path }
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).context("criar diretório da outbox")?;
        }
        let raw = serde_json::to_string_pretty(&self.file).context("serializar outbox")?;
        std::fs::write(&self.path, raw).context("salvar outbox")
    }

    fn save_logged(&self) {
        if let Err(e) = self.save() {
            eprintln!("[outbox] falha ao salvar: {e}");
        }
    }

    pub fn entries(&self) -> Vec<OutboxEntry> {
        self.file.entries.clone()
    }

    pub fn len(&self) -> usize {
        self.file.entries.len()
    }

    pub fn push(&mut self, item: OutboxItem) -> u64 {
        let id = self.file.next_id;
        self.file.next_id += 1;
        self.file.entries.push(OutboxEntry {
            id,
            item,
            permanent_failures: 0,
            last_error: None,
        });
        self.save_logged();
        id
    }

    /// True se já existe item de match (pendente ou pronto) pra esse jogo.
    pub fn has_match_for(&self, game_id: i64) -> bool {
        self.file.entries.iter().any(|e| matches!(
            &e.item,
            OutboxItem::PendingMatch { game_id: g } | OutboxItem::MatchPayload { game_id: g, .. } if *g == game_id
        ))
    }

    pub fn has_snapshot_for(&self, game_id: i64) -> bool {
        self.file.entries.iter().any(|e| {
            matches!(
                &e.item, OutboxItem::Snapshot { game_id: g, .. } if *g == game_id
            )
        })
    }

    /// PendingMatch resolvido → vira MatchPayload (mantém id e contadores).
    pub fn replace_item(&mut self, id: u64, item: OutboxItem) {
        if let Some(e) = self.file.entries.iter_mut().find(|e| e.id == id) {
            e.item = item;
        }
        self.save_logged();
    }

    pub fn remove(&mut self, id: u64) {
        self.file.entries.retain(|e| e.id != id);
        self.save_logged();
    }

    /// 4xx: conta a falha e descarta após MAX_PERMANENT_FAILURES.
    pub fn record_permanent_failure(&mut self, id: u64, err: &str) {
        if let Some(e) = self.file.entries.iter_mut().find(|e| e.id == id) {
            e.permanent_failures += 1;
            e.last_error = Some(err.to_string());
            if e.permanent_failures >= MAX_PERMANENT_FAILURES {
                eprintln!(
                    "[outbox] descartando item {id} após {} falhas 4xx: {err}",
                    e.permanent_failures
                );
                self.file.entries.retain(|e| e.id != id);
            }
        }
        self.save_logged();
    }

    /// 5xx/rede: só registra; retenta no próximo dreno.
    pub fn record_transient_failure(&mut self, id: u64, err: &str) {
        if let Some(e) = self.file.entries.iter_mut().find(|e| e.id == id) {
            e.last_error = Some(err.to_string());
        }
        self.save_logged();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_outbox(name: &str) -> Outbox {
        let path = std::env::temp_dir().join(format!(
            "adg-outbox-test-{name}-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        Outbox::load(path)
    }

    #[test]
    fn push_persiste_e_recarrega() {
        let mut ob = temp_outbox("reload");
        ob.push(OutboxItem::PendingMatch { game_id: 42 });
        let reloaded = Outbox::load(ob.path.clone());
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded.has_match_for(42));
        assert_eq!(reloaded.entries()[0].item.game_id(), 42);
        let _ = std::fs::remove_file(&ob.path);
    }

    #[test]
    fn replace_preserva_id_e_remove_funciona() {
        let mut ob = temp_outbox("replace");
        let id = ob.push(OutboxItem::PendingMatch { game_id: 7 });
        ob.replace_item(
            id,
            OutboxItem::MatchPayload {
                game_id: 7,
                payload: serde_json::json!({"x":1}),
            },
        );
        assert!(matches!(
            ob.entries()[0].item,
            OutboxItem::MatchPayload { game_id: 7, .. }
        ));
        assert_eq!(ob.entries()[0].id, id);
        ob.remove(id);
        assert_eq!(ob.len(), 0);
        let _ = std::fs::remove_file(&ob.path);
    }

    #[test]
    fn falha_permanente_descarta_apos_limite() {
        let mut ob = temp_outbox("perm");
        let id = ob.push(OutboxItem::Snapshot {
            game_id: 1,
            champion_stats: serde_json::json!({}),
            peak: serde_json::json!({}),
        });
        for _ in 0..MAX_PERMANENT_FAILURES {
            ob.record_permanent_failure(id, "400: payload inválido");
        }
        assert_eq!(ob.len(), 0);
        let _ = std::fs::remove_file(&ob.path);
    }

    #[test]
    fn falha_transiente_nunca_descarta() {
        let mut ob = temp_outbox("trans");
        let id = ob.push(OutboxItem::PendingMatch { game_id: 9 });
        for _ in 0..20 {
            ob.record_transient_failure(id, "rede fora");
        }
        assert_eq!(ob.len(), 1);
        assert_eq!(ob.entries()[0].last_error.as_deref(), Some("rede fora"));
        let _ = std::fs::remove_file(&ob.path);
    }

    #[test]
    fn dedupe_por_game_id() {
        let mut ob = temp_outbox("dedupe");
        ob.push(OutboxItem::Snapshot {
            game_id: 5,
            champion_stats: serde_json::json!({}),
            peak: serde_json::json!({}),
        });
        assert!(ob.has_snapshot_for(5));
        assert!(!ob.has_snapshot_for(6));
        assert!(!ob.has_match_for(5));
        let _ = std::fs::remove_file(&ob.path);
    }
}
