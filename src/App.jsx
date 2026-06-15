import { useState, useEffect, useCallback } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { checkForUpdate, downloadAndRestart } from './updater'

/* ============================================================
   ADG Companion — shell multi-jogo.
   Hoje só o painel de League of Legends é funcional; os demais
   jogos são placeholders "em breve". Adicionar um jogo no futuro =
   nova entrada em GAMES + o painel/adaptador correspondente.
   ============================================================ */

const GAMES = [
  { id: 'lol',      name: 'League of Legends', tag: 'L',  color: '#c89b3c', status: 'active', sub: 'ARAM: Mayhem' },
  { id: 'deadlock', name: 'Deadlock',          tag: 'D',  color: '#c1440e', status: 'soon' },
]

// ── Login (agnóstico de jogo) ─────────────────────────────────────────────────

function LoginScreen({ onLoggedIn }) {
  const [email, setEmail]       = useState('')
  const [password, setPassword] = useState('')
  const [loading, setLoading]   = useState(false)
  const [error, setError]       = useState(null)

  async function handleLogin(e) {
    e.preventDefault()
    setLoading(true)
    setError(null)
    try {
      await invoke('login', { email, password })
      onLoggedIn()
    } catch (e) {
      setError(typeof e === 'string' ? e : (e?.message ?? 'Erro desconhecido'))
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="screen login-screen">
      <div className="brand">
        <img src="/adg.png" alt="ADG" className="brand-logo" />
        <h1>ADG Companion</h1>
        <p className="brand-sub">Seu hub de jogos</p>
      </div>

      <div className="card">
        <form onSubmit={handleLogin}>
          <div className="field-group">
            <label>Email</label>
            <input type="email" value={email} onChange={e => setEmail(e.target.value)}
              placeholder="seu@email.com" className="input" autoComplete="username" required />
          </div>
          <div className="field-group">
            <label>Senha</label>
            <input type="password" value={password} onChange={e => setPassword(e.target.value)}
              placeholder="••••••••" className="input" autoComplete="current-password" required />
          </div>
          {error && <div className="alert alert-error">{error}</div>}
          <button type="submit" className="btn btn-primary" disabled={loading}>
            {loading ? 'Entrando…' : 'Entrar'}
          </button>
        </form>
      </div>
    </div>
  )
}

// ── Painel do League of Legends (a integração funcional de hoje) ──────────────

function LolPanel() {
  const [state, setState]           = useState(null)
  const [syncing, setSyncing]       = useState(false)
  const [syncResult, setSyncResult] = useState(null)

  const refresh = useCallback(async () => {
    try { setState(await invoke('get_state')) } catch { /* transitório */ }
  }, [])

  useEffect(() => {
    refresh()
    const id = setInterval(refresh, 3000)
    return () => clearInterval(id)
  }, [refresh])

  async function handleSync() {
    setSyncing(true)
    setSyncResult(null)
    try {
      const result = await invoke('sync_now')
      setSyncResult({ ok: true, ...result })
      await refresh()
    } catch (e) {
      setSyncResult({ ok: false, error: typeof e === 'string' ? e : String(e) })
    } finally {
      setSyncing(false)
    }
  }

  if (!state) return <div className="loading-msg">Carregando…</div>

  // Backend exigiu atualização (HTTP 426): bloqueia o painel até baixar a versão nova.
  // As partidas/stats pendentes ficam guardados na outbox e sobem após atualizar.
  if (state.updateRequired) {
    return (
      <div className="card">
        <h2 className="update-title">Atualização obrigatória</h2>
        <p className="auto-sync-note">
          Sua versão ({state.version}) ficou pra trás — a mínima agora é{' '}
          {state.updateRequired.minVersion}. Suas partidas pendentes ficam guardadas
          e sobem sozinhas depois de atualizar.
        </p>
        {state.updateRequired.downloadUrl && (
          <button className="btn btn-primary"
            onClick={() => invoke('open_external', { url: state.updateRequired.downloadUrl }).catch(() => {})}>
            Baixar versão nova
          </button>
        )}
      </div>
    )
  }

  const lastSync = state.lastSync ? new Date(state.lastSync).toLocaleString('pt-BR') : '—'

  return (
    <>
      <div className="card">
        <div className="stat-row">
          <span className="stat-label">Cliente LoL</span>
          <span className={`badge ${state.lcuConnected ? 'badge-green' : 'badge-red'}`}>
            {state.lcuConnected ? 'Conectado' : 'Desconectado'}
          </span>
        </div>

        {state.summoner && (
          <div className="stat-row">
            <span className="stat-label">Invocador</span>
            <span className="stat-value">{state.summoner.gameName}#{state.summoner.tagLine}</span>
          </div>
        )}

        <div className="stat-row">
          <span className="stat-label">ADG</span>
          <span className="badge badge-green">Logado</span>
        </div>

        <div className="stat-row">
          <span className="stat-label">Última sync</span>
          <span className="stat-value stat-dim">{lastSync}</span>
        </div>

        {state.pendingCount > 0 && (
          <div className="stat-row">
            <span className="stat-label">Envios pendentes</span>
            <span className="badge badge-red">{state.pendingCount}</span>
          </div>
        )}

        {state.lastError && <div className="alert alert-error">{state.lastError}</div>}

        {syncResult && (
          <div className={`alert ${syncResult.ok ? 'alert-success' : 'alert-error'}`}>
            {syncResult.ok
              ? (syncResult.sent > 0
                  ? `✓ ${syncResult.sent} partida(s) nova(s) sincronizada(s).`
                  : '✓ Tudo em dia — nenhuma partida nova.')
              : `Erro: ${syncResult.error}`}
          </div>
        )}
      </div>

      <button className="btn btn-primary" onClick={handleSync} disabled={syncing}>
        {syncing ? 'Sincronizando…' : 'Sincronizar agora'}
      </button>

      <p className="auto-sync-note">
        Sincroniza sozinho a cada 60s com o LoL aberto — não precisa clicar.
        Os stats finais (AD/AP/HP) são capturados ao vivo durante a partida de Mayhem.
      </p>
    </>
  )
}

// ── Painel "em breve" (jogos ainda não integrados) ────────────────────────────

function SoonPanel({ game }) {
  return (
    <div className="soon">
      <span className="soon__chip" style={{ background: game.color }}>{game.tag}</span>
      <h2 className="soon__title">{game.name}</h2>
      <p className="soon__text">
        Em breve no ADG Companion. Esse jogo entra numa próxima atualização —
        com sync e presença ricos, do mesmo jeito leve do LoL.
      </p>
    </div>
  )
}

// ── Auto-update in-app (updater do Tauri) ─────────────────────────────────────

function UpdateBanner() {
  const [update, setUpdate] = useState(null)   // objeto update disponível
  const [phase, setPhase]   = useState('idle')  // idle | downloading | error
  const [pct, setPct]       = useState(0)

  useEffect(() => {
    let alive = true
    const run = async () => {
      const u = await checkForUpdate()
      if (alive && u) setUpdate(u)
    }
    run()
    const id = setInterval(run, 6 * 60 * 60 * 1000) // a cada 6h
    return () => { alive = false; clearInterval(id) }
  }, [])

  if (!update) return null

  async function handleInstall() {
    setPhase('downloading')
    try {
      await downloadAndRestart(update, (d, t) => setPct(t ? Math.round((d / t) * 100) : 0))
      // relaunch reinicia o app; normalmente não passa daqui
    } catch (e) {
      console.error('[updater] instalação falhou:', e)
      setPhase('error')
    }
  }

  return (
    <div className="update-banner">
      {phase === 'downloading' ? (
        <span>Baixando atualização… {pct}%</span>
      ) : phase === 'error' ? (
        <span>Falha ao atualizar. Tente de novo mais tarde.</span>
      ) : (
        <>
          <span>Atualização v{update.version} disponível.</span>
          <button className="btn btn-primary btn-sm" onClick={handleInstall}>
            Reiniciar e atualizar
          </button>
        </>
      )}
    </div>
  )
}

// ── Shell principal (seletor de jogos + painel) ───────────────────────────────

function MainShell({ onLogout }) {
  const [selected, setSelected] = useState(null) // null = home (lista de jogos)
  const [version, setVersion]   = useState('')
  const game = selected ? GAMES.find(g => g.id === selected) : null

  useEffect(() => {
    invoke('get_state').then(s => setVersion(s.version)).catch(() => {})
  }, [])

  async function handleLogout() {
    try { await invoke('logout') } catch { /* ignore */ }
    onLogout()
  }

  return (
    <div className="shell">
      <UpdateBanner />
      <header className="shell__head">
        {game
          ? <button className="shell__back" onClick={() => setSelected(null)}>‹ Jogos</button>
          : <div className="brand compact">
              <img src="/adg.png" alt="ADG" className="brand-logo" />
              <h1>ADG Companion</h1>
            </div>}
        <button className="btn-ghost btn-sm" onClick={handleLogout}>Sair</button>
      </header>

      <main className="shell__body">
        {!game ? (
          <div className="game-list">
            {GAMES.map(g => (
              <button key={g.id}
                className={'game-card' + (g.status === 'soon' ? ' game-card--soon' : '')}
                onClick={() => setSelected(g.id)}>
                <span className="game-card__chip" style={{ background: g.color }}>{g.tag}</span>
                <span className="game-card__meta">
                  <span className="game-card__name">{g.name}</span>
                  <span className={'game-card__sub' + (g.status === 'soon' ? ' game-card__sub--soon' : '')}>
                    {g.status === 'soon' ? 'Em breve' : (g.sub ?? 'Ativo')}
                  </span>
                </span>
                <span className="game-card__arrow">›</span>
              </button>
            ))}
          </div>
        ) : (
          <div className="game-detail">
            <div className="game-detail__head">
              <span className="game-card__chip" style={{ background: game.color }}>{game.tag}</span>
              <div className="game-detail__id">
                <div className="game-detail__name">{game.name}</div>
                {game.sub && <div className="game-detail__sub">{game.sub}</div>}
              </div>
            </div>
            {game.status === 'active' ? <LolPanel /> : <SoonPanel game={game} />}
          </div>
        )}
      </main>

      {version && <footer className="shell__foot">v{version}</footer>}
    </div>
  )
}

// ── Root ──────────────────────────────────────────────────────────────────────

export default function App() {
  const [loggedIn, setLoggedIn] = useState(null) // null = carregando

  useEffect(() => {
    invoke('get_state')
      .then(s => setLoggedIn(s.loggedIn))
      .catch(() => setLoggedIn(false))
  }, [])

  if (loggedIn === null) {
    return <div className="screen"><div className="loading-msg">Iniciando…</div></div>
  }
  if (!loggedIn) {
    return <LoginScreen onLoggedIn={() => setLoggedIn(true)} />
  }
  return <MainShell onLogout={() => setLoggedIn(false)} />
}
