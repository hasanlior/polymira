// Theme toggle (light default, persisted in localStorage), GitHub stars, latest release link, docs sidebar toggle.
;(() => {
  const REPO = 'hasanlior/polymira'
  const STORAGE_KEY = 'polymira-theme'
  const root = document.documentElement

  function applyTheme(theme) {
    const t = theme === 'dark' ? 'dark' : 'light'
    root.setAttribute('data-theme', t)
    const metaTheme = document.querySelector('meta[name="theme-color"]')
    if (metaTheme) metaTheme.setAttribute('content', t === 'dark' ? '#090b10' : '#ffffff')
    document.querySelectorAll('.theme-toggle').forEach((btn) => {
      const next = t === 'dark' ? 'light' : 'dark'
      btn.setAttribute('aria-label', `Switch to ${next} theme`)
      btn.setAttribute('title', `Switch to ${next} theme`)
      btn.setAttribute('aria-pressed', String(t === 'dark'))
    })
  }

  const initial = root.getAttribute('data-theme') === 'dark' ? 'dark' : 'light'
  applyTheme(initial)

  document.querySelectorAll('.theme-toggle').forEach((btn) => {
    btn.addEventListener('click', () => {
      const next = root.getAttribute('data-theme') === 'dark' ? 'light' : 'dark'
      try { localStorage.setItem(STORAGE_KEY, next) } catch (_) {}
      applyTheme(next)
    })
  })

  fetch(`https://api.github.com/repos/${REPO}`)
    .then((r) => (r.ok ? r.json() : null))
    .then((repo) => {
      if (!repo || typeof repo.stargazers_count !== 'number') return
      const n = repo.stargazers_count
      const text = n >= 1000 ? (n / 1000).toFixed(1).replace(/\.0$/, '') + 'k' : String(n)
      document.querySelectorAll('.stars').forEach((el) => (el.textContent = text))
    })
    .catch(() => {})

  fetch(`https://api.github.com/repos/${REPO}/releases/latest`)
    .then((r) => (r.ok ? r.json() : null))
    .then((rel) => {
      if (!rel) return
      const v = rel.tag_name.replace(/^v/, '')
      const setup = rel.assets.find((a) => /setup.*\.exe$/i.test(a.name))
      if (setup) document.querySelectorAll('a.dl').forEach((a) => (a.href = setup.browser_download_url))
      document.querySelectorAll('.ver').forEach((el) => (el.textContent = 'v' + v))
      const meta = document.getElementById('meta')
      if (meta) meta.textContent = `Version ${v}${setup ? ' · ' + Math.round(setup.size / 1048576) + ' MB' : ''} · Windows 10 & 11 · Free`
    })
    .catch(() => {})

  const toggle = document.querySelector('.side-toggle')
  const side = document.querySelector('.side')
  if (toggle && side) toggle.addEventListener('click', () => side.classList.toggle('open'))
})()
