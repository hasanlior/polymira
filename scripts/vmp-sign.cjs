// electron-builder afterPack hook: VMP-signs the packaged app so streaming services
// (Netflix, Disney+, Prime Video, Spotify) can play inside the console.
// Needs a free castlabs EVS account: `pip install --upgrade castlabs-evs` and
// `python -m castlabs_evs.account signup` (once). Without it the build still succeeds and
// DRM apps open in Microsoft Edge instead.
const { execFileSync } = require('child_process')

exports.default = async function vmpSign(context) {
  const python = process.env.PYTHON || 'python'
  try {
    execFileSync(python, ['-m', 'castlabs_evs.vmp', 'sign-pkg', context.appOutDir], { stdio: 'inherit' })
    console.log('[vmp] signed', context.appOutDir)
  } catch {
    console.warn('[vmp] not signed (castlabs-evs missing or not logged in). DRM apps will open in Edge.')
  }
}
