# POLYMIRA Orbis

Orbis is POLYMIRA's own, experimental PS4 core, written in Rust. It is at an early stage: it does not run commercial games yet.

PS4 and PC share the x86-64 instruction set, so a game's own code can run directly on the PC's processor. What has to be emulated is the system around it: the PS4's kernel and libraries (files, threads, memory, video, audio, controllers). Orbis is built in that order.

## What works

- Reads PS4 and PS5 executables: SELF containers (fake-signed / decrypted) and ELF files with Sony's program-header and dynamic-table extensions.
- Thread-local storage, crash reports with the game offset, instruction bytes, registers and call chain.
- Lists every imported system function by NID and library, and reports how many of them Orbis implements (`orbis analyze`).
- Loads an executable into memory, applies its relocations and binds each import to a native implementation or to a stub that reports it.
- Calls the entry point using the PS4 calling convention (System V).
- Implemented so far: a handful of libkernel, libc and system-service functions (see `src/hle.rs`).

`orbis selftest` builds a small program in the PS4 format, loads and runs it: it prints through `sceKernelDebugOutText`, calls a function that is not implemented yet (reported, not fatal) and exits with 42.

## First real game: Minecraft (PS5, decrypted dump)

What happens today with a real PS5 game executable (254 MB `eboot.bin`):

- **Analysis:** 42 system libraries, 876 imported functions (libc 347, libkernel 70, libScePosix 59, libSceAgc 48, …).
- **Loading:** all segments mapped, **719,741 relocations** applied, 70 imports bound to Orbis, the rest stubbed.
- **Thread-local storage:** the game's 47 `fs:` accesses are rewritten to Orbis' TLS block.
- **Execution:** the game's own start-up code runs on the PC processor: environment setup, `atexit` handlers, C++ static initialisers, mutexes, thread keys.
- **Where it stops now:** while constructing C++ streams (`std::locale` / `iostream`), which needs the console's C/C++ runtime library. That is the next milestone: implement it, or load the user's own `libc.prx` system module.

PS4 packages (`.pkg`) are recognised, but their file system is encrypted; use the game folder instead.

## Roadmap

1. ~~Thread-local storage~~ (done), C/C++ runtime (locale, streams), libkernel memory and threads: reach the game's `main`.
2. Video out to a window, controller input, audio out: homebrew samples running.
3. GPU command processing (Gnm) on Vulkan: the long part.
4. Commercial games, one system library at a time, with `orbis analyze` showing what each game needs.

## Use

```bash
cargo build --release
target/release/orbis analyze path/to/eboot.bin
target/release/orbis run path/to/eboot.bin
target/release/orbis selftest
```

In POLYMIRA: a PS4 or PS5 game's options → **Orbis analysis** / **Try with Orbis**, and Settings → Emulation Settings → **POLYMIRA Orbis** for the self-test.

Orbis runs only files you provide. It contains no Sony code, keys or firmware, and cannot decrypt retail files; executables must be decrypted on your own console.
