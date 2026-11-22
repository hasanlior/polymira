/**
 * Thin Win32 bindings (via koffi) used for things Electron cannot do on its own:
 *  - reading XInput controllers while another app (the emulator) has focus,
 *  - suspending/resuming the game process (real pause, emulator-independent),
 *  - focusing the game window after the in-game menu closes,
 *  - per-process CPU / memory statistics.
 *
 * Every binding is optional: if anything fails to load, callers get `null`/no-op
 * and the rest of the app keeps working.
 */
import koffi from 'koffi'

type Fn = (...args: unknown[]) => unknown

interface Bindings {
  xinputGetState: ((index: number) => XInputState | null) | null
  hasGuideButton: boolean
  OpenProcess: Fn
  CloseHandle: Fn
  GetProcessTimes: Fn
  GetProcessMemoryInfo: Fn
  NtSuspendProcess: Fn
  NtResumeProcess: Fn
  EnumWindows: Fn
  GetWindowThreadProcessId: Fn
  IsWindowVisible: Fn
  SetForegroundWindow: Fn
  ShowWindow: Fn
  IsIconic: Fn
  EnumWindowsProc: unknown
}

export interface XInputState {
  packet: number
  buttons: number
  leftTrigger: number
  rightTrigger: number
  lx: number
  ly: number
}

export const XINPUT = {
  DPAD_UP: 0x0001,
  DPAD_DOWN: 0x0002,
  DPAD_LEFT: 0x0004,
  DPAD_RIGHT: 0x0008,
  START: 0x0010,
  BACK: 0x0020,
  LEFT_THUMB: 0x0040,
  RIGHT_THUMB: 0x0080,
  LB: 0x0100,
  RB: 0x0200,
  GUIDE: 0x0400,
  A: 0x1000,
  B: 0x2000,
  X: 0x4000,
  Y: 0x8000
} as const

const PROCESS_SUSPEND_RESUME = 0x0800
const PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
const PROCESS_VM_READ = 0x0010
const SW_RESTORE = 9

let bindings: Bindings | null = null
let loadError: string | null = null

function load(): Bindings | null {
  if (bindings || loadError) return bindings
  if (process.platform !== 'win32') {
    loadError = 'not windows'
    return null
  }
  try {
    const kernel32 = koffi.load('kernel32.dll')
    const ntdll = koffi.load('ntdll.dll')
    const user32 = koffi.load('user32.dll')

    const XINPUT_GAMEPAD = koffi.struct('XINPUT_GAMEPAD', {
      wButtons: 'uint16',
      bLeftTrigger: 'uint8',
      bRightTrigger: 'uint8',
      sThumbLX: 'int16',
      sThumbLY: 'int16',
      sThumbRX: 'int16',
      sThumbRY: 'int16'
    })
    const XINPUT_STATE = koffi.struct('XINPUT_STATE', {
      dwPacketNumber: 'uint32',
      Gamepad: XINPUT_GAMEPAD
    })
    const FILETIME = koffi.struct('FILETIME', { low: 'uint32', high: 'uint32' })
    const PMC = koffi.struct('PROCESS_MEMORY_COUNTERS', {
      cb: 'uint32',
      PageFaultCount: 'uint32',
      PeakWorkingSetSize: 'uintptr_t',
      WorkingSetSize: 'uintptr_t',
      QuotaPeakPagedPoolUsage: 'uintptr_t',
      QuotaPagedPoolUsage: 'uintptr_t',
      QuotaPeakNonPagedPoolUsage: 'uintptr_t',
      QuotaNonPagedPoolUsage: 'uintptr_t',
      PagefileUsage: 'uintptr_t',
      PeakPagefileUsage: 'uintptr_t'
    })

    // XInput: prefer the undocumented ordinal 100 (XInputGetStateEx) because it
    // reports the Guide button; fall back to the public XInputGetState.
    let xinputGetState: Bindings['xinputGetState'] = null
    let hasGuideButton = false
    for (const dll of ['xinput1_4.dll', 'xinput1_3.dll', 'xinput9_1_0.dll']) {
      try {
        const lib = koffi.load(dll)
        const LoadLibraryA = kernel32.func('void* __stdcall LoadLibraryA(const char *name)')
        const GetProcAddress = kernel32.func('void* __stdcall GetProcAddress(void *module, uintptr_t ordinal)')
        const proto = koffi.proto('uint32 __stdcall XInputGetStateFn(uint32 index, _Out_ XINPUT_STATE *state)')
        const mod = LoadLibraryA(dll)
        const exPtr = mod ? GetProcAddress(mod, 100) : null
        let raw: (i: number, out: Record<string, unknown>) => number
        if (exPtr) {
          raw = (i, out) => koffi.call(exPtr, proto, i, out) as number
          hasGuideButton = true
        } else {
          const f = lib.func('uint32 __stdcall XInputGetState(uint32 index, _Out_ XINPUT_STATE *state)')
          raw = (i, out) => f(i, out) as number
        }
        xinputGetState = (index: number) => {
          const out: Record<string, unknown> = {}
          if (raw(index, out) !== 0) return null
          const gp = out.Gamepad as Record<string, number>
          return {
            packet: out.dwPacketNumber as number,
            buttons: gp.wButtons,
            leftTrigger: gp.bLeftTrigger,
            rightTrigger: gp.bRightTrigger,
            lx: gp.sThumbLX,
            ly: gp.sThumbLY
          }
        }
        break
      } catch {
        // try next dll
      }
    }

    const EnumWindowsProc = koffi.proto('bool __stdcall EnumWindowsProc(void *hwnd, intptr_t lParam)')

    bindings = {
      xinputGetState,
      hasGuideButton,
      OpenProcess: kernel32.func('void* __stdcall OpenProcess(uint32 access, bool inherit, uint32 pid)'),
      CloseHandle: kernel32.func('bool __stdcall CloseHandle(void *h)'),
      GetProcessTimes: kernel32.func(
        'bool __stdcall GetProcessTimes(void *h, _Out_ FILETIME *c, _Out_ FILETIME *e, _Out_ FILETIME *k, _Out_ FILETIME *u)'
      ),
      GetProcessMemoryInfo: kernel32.func(
        'bool __stdcall K32GetProcessMemoryInfo(void *h, _Inout_ PROCESS_MEMORY_COUNTERS *pmc, uint32 cb)'
      ),
      NtSuspendProcess: ntdll.func('int32 __stdcall NtSuspendProcess(void *h)'),
      NtResumeProcess: ntdll.func('int32 __stdcall NtResumeProcess(void *h)'),
      EnumWindows: user32.func('bool __stdcall EnumWindows(EnumWindowsProc *cb, intptr_t lParam)'),
      GetWindowThreadProcessId: user32.func('uint32 __stdcall GetWindowThreadProcessId(void *hwnd, _Out_ uint32 *pid)'),
      IsWindowVisible: user32.func('bool __stdcall IsWindowVisible(void *hwnd)'),
      SetForegroundWindow: user32.func('bool __stdcall SetForegroundWindow(void *hwnd)'),
      ShowWindow: user32.func('bool __stdcall ShowWindow(void *hwnd, int cmd)'),
      IsIconic: user32.func('bool __stdcall IsIconic(void *hwnd)'),
      EnumWindowsProc
    }
    // Registered by name for the function signatures above.
    void XINPUT_STATE
    void FILETIME
    void PMC
    return bindings
  } catch (err) {
    loadError = String(err)
    console.warn('[native] Win32 bindings unavailable:', loadError)
    return null
  }
}

