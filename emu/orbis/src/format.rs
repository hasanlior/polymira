//! PS4 executable formats.
//!
//! - SELF ("signed ELF", magic 4F 15 3D 1D): a header, a table of segment entries, then the
//!   ELF header and program headers. Retail files are encrypted; fake-signed files (homebrew,
//!   and dumps decrypted on the user's own console) store the segments in plain form.
//! - ELF64 with Sony program-header and dynamic-table extensions (PT_SCE_*, DT_SCE_*).

use std::fmt;

pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_TLS: u32 = 7;
pub const PT_SCE_DYNLIBDATA: u32 = 0x6100_0000;
pub const PT_SCE_PROCPARAM: u32 = 0x6100_0001;
pub const PT_SCE_RELRO: u32 = 0x6100_0010;

pub const DT_NULL: i64 = 0;
pub const DT_SCE_NEEDED_MODULE: i64 = 0x6100_000F;
pub const DT_SCE_IMPORT_LIB: i64 = 0x6100_0015;
pub const DT_SCE_JMPREL: i64 = 0x6100_0029;
pub const DT_SCE_PLTRELSZ: i64 = 0x6100_002D;
pub const DT_SCE_RELA: i64 = 0x6100_002F;
pub const DT_SCE_RELASZ: i64 = 0x6100_0031;
pub const DT_SCE_STRTAB: i64 = 0x6100_0035;
pub const DT_SCE_STRSZ: i64 = 0x6100_0037;
pub const DT_SCE_SYMTAB: i64 = 0x6100_0039;
pub const DT_SCE_SYMTABSZ: i64 = 0x6100_003F;

#[derive(Debug)]
pub enum FormatError {
    TooSmall,
    NotExecutable,
    Encrypted,
    Bad(&'static str),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::TooSmall => write!(f, "file is too small"),
            FormatError::NotExecutable => write!(f, "not a PS4 SELF or ELF executable"),
            FormatError::Encrypted => write!(f, "encrypted retail SELF: decrypt it on your own console first"),
            FormatError::Bad(why) => write!(f, "malformed executable: {why}"),
        }
    }
}

