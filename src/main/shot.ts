/**
 * Development-only screenshot runner, for checking the UI without opening a window.
 *
 *   POLYMIRA_SHOT=<folder>  POLYMIRA_SHOT_SCRIPT=<steps.json>  npm run dev  (or electron .)
 *
 * The app uses `<folder>/userdata` instead of the real data folder, the window is never shown,
 * and the steps run in order once the page has loaded, then the app quits:
 *   { "wait": ms } · { "key": "Enter" } (Electron key code) · { "eval": "js" } · { "shot": "name" }
 * Screenshots are written to `<folder>/<name>.png`. Ignored in packaged builds.
 */
import { app, type BrowserWindow } from 'electron'
import fs from 'fs'
import path from 'path'

type Step = { wait?: number; key?: string; eval?: string; shot?: string }

export const shotDir: string | null = !app.isPackaged && process.env.POLYMIRA_SHOT ? process.env.POLYMIRA_SHOT : null

/** Call before anything reads the data folder. */
export function prepareShotMode(): void {
  if (!shotDir) return
  fs.mkdirSync(shotDir, { recursive: true })
  app.setPath('userData', path.join(shotDir, 'userdata'))
}

export function runShotScript(win: BrowserWindow): void {
  if (!shotDir) return
  const file = process.env.POLYMIRA_SHOT_SCRIPT
  const steps: Step[] = file && fs.existsSync(file) ? (JSON.parse(fs.readFileSync(file, 'utf8')) as Step[]) : [{ wait: 3000 }, { shot: 'home' }]
  const wc = win.webContents
  wc.setBackgroundThrottling(false)
  wc.once('did-finish-load', async () => {
    for (const s of steps) {
      try {
        if (s.wait) await new Promise((r) => setTimeout(r, s.wait))
        if (s.key) {
          wc.sendInputEvent({ type: 'keyDown', keyCode: s.key })
          await new Promise((r) => setTimeout(r, 60))
          wc.sendInputEvent({ type: 'keyUp', keyCode: s.key })
          await new Promise((r) => setTimeout(r, 250))
        }
        if (s.eval) await wc.executeJavaScript(s.eval, true)
        if (s.shot) {
          const img = await wc.capturePage()
          fs.writeFileSync(path.join(shotDir!, `${s.shot}.png`), img.toPNG())
          console.log(`[shot] ${s.shot}.png`)
        }
      } catch (err) {
        console.log('[shot] step failed:', (err as Error).message)
      }
    }
    app.exit(0)
  })
}
