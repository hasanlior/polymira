// Puts a desktop shortcut to the freshly built app after every build (Windows only).
//   node scripts/shortcut.cjs        → shortcut runs the development build (out/) with Electron
//   node scripts/shortcut.cjs dist   → shortcut runs the packaged app in release/win-unpacked
// Set NO_SHORTCUT=1 (or CI=1) to skip.
const { execFileSync } = require('child_process')
const fs = require('fs')
const path = require('path')

if (process.platform !== 'win32' || process.env.CI || process.env.NO_SHORTCUT) process.exit(0)

const root = path.resolve(__dirname, '..')
const pkg = require(path.join(root, 'package.json'))
const name = pkg.build?.productName || pkg.productName || pkg.name
const mode = process.argv[2] === 'dist' ? 'dist' : 'dev'

const target =
  mode === 'dist'
    ? path.join(root, pkg.build?.directories?.output || 'release', 'win-unpacked', `${name}.exe`)
    : path.join(root, 'node_modules', 'electron', 'dist', 'electron.exe')
if (!fs.existsSync(target)) {
  console.warn(`[shortcut] ${target} not found, skipped.`)
  process.exit(0)
}
const args = mode === 'dist' ? '' : `"${root}"`
const icon = path.join(root, 'build', 'icon.ico')

// Values reach PowerShell through environment variables, so paths with spaces or quotes are safe.
const script = [
  '$desk = [Environment]::GetFolderPath("Desktop")',
  '$lnk = Join-Path $desk ($env:SC_NAME + ".lnk")',
  '$s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)',
  '$s.TargetPath = $env:SC_TARGET',
  '$s.Arguments = $env:SC_ARGS',
  '$s.WorkingDirectory = $env:SC_CWD',
  '$s.IconLocation = $env:SC_ICON',
  '$s.Description = $env:SC_DESC',
  '$s.Save()',
  'Write-Output $lnk'
].join('; ')

try {
  const out = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
    env: {
      ...process.env,
      SC_NAME: name,
      SC_TARGET: target,
      SC_ARGS: args,
      SC_CWD: mode === 'dist' ? path.dirname(target) : root,
      SC_ICON: fs.existsSync(icon) ? icon : target,
      SC_DESC: `${name} (${mode === 'dist' ? 'kurulum paketi' : 'geliştirme sürümü'})`
    },
    encoding: 'utf8'
  })
  console.log(`[shortcut] ${out.trim()}`)
} catch (err) {
  console.warn('[shortcut] could not create the desktop shortcut:', err.message)
}