fn u16_at(b: &[u8], o: usize) -> Result<u16, FormatError> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or(FormatError::Bad("truncated"))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, FormatError> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or(FormatError::Bad("truncated"))
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, FormatError> {
    b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or(FormatError::Bad("truncated"))
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Phdr {
    pub kind: u32,
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Import {
    pub nid: String,
    pub library: String,
    pub module: String,
}

#[derive(Debug)]
pub struct Executable {
    /// The reconstructed ELF image (file layout).
    pub elf: Vec<u8>,
    pub container: &'static str,
    pub elf_type: u16,
    pub entry: u64,
    pub phdrs: Vec<Phdr>,
    pub dynamic: Vec<(i64, u64)>,
    pub libraries: Vec<(u32, String)>,
    pub modules: Vec<(u32, String)>,
}

impl Executable {
    pub fn parse(file: &[u8]) -> Result<Executable, FormatError> {
        if file.len() < 64 {
            return Err(FormatError::TooSmall);
        }
        let (elf, container) = if file[..4] == [0x4F, 0x15, 0x3D, 0x1D] {
            (unwrap_self(file)?, "SELF")
        } else if file[..4] == [0x7F, b'E', b'L', b'F'] {
            (file.to_vec(), "ELF")
        } else {
            return Err(FormatError::NotExecutable);
        };
        if elf.len() < 64 || elf[4] != 2 {
            return Err(FormatError::Bad("not a 64-bit ELF"));
        }
        let elf_type = u16_at(&elf, 16)?;
        let entry = u64_at(&elf, 24)?;
        let phoff = u64_at(&elf, 32)? as usize;
        let phnum = u16_at(&elf, 56)? as usize;
        let mut phdrs = Vec::with_capacity(phnum);
        for i in 0..phnum {
            let o = phoff + i * 56;
            phdrs.push(Phdr {
                kind: u32_at(&elf, o)?,
                flags: u32_at(&elf, o + 4)?,
                offset: u64_at(&elf, o + 8)?,
                vaddr: u64_at(&elf, o + 16)?,
                filesz: u64_at(&elf, o + 32)?,
                memsz: u64_at(&elf, o + 40)?,
                align: u64_at(&elf, o + 48)?,
            });
        }
        let mut exe = Executable { elf, container, elf_type, entry, phdrs, dynamic: vec![], libraries: vec![], modules: vec![] };
        exe.read_dynamic()?;
        Ok(exe)
    }

    pub fn segment(&self, kind: u32) -> Option<&Phdr> {
        self.phdrs.iter().find(|p| p.kind == kind)
    }

    pub fn dyn_value(&self, tag: i64) -> Option<u64> {
        self.dynamic.iter().find(|(t, _)| *t == tag).map(|(_, v)| *v)
    }

    /// Bytes of the PT_SCE_DYNLIBDATA segment, where Sony keeps the symbol and string tables.
    pub fn dynlib(&self) -> &[u8] {
        match self.segment(PT_SCE_DYNLIBDATA) {
            Some(p) => self.elf.get(p.offset as usize..(p.offset + p.filesz) as usize).unwrap_or(&[]),
            None => &[],
        }
    }

    fn read_dynamic(&mut self) -> Result<(), FormatError> {
        let Some(d) = self.segment(PT_DYNAMIC).cloned() else { return Ok(()) };
        let mut o = d.offset as usize;
        let end = (d.offset + d.filesz) as usize;
        while o + 16 <= end {
            let tag = u64_at(&self.elf, o)? as i64;
            let val = u64_at(&self.elf, o + 8)?;
            if tag == DT_NULL {
                break;
            }
            self.dynamic.push((tag, val));
            o += 16;
        }
        let strtab = self.dyn_value(DT_SCE_STRTAB).unwrap_or(0) as usize;
        let read_name = |exe: &Executable, off: u64| -> String {
            let dl = exe.dynlib();
            let start = strtab + (off & 0xFFFF_FFFF) as usize;
            let bytes = dl.get(start..).unwrap_or(&[]);
            let len = bytes.iter().position(|&b| b == 0).unwrap_or(0);
            String::from_utf8_lossy(&bytes[..len]).into_owned()
        };
        let mut libs = vec![];
        let mut mods = vec![];
        for &(tag, val) in &self.dynamic {
            let id = (val >> 48) as u32;
            if tag == DT_SCE_IMPORT_LIB {
                libs.push((id, read_name(self, val)));
            } else if tag == DT_SCE_NEEDED_MODULE {
                mods.push((id, read_name(self, val)));
            }
        }
        self.libraries = libs;
        self.modules = mods;
        Ok(())
    }

    /// Name of the symbol at `index` in the Sony symbol table.
    pub fn symbol_name(&self, index: u64) -> Option<String> {
        let dl = self.dynlib();
        let symtab = self.dyn_value(DT_SCE_SYMTAB)? as usize;
        let strtab = self.dyn_value(DT_SCE_STRTAB)? as usize;
        let name_off = u32_at(dl, symtab + index as usize * 24).ok()? as usize;
        let bytes = dl.get(strtab + name_off..)?;
        let len = bytes.iter().position(|&b| b == 0)?;
        Some(String::from_utf8_lossy(&bytes[..len]).into_owned())
    }

    /// Every imported function, from the symbol table ("NID#libraryId#moduleId").
    pub fn imports(&self) -> Vec<Import> {
        let dl = self.dynlib();
        let (Some(symtab), Some(size)) = (self.dyn_value(DT_SCE_SYMTAB), self.dyn_value(DT_SCE_SYMTABSZ)) else { return vec![] };
        let mut out = vec![];
        for i in 0..(size / 24) {
            let o = symtab as usize + i as usize * 24;
            let shndx = u16_at(dl, o + 6).unwrap_or(1);
            if shndx != 0 {
                continue; // defined here, not imported
            }
            let Some(name) = self.symbol_name(i) else { continue };
            let mut parts = name.split('#');
            let (Some(nid), Some(lib), Some(module)) = (parts.next(), parts.next(), parts.next()) else { continue };
            let lib_name = self.libraries.iter().find(|(id, _)| *id == crate::nid::decode_id(lib)).map(|(_, n)| n.clone()).unwrap_or_else(|| lib.to_string());
            let mod_name = self.modules.iter().find(|(id, _)| *id == crate::nid::decode_id(module)).map(|(_, n)| n.clone()).unwrap_or_else(|| module.to_string());
            out.push(Import { nid: nid.to_string(), library: lib_name, module: mod_name });
        }
        out
    }

    /// Relocation entries: (offset, type, symbol index, addend).
    pub fn relocations(&self) -> Vec<(u64, u32, u64, i64)> {
        let dl = self.dynlib();
        let mut out = vec![];
        for (start_tag, size_tag) in [(DT_SCE_RELA, DT_SCE_RELASZ), (DT_SCE_JMPREL, DT_SCE_PLTRELSZ)] {
            let (Some(start), Some(size)) = (self.dyn_value(start_tag), self.dyn_value(size_tag)) else { continue };
            for i in 0..size / 24 {
                let o = (start + i * 24) as usize;
                let (Ok(off), Ok(info), Ok(add)) = (u64_at(dl, o), u64_at(dl, o + 8), u64_at(dl, o + 16)) else { continue };
                out.push((off, (info & 0xFFFF_FFFF) as u32, info >> 32, add as i64));
            }
        }
        out
    }
}

/// Rebuilds the ELF file from a fake-signed (unencrypted) SELF.
fn unwrap_self(file: &[u8]) -> Result<Vec<u8>, FormatError> {
    let num_entries = u16_at(file, 0x18)? as usize;
    let elf_off = 0x20 + num_entries * 0x20;
    let elf_hdr = file.get(elf_off..elf_off + 64).ok_or(FormatError::Bad("truncated SELF"))?;
    if elf_hdr[..4] != [0x7F, b'E', b'L', b'F'] {
        return Err(FormatError::Bad("SELF without ELF header"));
    }
    let phnum = u16_at(elf_hdr, 56)? as usize;
    let ph_start = elf_off + 64;
    let mut size = 64 + phnum * 56;
    let mut phdrs = vec![];
    for i in 0..phnum {
        let o = ph_start + i * 56;
        let off = u64_at(file, o + 8)?;
        let filesz = u64_at(file, o + 32)?;
        size = size.max((off + filesz) as usize);
        phdrs.push((off, filesz));
    }
    let mut elf = vec![0u8; size];
    elf[..64].copy_from_slice(elf_hdr);
    // Program headers right after the ELF header in the rebuilt file.
    elf[64..64 + phnum * 56].copy_from_slice(&file[ph_start..ph_start + phnum * 56]);
    elf[32..40].copy_from_slice(&64u64.to_le_bytes());
    for e in 0..num_entries {
        let o = 0x20 + e * 0x20;
        let props = u64_at(file, o)?;
        let offset = u64_at(file, o + 8)? as usize;
        let filesz = u64_at(file, o + 16)? as usize;
        let blocked = props & 0x800 != 0;
        if !blocked {
            continue;
        }
        if props & 0x2 != 0 {
            return Err(FormatError::Encrypted);
        }
        if props & 0x8 != 0 {
            return Err(FormatError::Bad("compressed SELF segments are not supported yet"));
        }
        let ph = ((props >> 20) & 0xFFF) as usize;
        let Some(&(dst, _)) = phdrs.get(ph) else { continue };
        let src = file.get(offset..offset + filesz).ok_or(FormatError::Bad("segment outside file"))?;
        elf[dst as usize..dst as usize + filesz].copy_from_slice(src);
    }
    Ok(elf)
}
