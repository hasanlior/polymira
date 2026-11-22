/**
 * PlayStation controllers (DualShock 4, DualSense, DualSense Edge) read directly over HID.
 *
 * Windows' Xbox controller interface (XInput), which the main process polls for the Guide
 * button during games, does not see Sony controllers. WebHID does, and its input reports keep
 * arriving while a game has focus, so the PS button (or Share + Options) can open the
 * in-game menu without DS4Windows or Steam Input.
 */

const SONY = 0x054c
const DS4 = new Set([0x05c4, 0x09cc, 0x0ba0])
const DUALSENSE = new Set([0x0ce6, 0x0df2])

interface HidDevice extends EventTarget {
  vendorId: number
  productId: number
  opened: boolean
  productName: string
  open(): Promise<void>
}
interface HidInputReportEvent extends Event {
  device: HidDevice
  reportId: number
  data: DataView
}

type Buttons = { ps: boolean; share: boolean; options: boolean }

/** Where the PS / Share / Options bits are, per controller and report type. */
function readButtons(dev: HidDevice, reportId: number, d: DataView): Buttons | null {
  const at = (i: number) => (i < d.byteLength ? d.getUint8(i) : 0)
  if (DS4.has(dev.productId)) {
    // USB report 0x01 / Bluetooth report 0x11 (two extra header bytes)
    const o = reportId === 0x11 ? 2 : reportId === 0x01 ? 0 : -1
    if (o < 0) return null
    return { ps: (at(o + 6) & 0x01) !== 0, share: (at(o + 5) & 0x10) !== 0, options: (at(o + 5) & 0x20) !== 0 }
  }
  if (DUALSENSE.has(dev.productId)) {
    if (reportId === 0x31) return { ps: (at(9) & 0x01) !== 0, share: (at(8) & 0x10) !== 0, options: (at(8) & 0x20) !== 0 } // Bluetooth, full report
    if (reportId === 0x01 && d.byteLength >= 60) return { ps: (at(9) & 0x01) !== 0, share: (at(8) & 0x10) !== 0, options: (at(8) & 0x20) !== 0 } // USB
    if (reportId === 0x01) return { ps: (at(6) & 0x01) !== 0, share: (at(5) & 0x10) !== 0, options: (at(5) & 0x20) !== 0 } // Bluetooth, simple report
  }
  return null
}

export function startPsHid(onMenu: (how: 'guide' | 'combo') => void): void {
  const hid = (navigator as unknown as { hid?: EventTarget & { getDevices(): Promise<HidDevice[]> } }).hid
  if (!hid) return
  const prev = new WeakMap<HidDevice, Buttons>()

  const onReport = (e: Event) => {
    const ev = e as HidInputReportEvent
    const b = readButtons(ev.device, ev.reportId, ev.data)
    if (!b) return
    const p = prev.get(ev.device) ?? { ps: false, share: false, options: false }
    prev.set(ev.device, b)
    if (b.ps && !p.ps) onMenu('guide')
    else if (b.share && b.options && !(p.share && p.options)) onMenu('combo')
  }

  const attach = async (dev: HidDevice) => {
    if (dev.vendorId !== SONY || (!DS4.has(dev.productId) && !DUALSENSE.has(dev.productId))) return
    try {
      if (!dev.opened) await dev.open()
      dev.addEventListener('inputreport', onReport)
    } catch {
      // another program holds the controller exclusively
    }
  }

  void hid.getDevices().then((list) => list.forEach((d) => void attach(d)))
  hid.addEventListener('connect', (e) => void attach((e as unknown as { device: HidDevice }).device))
}
