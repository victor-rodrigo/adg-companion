# Desenvolvimento — ADG Companion

App de desktop (Tauri v2 + React) que lê o histórico de **ARAM: Mayhem** (fila 2400) do
cliente local do League of Legends e envia pro backend do ADG — já que a Riot bloqueia
esses dados na API pública.

```
.
  src/            React (Vite)
  src-tauri/      Rust (LCU, envio ao ADG, bandeja, keychain)
```

## Pré-requisitos (Windows)

| O quê | Onde | Observação |
|------|------|-----------|
| **Rust + Cargo** | https://rustup.rs | toolchain MSVC (`rustup default stable-x86_64-pc-windows-msvc`) |
| **Build Tools C++** | Visual Studio Build Tools 2022 → workload "Desenvolvimento para desktop com C++" | dá o `link.exe` que o Rust usa |
| **Node.js LTS** | https://nodejs.org | Vite + Tauri CLI |
| **WebView2** | já vem no Win 10/11 | se faltar: site da Microsoft |

Conferir: `cargo --version && node --version`

## Instalar deps e rodar em dev

```powershell
npm install
npm run tauri dev
```
- Suba o backend antes (local: porta 3100).
- Abra o cliente do LoL (não precisa estar em jogo).
- No app: entre com sua conta **ADG** (e-mail/senha) → "Sincronizar agora".
- A URL da API **não é digitada** — vem assada no build (default `http://localhost:3100`; veja abaixo).
- Fechar a janela **esconde pra bandeja**; o ícone reabre / tem "Sair".

## Build de produção (.exe)

A URL da API é **assada no binário** via a env `ADG_API_URL` (sem segredo; é só a URL pública do site).
Se não setar, usa `http://localhost:3100`.

```powershell
$env:ADG_API_URL = "https://adg-api.duckdns.org"
npm run tauri build
```

Saída (Windows):
- Instalador NSIS: `src-tauri/target/release/bundle/nsis/ADG Companion_<versão>_x64-setup.exe`
- Instalador MSI:  `src-tauri/target/release/bundle/msi/ADG Companion_<versão>_x64_en-US.msi`

> Bump de versão: `node bump-version.mjs patch|minor|major` (mantém `src-tauri/tauri.conf.json`,
> `src-tauri/Cargo.toml` e `package.json` em sincronia).
> Ícone: `npm run tauri icon adg-icon.png` (já commitado; rode de novo se trocar a arte).

## Publicar uma release (GitHub Actions)

Workflow em `.github/workflows/release.yml`. Em **Actions → Release → Run workflow**,
escolha o **bump** (`patch`/`minor`/`major`) e rode. Ele:
1. Sobe a versão nos 3 arquivos (via `bump-version.mjs`).
2. Builda o instalador no Windows.
3. Commita o bump neste repo.
4. Publica a release **neste repo** (tag `vX.Y.Z`), com o instalador de nome fixo
   `ADG-Companion-Setup.exe` (+ MSI), pelo `GITHUB_TOKEN` padrão.

Pré-requisito (uma vez):
> Variável de repositório **`ADG_API_URL`** (Settings → Secrets and variables → Actions →
> Variables) = `https://adg-api.duckdns.org`. Sem ela, o build usa `http://localhost:3100`.

## Forçar atualização (versão mínima)

O backend guarda a config em `companion_version` (versão mínima, última versão e URL de
download), editável no site em **Admin → Companion**. Depois de publicar `vX.Y.Z`: suba a
*versão mínima* pra `X.Y.Z`. Apps abaixo da mínima recebem **HTTP 426** no próximo sync e
mostram o aviso de atualização (com link); as partidas pendentes ficam na outbox e sobem
depois de atualizar.

## Notas técnicas

- **LCU**: descobre o `LeagueClientUx` via `sysinfo` (lê `--app-port`/`--remoting-auth-token`) e
  fala HTTPS local ignorando o cert self-signed (só pro LCU; chamadas ao ADG usam TLS normal).
- **Nunca paginar o histórico do LCU**: o list endpoint tem cache compartilhada com a UI do
  cliente; janelas custom faziam o histórico mostrar 1 partida com o app aberto. O sync é por
  **fim de jogo**: o `gameId` vem do gameflow e os dados de `/lol-match-history/v1/games/{gameId}`
  (cache separada) trazem os stats dos 10 players — 1 pessoa sincroniza o time. Único toque no
  list endpoint: 1 chamada `0-19` por sessão do LCU, pra backfill.
- **Outbox persistente** (`%APPDATA%/adg-companion/outbox.json`): grava antes de enviar e só sai
  após 2xx. 5xx/rede retenta pra sempre; 4xx descarta após 5 tentativas; 426 retém até atualizar.
- Snapshot AD/AP/HP é **só do próprio jogador** (LiveClient não expõe `championStats` dos outros 9).
- `current-summoner` do LCU é **UUID de 36 chars** ≠ **PUUID de 78 chars** → validação por **Riot ID**.
- Augments do Mayhem: resolvidos no backend via `cherry-augments.json` (Community Dragon).
