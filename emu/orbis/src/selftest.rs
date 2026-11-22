//! Builds a small executable in the PS4 format (Sony program headers, SCE dynamic table,
//! "NID#lib#module" imports) and runs it through the real parser and loader. It calls one
//! implemented system function (sceKernelDebugOutText) and one that is not implemented yet
//! (sceVideoOutOpen), then returns 42.

use crate::format::*;
use crate::nid::nid;

fn push_phdr(out: &mut Vec<u8>, kind: u32, flags: u32, offset: u64, vaddr: u64, size: u64) {
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&vaddr.to_le_bytes());
    out.extend_from_slice(&vaddr.to_le_bytes()); // paddr
    out.extend_from_slice(&size.to_le_bytes()); // filesz
    out.extend_from_slice(&size.to_le_bytes()); // memsz
    out.extend_from_slice(&0x1000u64.to_le_bytes());
}

pub fn build_elf() -> Vec<u8> {
    const CODE: usize = 0x1000;
    const DYNLIB: usize = 0x2000;
    const DYNAMIC: usize = 0x3000;
    let mut f = vec![0u8; 0x3200];

    // ELF header
    f[..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);
    f[4] = 2; // 64-bit
    f[5] = 1; // little endian
    f[6] = 1;
    f[7] = 9; // FreeBSD ABI, as on PS4
    f[16..18].copy_from_slice(&0xFE10u16.to_le_bytes()); // ET_SCE_EXEC
    f[18..20].copy_from_slice(&62u16.to_le_bytes()); // x86-64
    f[24..32].copy_from_slice(&(CODE as u64).to_le_bytes()); // entry
    f[32..40].copy_from_slice(&64u64.to_le_bytes()); // phoff
    f[52..54].copy_from_slice(&64u16.to_le_bytes());
    f[54..56].copy_from_slice(&56u16.to_le_bytes());
    f[56..58].copy_from_slice(&3u16.to_le_bytes()); // phnum

    let mut ph = vec![];
    push_phdr(&mut ph, PT_LOAD, 7, CODE as u64, CODE as u64, 0x1000);
    push_phdr(&mut ph, PT_SCE_DYNLIBDATA, 4, DYNLIB as u64, 0, 0x1000);
    push_phdr(&mut ph, PT_DYNAMIC, 4, DYNAMIC as u64, 0, 0x200);
    f[64..64 + ph.len()].copy_from_slice(&ph);

    // Code (vaddr == file offset here)
    let code: &[u8] = &[
        0x53, // push rbx (keeps the stack 16-byte aligned for the calls)
        0x48, 0x8D, 0x35, 0xF8, 0x00, 0x00, 0x00, // lea rsi, [rip+0xF8]  -> message at 0x1100
        0x31, 0xFF, // xor edi, edi
        0xFF, 0x15, 0xF0, 0x01, 0x00, 0x00, // call [rip+0x1F0]     -> GOT slot 0x1200
        0xFF, 0x15, 0xF2, 0x01, 0x00, 0x00, // call [rip+0x1F2]     -> GOT slot 0x1208
        0xB8, 0x2A, 0x00, 0x00, 0x00, // mov eax, 42
        0x5B, // pop rbx
        0xC3, // ret
    ];
    f[CODE..CODE + code.len()].copy_from_slice(code);
    let msg = b"Hello from a PS4-format executable, running on POLYMIRA Orbis!\n\0";
    f[CODE + 0x100..CODE + 0x100 + msg.len()].copy_from_slice(msg);

    // Dynlib data: string table, symbol table, PLT relocations
    let mut strtab = vec![0u8];
    let mut add = |s: &str| {
        let off = strtab.len() as u64;
        strtab.extend_from_slice(s.as_bytes());
        strtab.push(0);
        off
    };
    let libkernel = add("libkernel");
    let videoout = add("libSceVideoOut");
    let sym1 = add(&format!("{}#B#B", nid("sceKernelDebugOutText")));
    let sym2 = add(&format!("{}#C#C", nid("sceVideoOutOpen")));
    f[DYNLIB..DYNLIB + strtab.len()].copy_from_slice(&strtab);

    let symtab = 0x200usize;
    for (i, name) in [0u64, sym1, sym2].iter().enumerate() {
        let o = DYNLIB + symtab + i * 24;
        f[o..o + 4].copy_from_slice(&(*name as u32).to_le_bytes());
        f[o + 4] = if i == 0 { 0 } else { 0x12 }; // global function
    }
    let rela = 0x300usize;
    for (i, (slot, sym)) in [(0x1200u64, 1u64), (0x1208, 2)].iter().enumerate() {
        let o = DYNLIB + rela + i * 24;
        f[o..o + 8].copy_from_slice(&slot.to_le_bytes());
        f[o + 8..o + 16].copy_from_slice(&((sym << 32) | 7).to_le_bytes()); // JUMP_SLOT
    }

    let entries: [(i64, u64); 10] = [
        (DT_SCE_STRTAB, 0),
        (DT_SCE_STRSZ, strtab.len() as u64),
        (DT_SCE_SYMTAB, symtab as u64),
        (DT_SCE_SYMTABSZ, 3 * 24),
        (DT_SCE_JMPREL, rela as u64),
        (DT_SCE_PLTRELSZ, 2 * 24),
        (DT_SCE_IMPORT_LIB, (1 << 48) | libkernel),
        (DT_SCE_NEEDED_MODULE, (1 << 48) | libkernel),
        (DT_SCE_IMPORT_LIB, (2 << 48) | videoout),
        (DT_SCE_NEEDED_MODULE, (2 << 48) | videoout),
    ];
    for (i, (tag, val)) in entries.iter().enumerate() {
        let o = DYNAMIC + i * 16;
        f[o..o + 8].copy_from_slice(&(*tag as u64).to_le_bytes());
        f[o + 8..o + 16].copy_from_slice(&val.to_le_bytes());
    }
    f
}

