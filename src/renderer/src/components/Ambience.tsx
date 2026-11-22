import { useEffect, useRef, useState } from 'react'
import type { Game } from '@shared/types'
import { MEDIA_SCHEME } from '@shared/ipc'

export function backgroundUrl(g: Game): string {
  return `${MEDIA_SCHEME}://bg/${g.id}.jpg?v=${g.bgRev ?? 0}`
}

export function musicUrl(g: Game): string {
  return `${MEDIA_SCHEME}://music/${g.id}?p=${encodeURIComponent(g.musicPath ?? '')}`
}

/**
 * Full-screen artwork of the focused game, cross-faded over the wave background
 * (like PIC1 on the PS3). Two stacked layers so the old image fades out while the new one fades in.
 */
export function Backdrop({ game }: { game: Game | null }) {
  const url = game?.hasBackground ? backgroundUrl(game) : null
  // Last two loaded images; only the one matching `shown` is visible, so changes cross-fade.
  const [layers, setLayers] = useState<string[]>([])
  const [shown, setShown] = useState<string | null>(null)

  useEffect(() => {
    if (!url) {
      setShown(null)
      return
    }
    let alive = true
    // Wait briefly so fast scrolling does not flash every image, then preload and fade in.
    const t = setTimeout(() => {
      const img = new Image()
      img.onload = () => {
        if (!alive) return
        setLayers((ls) => [...ls.filter((l) => l !== url).slice(-1), url])
        requestAnimationFrame(() => alive && setShown(url))
      }
      img.src = url
    }, 250)
    return () => {
      alive = false
      clearTimeout(t)
    }
  }, [url])

  return (
    <div className="backdrop">
      {layers.map((l) => (
        <div key={l} className={`backdrop-img ${l === shown ? 'show' : ''}`} style={{ backgroundImage: `url("${l}")` }} />
      ))}
      <div className={`backdrop-shade ${shown ? 'show' : ''}`} />
    </div>
  )
}

/** Plays the focused game's theme music with fade in / fade out, like the PS3 XMB. */
export function ThemeMusic({ game, enabled, volume }: { game: Game | null; enabled: boolean; volume: number }) {
  const audioRef = useRef<HTMLAudioElement | null>(null)
  const fadeRef = useRef<number | null>(null)
  const src = enabled && game?.musicPath ? musicUrl(game) : null

  const fadeTo = (target: number, ms: number, done?: () => void) => {
    const a = audioRef.current
    if (!a) return done?.()
    if (fadeRef.current) cancelAnimationFrame(fadeRef.current)
    const from = a.volume
    const start = performance.now()
    const step = (t: number) => {
      const k = Math.min(1, (t - start) / ms)
      a.volume = Math.max(0, Math.min(1, from + (target - from) * k))
      if (k < 1) fadeRef.current = requestAnimationFrame(step)
      else {
        fadeRef.current = null
        done?.()
      }
    }
    fadeRef.current = requestAnimationFrame(step)
  }

  useEffect(() => {
    const a = (audioRef.current ??= new Audio())
    a.loop = true
    if (!src) {
      fadeTo(0, 500, () => a.pause())
      return
    }
    // Wait a moment so scrolling quickly through games does not start every track.
    const t = setTimeout(() => {
      fadeTo(0, 350, () => {
        a.src = src
        a.currentTime = 0
        a.volume = 0
        void a.play().then(
          () => fadeTo(volume, 1500),
          () => {}
        )
      })
    }, 700)
    return () => clearTimeout(t)
  }, [src])

  useEffect(() => {
    const a = audioRef.current
    if (a && !a.paused) fadeTo(volume, 300)
  }, [volume])

  // Stop when the window is hidden (a game is running) and resume when we come back.
  useEffect(() => {
    const onVis = () => {
      const a = audioRef.current
      if (!a || !a.src) return
      if (document.hidden) a.pause()
      else if (src) void a.play().catch(() => {})
    }
    document.addEventListener('visibilitychange', onVis)
    return () => document.removeEventListener('visibilitychange', onVis)
  }, [src])

  useEffect(() => () => audioRef.current?.pause(), [])
  return null
}

/**
 * The focused game's official trailer, streamed from Steam and faded in over the artwork
 * after the game has been focused for a moment (like the PS4/PS5 home screen).
 */
export function Trailer({ game, enabled, volume, muted }: { game: Game | null; enabled: boolean; volume: number; muted: boolean }) {
  const videoRef = useRef<HTMLVideoElement>(null)
  const [visible, setVisible] = useState(false)
  const url = enabled && game?.trailerUrl ? game.trailerUrl : null

  useEffect(() => {
    const v = videoRef.current
    setVisible(false)
    if (!url || !v) return
    let hls: import('hls.js').default | null = null
    let alive = true
    let fade: number | null = null

    const fadeVolume = (target: number, ms: number) => {
      if (fade) cancelAnimationFrame(fade)
      const from = v.volume
      const start = performance.now()
      const step = (t: number) => {
        const k = Math.min(1, (t - start) / ms)
        v.volume = Math.max(0, Math.min(1, from + (target - from) * k))
        if (k < 1) fade = requestAnimationFrame(step)
      }
      fade = requestAnimationFrame(step)
    }

    // Only start after the user settles on a game.
    const timer = setTimeout(async () => {
      const { default: Hls } = await import('hls.js')
      if (!alive || !Hls.isSupported()) return
      hls = new Hls({ capLevelToPlayerSize: true, maxBufferLength: 20, startLevel: -1 })
      hls.loadSource(url)
      hls.attachMedia(v)
      v.volume = 0
      v.muted = muted
      v.onplaying = () => {
        if (!alive) return
        setVisible(true)
        if (!muted) fadeVolume(volume, 2000)
      }
      v.onended = () => setVisible(false)
      void v.play().catch(() => {})
    }, 1800)

    const onVis = () => {
      if (document.hidden) v.pause()
      else if (hls) void v.play().catch(() => {})
    }
    document.addEventListener('visibilitychange', onVis)

    return () => {
      alive = false
      clearTimeout(timer)
      document.removeEventListener('visibilitychange', onVis)
      if (fade) cancelAnimationFrame(fade)
      setVisible(false)
      const h = hls
      // Let the fade-out finish before tearing the stream down.
      const startVol = v.volume
      const t0 = performance.now()
      const out = (t: number) => {
        const k = Math.min(1, (t - t0) / 400)
        v.volume = startVol * (1 - k)
        if (k < 1) requestAnimationFrame(out)
        else {
          v.pause()
          h?.destroy()
          v.removeAttribute('src')
          v.load()
        }
      }
      requestAnimationFrame(out)
    }
  }, [url, muted])

  useEffect(() => {
    const v = videoRef.current
    if (v && visible && !muted) v.volume = volume
  }, [volume, visible, muted])

  return <video ref={videoRef} className={`backdrop-video ${visible ? 'show' : ''}`} playsInline />
}
