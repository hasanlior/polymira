// Builds POLYMIRA Orbis (Rust, emu/orbis) and copies orbis.exe to resources/bin, where the app
// and the installer pick it up. Skips with a warning when Rust is not installed.
const { execFileSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const root = path.resolve(__dirname, '..')
const crate = path.join(root, 'emu', 'orbis')
const out = path.join(root, 'resources', 'bin')
const exe = path.join(crate, 'target', 'release', process.platform === 'win32' ? 'orbis.exe' : 'orbis')

try {
  execFileSync('cargo', ['build', '--release', '--quiet'], { cwd: crate, stdio: 'inherit' })
} catch (err) {
  console.warn('[orbis] not built (is Rust installed? https://rustup.rs):', err.message)
  process.exit(0)
}
fs.mkdirSync(out, { recursive: true })
fs.copyFileSync(exe, path.join(out, path.basename(exe)))
console.log('[orbis] built →', path.relative(root, path.join(out, path.basename(exe))))