/// Wraps an ELF into a fake-signed SELF container (plain segments), like homebrew uses.
pub fn wrap_self(elf: &[u8]) -> Vec<u8> {
    let phnum = u16::from_le_bytes([elf[56], elf[57]]) as usize;
    let header_end = 0x20 + phnum * 0x20 + 64 + phnum * 56;
    let mut out = vec![0u8; header_end];
    out[..4].copy_from_slice(&[0x4F, 0x15, 0x3D, 0x1D]);
    out[0x18..0x1A].copy_from_slice(&(phnum as u16).to_le_bytes());
    let elf_at = 0x20 + phnum * 0x20;
    out[elf_at..elf_at + 64].copy_from_slice(&elf[..64]);
    out[elf_at + 64..elf_at + 64 + phnum * 56].copy_from_slice(&elf[64..64 + phnum * 56]);
    for i in 0..phnum {
        let p = 64 + i * 56;
        let off = u64::from_le_bytes(elf[p + 8..p + 16].try_into().unwrap()) as usize;
        let size = u64::from_le_bytes(elf[p + 32..p + 40].try_into().unwrap()) as usize;
        let data_at = out.len();
        out.extend_from_slice(&elf[off..off + size]);
        let e = 0x20 + i * 0x20;
        let props: u64 = 0x800 | ((i as u64) << 20); // blocked, segment = program header i, not encrypted
        out[e..e + 8].copy_from_slice(&props.to_le_bytes());
        out[e + 8..e + 16].copy_from_slice(&(data_at as u64).to_le_bytes());
        out[e + 16..e + 24].copy_from_slice(&(size as u64).to_le_bytes());
        out[e + 24..e + 32].copy_from_slice(&(size as u64).to_le_bytes());
    }
    out
}

pub fn run() -> Result<(), String> {
    for (label, bytes) in [("ELF", build_elf()), ("SELF", wrap_self(&build_elf()))] {
        println!("── {label} test executable ({} bytes)", bytes.len());
        let exe = Executable::parse(&bytes).map_err(|e| e.to_string())?;
        let imports = exe.imports();
        println!("   container: {}, imports: {}", exe.container, imports.iter().map(|i| format!("{}::{}", i.library, crate::known::names().get(&i.nid).copied().unwrap_or(&i.nid))).collect::<Vec<_>>().join(", "));
        let loaded = crate::loader::load(&exe)?;
        println!("   loaded at {:p}: {} bound to Orbis, {} stubbed", loaded.base, loaded.bound, loaded.stubbed);
        print!("   output: ");
        let code = crate::loader::run(&loaded);
        println!("   returned {code}");
        if code != 42 {
            return Err(format!("{label}: expected 42, got {code}"));
        }
    }
    println!("selftest passed");
    Ok(())
}
