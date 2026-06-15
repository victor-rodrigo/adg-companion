/**
 * bump-version.mjs — sobe a versão do companion nos 3 arquivos que precisam ficar
 * em sincronia: src-tauri/tauri.conf.json, src-tauri/Cargo.toml e package.json.
 *
 * Uso (rodar de dentro de companion/app):
 *   node bump-version.mjs patch        # 0.1.0 -> 0.1.1
 *   node bump-version.mjs minor        # 0.1.0 -> 0.2.0
 *   node bump-version.mjs major        # 0.1.0 -> 1.0.0
 *   node bump-version.mjs 0.4.2        # versão exata
 *
 * Em CI (GitHub Actions) também escreve old=/new= no $GITHUB_OUTPUT.
 */
import { readFileSync, writeFileSync, appendFileSync } from 'fs'

const arg = process.argv[2]
if (!arg) { console.error('Uso: node bump-version.mjs <patch|minor|major|X.Y.Z>'); process.exit(1) }

const TAURI = 'src-tauri/tauri.conf.json'
const PKG   = 'package.json'
const CARGO = 'src-tauri/Cargo.toml'

const tauri = JSON.parse(readFileSync(TAURI, 'utf8'))
const cur = tauri.version
if (!/^\d+\.\d+\.\d+$/.test(cur)) { console.error(`Versão atual inválida em ${TAURI}: ${cur}`); process.exit(1) }

let next
if (/^\d+\.\d+\.\d+$/.test(arg)) {
  next = arg
} else {
  const [a, b, c] = cur.split('.').map(Number)
  if (arg === 'major') next = `${a + 1}.0.0`
  else if (arg === 'minor') next = `${a}.${b + 1}.0`
  else if (arg === 'patch') next = `${a}.${b}.${c + 1}`
  else { console.error(`Argumento inválido: ${arg} (use patch|minor|major|X.Y.Z)`); process.exit(1) }
}

if (next === cur) { console.error(`Nada a fazer: ${cur} == ${next}`); process.exit(1) }

// tauri.conf.json
tauri.version = next
writeFileSync(TAURI, JSON.stringify(tauri, null, 2) + '\n')

// package.json
const pkg = JSON.parse(readFileSync(PKG, 'utf8'))
pkg.version = next
writeFileSync(PKG, JSON.stringify(pkg, null, 2) + '\n')

// Cargo.toml — só a primeira linha `version = "..."` (a do [package])
const cargo = readFileSync(CARGO, 'utf8')
const bumped = cargo.replace(/^version\s*=\s*"[^"]+"/m, `version = "${next}"`)
if (bumped === cargo) { console.error(`Não achei a linha de versão em ${CARGO}`); process.exit(1) }
writeFileSync(CARGO, bumped)

console.log(`Bump ${cur} -> ${next}`)
if (process.env.GITHUB_OUTPUT) {
  appendFileSync(process.env.GITHUB_OUTPUT, `old=${cur}\nnew=${next}\n`)
}
