// Lists interface texts written in Turkish that have no English translation in
// src/renderer/src/i18n.ts. Exits with code 1 when something is missing (usable in CI).
//   npm run i18n:check
const fs = require('fs')
const path = require('path')

const root = path.resolve(__dirname, '..')
const i18n = fs.readFileSync(path.join(root, 'src/renderer/src/i18n.ts'), 'utf8')

// Dictionary keys: 'quoted': or Bare: at the start of a line.
const keys = new Set()
for (const m of i18n.matchAll(/^\s*(?:'((?:[^'\\]|\\.)*)'|"((?:[^"\\]|\\.)*)"|([A-Za-zÇĞİÖŞÜçğıöşü]+))\s*:/gm)) {
  keys.add((m[1] ?? m[2] ?? m[3]).replace(/\\'/g, "'").replace(/\\n/g, '\n').replace(/\s+/g, ' '))
}
const patterns = [...i18n.matchAll(/^\s*\[\/(.+)\/,\s*['"]/gm)].map((m) => new RegExp(m[1]))

const TR = /[çğıöşüÇĞİÖŞÜ]/
const IGNORE = [/^[a-zçğıöşü]+$/i.source.length && /^(qwertyuıopğü|asdfghjklşi|Türkçe)$/, /\\\\|https?:|\.(exe|dll|ini|json|php)\b/]

function files(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = path.join(dir, e.name)
    if (e.isDirectory()) return files(p)
    return /\.(ts|tsx)$/.test(e.name) && !e.name.endsWith('i18n.ts') ? [p] : []
  })
}

function covered(text) {
  const t = text.trim().replace(/\s+/g, ' ')
  if (keys.has(t) || patterns.some((re) => re.test(t))) return true
  if (t.includes(' · ')) return t.split(' · ').every((p) => !TR.test(p) || covered(p))
  return false
}

const missing = new Map()
for (const f of [...files(path.join(root, 'src/renderer/src')), ...files(path.join(root, 'src/main')), ...files(path.join(root, 'src/shared'))]) {
  let src = fs.readFileSync(f, 'utf8')
  src = src.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '')
  const found = []
  for (const m of src.matchAll(/'((?:[^'\\\n]|\\.)*)'/g)) found.push(m[1])
  for (const m of src.matchAll(/>([^<>{}]+)</g)) found.push(m[1])
  // Template strings without placeholders, and the fixed parts of ones with placeholders.
  for (const m of src.matchAll(/`([^`]*)`/g)) {
    if (!m[1].includes('${')) found.push(m[1])
  }
  for (const raw of found) {
    const t = raw.replace(/\\'/g, "'").replace(/\\n/g, '\n').trim().replace(/\s+/g, ' ')
    if (/=>|\)\s*(const|if|return)\b|useState|hints\.push|invoke\(/.test(t)) continue // code, not text
    if (t.length < 2 || !TR.test(t) || IGNORE.some((re) => re.test(t))) continue
    if (/^[\w.$-]+$/.test(t) && !/\s/.test(t) && t.length < 3) continue
    if (!covered(t)) missing.set(t, path.relative(root, f).replace(/\\/g, '/'))
  }
}

if (!missing.size) {
  console.log('i18n: every Turkish interface text has an English translation.')
  process.exit(0)
}
console.log(`i18n: ${missing.size} text(s) without an English translation:\n`)
for (const [t, f] of [...missing].sort((a, b) => a[1].localeCompare(b[1]))) console.log(`  ${f}: ${JSON.stringify(t)}`)
process.exit(1)