export function nativeAvailable(): boolean {
  return load() !== null
}

export function xinputSupportsGuide(): boolean {
  return load()?.hasGuideButton ?? false
}

/** Reads controller `index` (0-3). Returns null when not connected or XInput is unavailable. */
export function readXInput(index: number): XInputState | null {
  const b = load()
  return b?.xinputGetState ? b.xinputGetState(index) : null
}

function withProcess<T>(pid: number, access: number, fn: (h: unknown, b: Bindings) => T): T | null {
  const b = load()
  if (!b) return null
  const h = b.OpenProcess(access, false, pid)
  if (!h) return null
  try {
    return fn(h, b)
  } finally {
    b.CloseHandle(h)
  }
}

export function suspendProcess(pid: number): boolean {
  return withProcess(pid, PROCESS_SUSPEND_RESUME, (h, b) => b.NtSuspendProcess(h) === 0) ?? false
}

export function resumeProcess(pid: number): boolean {
  return withProcess(pid, PROCESS_SUSPEND_RESUME, (h, b) => b.NtResumeProcess(h) === 0) ?? false
}

/** Total CPU time (user + kernel) in milliseconds. */
export function processCpuTimeMs(pid: number): number | null {
  return withProcess(pid, PROCESS_QUERY_LIMITED_INFORMATION, (h, b) => {
    const c = {}, e = {}, k: Record<string, number> = {}, u: Record<string, number> = {}
    if (!b.GetProcessTimes(h, c, e, k, u)) return null
    const ft = (t: Record<string, number>) => (t.high * 4294967296 + t.low) / 10000
    return ft(k) + ft(u)
  })
}

export function processWorkingSetMb(pid: number): number | null {
  return withProcess(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, (h, b) => {
    const pmc: Record<string, number> = { cb: 72 }
    if (!b.GetProcessMemoryInfo(h, pmc, 72)) return null
    return Number(pmc.WorkingSetSize) / (1024 * 1024)
  })
}

/** Finds the first visible top-level window owned by one of `pids`. */
function findWindow(pids: Set<number>): unknown | null {
  const b = load()
  if (!b) return null
  let found: unknown = null
  const cb = koffi.register((hwnd: unknown) => {
    const out = [0]
    b.GetWindowThreadProcessId(hwnd, out)
    if (pids.has(out[0]) && b.IsWindowVisible(hwnd)) {
      found = hwnd
      return false
    }
    return true
  }, koffi.pointer(b.EnumWindowsProc as koffi.IKoffiCType))
  try {
    b.EnumWindows(cb, 0)
  } finally {
    koffi.unregister(cb)
  }
  return found
}

/** Brings the game's main window back to the front. */
export function focusProcessWindow(pids: number[]): boolean {
  const b = load()
  if (!b) return false
  const hwnd = findWindow(new Set(pids))
  if (!hwnd) return false
  if (b.IsIconic(hwnd)) b.ShowWindow(hwnd, SW_RESTORE)
  return Boolean(b.SetForegroundWindow(hwnd))
}
