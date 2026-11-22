import fs from 'fs'

/**
 * Minimal PARAM.SFO reader (PS3 / PS4 / PS Vita / PSP).
 * Returns key → value (strings and integers). Returns null if the file is not a valid SFO.
 */
export function readSfo(file: string): Record<string, string | number> | null {
  let buf: Buffer
  try {
    buf = fs.readFileSync(file)
  } catch {
    return null
  }
  if (buf.length < 20 || buf.readUInt32BE(0) !== 0x00505346) return null
  const keyTable = buf.readUInt32LE(8)
  const dataTable = buf.readUInt32LE(12)
  const count = buf.readUInt32LE(16)
  const out: Record<string, string | number> = {}
  for (let i = 0; i < count; i++) {
    const e = 20 + i * 16
    if (e + 16 > buf.length) break
    const keyOff = buf.readUInt16LE(e)
    const fmt = buf.readUInt16LE(e + 2)
    const len = buf.readUInt32LE(e + 4)
    const dataOff = buf.readUInt32LE(e + 12)
    const keyStart = keyTable + keyOff
    const keyEnd = buf.indexOf(0, keyStart)
    if (keyEnd < 0) break
    const key = buf.toString('utf8', keyStart, keyEnd)
    const start = dataTable + dataOff
    if (fmt === 0x0404) {
      out[key] = buf.readUInt32LE(start)
    } else {
      out[key] = buf.toString('utf8', start, start + len).replace(/\0+$/, '')
    }
  }
  return out
}
