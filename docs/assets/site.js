// Live GitHub star count, newest installer link, docs sidebar toggle.
;(() => {
  const REPO = 'hasanlior/polymira'

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
