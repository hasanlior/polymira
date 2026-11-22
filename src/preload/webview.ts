import { ipcRenderer, webFrame } from 'electron'

/**
 * Preload for pages shown in the console's built-in browser.
 * - The console reads the controller itself and turns it into keys and a pointer, so the page
 *   must not also see the raw gamepad (it would get every press twice).
 * - Tells the console when a text field is focused (to offer the on-screen keyboard) and
 *   forwards the console's menu key.
 */

void webFrame.executeJavaScript(`(() => {
  try {
    Object.defineProperty(Navigator.prototype, 'getGamepads', { value: function () { return [] }, configurable: true })
    const block = (e) => e.stopImmediatePropagation()
    window.addEventListener('gamepadconnected', block, true)
    window.addEventListener('gamepaddisconnected', block, true)
  } catch (e) {}
})()`)

function isTextField(el: Element | null): boolean {
  if (!el) return false
  if ((el as HTMLElement).isContentEditable) return true
  if (el.tagName === 'TEXTAREA') return true
  if (el.tagName !== 'INPUT') return false
  const type = ((el as HTMLInputElement).type || 'text').toLowerCase()
  return ['text', 'search', 'email', 'url', 'tel', 'password', 'number'].includes(type)
}

window.addEventListener('focusin', () => ipcRenderer.sendToHost('text-focus', isTextField(document.activeElement)), true)
window.addEventListener('focusout', () => setTimeout(() => ipcRenderer.sendToHost('text-focus', isTextField(document.activeElement)), 0), true)

window.addEventListener(
  'keydown',
  (e) => {
    if (e.key === 'F1' || (e.key === 'Home' && e.ctrlKey && e.shiftKey)) {
      e.preventDefault()
      e.stopPropagation()
      ipcRenderer.sendToHost('menu')
    }
  },
  true
)
