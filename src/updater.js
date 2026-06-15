import { check } from '@tauri-apps/plugin-updater'
import { relaunch } from '@tauri-apps/plugin-process'

// Checa se há update. Retorna o objeto `update` (com .version) ou null.
// Nunca lança: offline/erro vira null (não atrapalha o app).
export async function checkForUpdate() {
  try {
    const update = await check()
    return update ?? null
  } catch (e) {
    console.warn('[updater] check falhou:', e)
    return null
  }
}

// Baixa e instala, chamando onProgress(downloaded, total) durante o download.
// Ao terminar, reinicia o app. Lança se o download/instalação falhar.
export async function downloadAndRestart(update, onProgress) {
  let downloaded = 0
  let total = 0
  await update.downloadAndInstall((event) => {
    if (event.event === 'Started') total = event.data.contentLength ?? 0
    else if (event.event === 'Progress') {
      downloaded += event.data.chunkLength ?? 0
      onProgress?.(downloaded, total)
    }
  })
  await relaunch()
}
