//! CPU interpreter for PS5 GPU shaders, one lane at a time.
//!
//! Used for vertex (NGG primitive) shaders: UI and menu draws have few vertices, and running
//! them on the CPU gives exact results with direct access to the game's memory. Each vertex is
//! a "wave" of one active lane (lane 0): exec/vcc masks are 64-bit values where bit 0 matters.

use super::{decode_shader, Ext, Fmt, Inst};

#[derive(Clone, Copy, Debug, Default)]
pub struct Export {
    pub target: u32,
    pub value: [f32; 4],
    pub mask: u32,
}

pub struct Lane {
    pub s: [u32; 128],
    pub v: Vec<u32>,
    pub scc: bool,
    pub exports: Vec<Export>,
}

#[derive(Debug)]
pub enum Error {
    Unsupported(&'static str, u32),
    Memory(u64),
    Runaway,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported(n, pc) => write!(f, "unsupported instruction {n} at {pc:#x}"),
            Error::Memory(a) => write!(f, "unreadable memory at {a:#x}"),
            Error::Runaway => write!(f, "too many instructions"),
        }
    }
}

const VCC: usize = 106;
const M0: usize = 124;
const EXEC: usize = 126;

fn f(x: u32) -> f32 {
    f32::from_bits(x)
}
fn u(x: f32) -> u32 {
    x.to_bits()
}

/// Reads guest memory (the game's address space is this process' address space).
pub type MemCheck = fn(usize, usize) -> bool;

impl Lane {
    pub fn new() -> Lane {
        let mut l = Lane { s: [0; 128], v: vec![0; 256], scc: false, exports: vec![] };
        l.set64(EXEC, 1);
        l
    }

    fn get64(&self, r: usize) -> u64 {
        self.s[r] as u64 | ((self.s[r + 1] as u64) << 32)
    }
    fn set64(&mut self, r: usize, v: u64) {
        self.s[r] = v as u32;
        self.s[r + 1] = (v >> 32) as u32;
    }
    fn exec_on(&self) -> bool {
        self.s[EXEC] & 1 != 0
    }

    /// Scalar/vector source operand (9-bit encoding).
    fn src(&self, code: u16, lit: Option<u32>) -> u32 {
        match code {
            0..=105 => self.s[code as usize],
            106..=127 => self.s[code as usize],
            128..=192 => (code - 128) as u32,
            193..=208 => (-((code - 192) as i32)) as u32,
            240 => u(0.5),
            241 => u(-0.5),
            242 => u(1.0),
            243 => u(-1.0),
            244 => u(2.0),
            245 => u(-2.0),
            246 => u(4.0),
            247 => u(-4.0),
            248 => 0x3E22_F983, // 1/(2*pi)
            251 => (self.s[VCC] & 1 == 0) as u32,
            252 => (!self.exec_on()) as u32,
            253 => self.scc as u32,
            255 => lit.unwrap_or(0),
            256..=511 => self.v[(code - 256) as usize],
            _ => 0,
        }
    }
    /// 64-bit scalar source.
    fn src64(&self, code: u16, lit: Option<u32>) -> u64 {
        match code {
            0..=127 => self.get64(code as usize),
            128..=192 => (code - 128) as u64,
            193..=208 => (-((code - 192) as i64)) as u64,
            255 => lit.unwrap_or(0) as u64,
            242 => 0x3FF0_0000_0000_0000,
            _ => self.src(code, lit) as u64,
        }
    }
    fn set_sdst(&mut self, r: u16, v: u32) {
        if (r as usize) < 128 && r != 125 {
            self.s[r as usize] = v;
        }
    }
    fn set_sdst64(&mut self, r: u16, v: u64) {
        if (r as usize) < 127 && r != 125 {
            self.set64(r as usize, v);
        }
    }
    /// Writes a lane mask bit (VOPC results, v_cmp) into an SGPR pair.
    fn set_mask(&mut self, r: usize, bit: bool) {
        let old = self.get64(r);
        self.set64(r, (old & !1) | bit as u64);
    }
}

fn read_u32(addr: u64, ok: MemCheck) -> Result<u32, Error> {
    if !ok(addr as usize, 4) {
        return Err(Error::Memory(addr));
    }
    Ok(unsafe { (addr as *const u32).read_unaligned() })
}

/// Decodes a buffer element of GFX10 unified format `fmt` at `addr` into 4 components.
fn load_format(addr: u64, fmt: u32, ok: MemCheck) -> Result<[u32; 4], Error> {
    let one = u(1.0);
    let rd = |o: u64, n: usize| -> Result<Vec<u8>, Error> {
        if !ok((addr + o) as usize, n) {
            return Err(Error::Memory(addr + o));
        }
        Ok(unsafe { std::slice::from_raw_parts((addr + o) as *const u8, n) }.to_vec())
    };
    let h = |b: &[u8], i: usize| half::f16::from_bits(u16::from_le_bytes([b[i * 2], b[i * 2 + 1]])).to_f32();
    let w = |b: &[u8], i: usize| u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
    Ok(match fmt {
        22 => [w(&rd(0, 4)?, 0), 0, 0, one],                                   // 32_FLOAT
        20 | 21 => [w(&rd(0, 4)?, 0), 0, 0, 1],                                // 32_UINT/SINT
        64 => { let b = rd(0, 8)?; [w(&b, 0), w(&b, 1), 0, one] }              // 32_32_FLOAT
        62 | 63 => { let b = rd(0, 8)?; [w(&b, 0), w(&b, 1), 0, 1] }
        74 => { let b = rd(0, 12)?; [w(&b, 0), w(&b, 1), w(&b, 2), one] }      // 32_32_32_FLOAT
        72 | 73 => { let b = rd(0, 12)?; [w(&b, 0), w(&b, 1), w(&b, 2), 1] }
        77 | 75 | 76 => { let b = rd(0, 16)?; [w(&b, 0), w(&b, 1), w(&b, 2), w(&b, 3)] }
        29 => { let b = rd(0, 4)?; [u(h(&b, 0)), u(h(&b, 1)), 0, one] }        // 16_16_FLOAT
        71 => { let b = rd(0, 8)?; [u(h(&b, 0)), u(h(&b, 1)), u(h(&b, 2)), u(h(&b, 3))] } // 16x4_FLOAT
        13 => { let b = rd(0, 2)?; [u(h(&b, 0)), 0, 0, one] }
        11 | 12 => { let b = rd(0, 2)?; [u16::from_le_bytes([b[0], b[1]]) as u32, 0, 0, 1] } // 16_UINT/SINT
        5 | 6 => { let b = rd(0, 1)?; [b[0] as u32, 0, 0, 1] } // 8_UINT/SINT
        18 | 19 => { let b = rd(0, 2)?; [b[0] as u32, b[1] as u32, 0, 1] } // 8_8_UINT/SINT
        23 => { let b = rd(0, 4)?; let c = |i: usize| u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]) as f32 / 65535.0; [u(c(0)), u(c(1)), 0, one] }
        65 => { let b = rd(0, 8)?; let c = |i: usize| u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]) as f32 / 65535.0; [u(c(0)), u(c(1)), u(c(2)), u(c(3))] }
        24 => { let b = rd(0, 4)?; let c = |i: usize| (i16::from_le_bytes([b[i * 2], b[i * 2 + 1]]) as f32 / 32767.0).max(-1.0); [u(c(0)), u(c(1)), 0, one] }
        66 => { let b = rd(0, 8)?; let c = |i: usize| (i16::from_le_bytes([b[i * 2], b[i * 2 + 1]]) as f32 / 32767.0).max(-1.0); [u(c(0)), u(c(1)), u(c(2)), u(c(3))] }
        56 => { let b = rd(0, 4)?; [u(b[0] as f32 / 255.0), u(b[1] as f32 / 255.0), u(b[2] as f32 / 255.0), u(b[3] as f32 / 255.0)] } // 8x4_UNORM
        57 => { let b = rd(0, 4)?; let c = |x: u8| u((x as i8 as f32 / 127.0).max(-1.0)); [c(b[0]), c(b[1]), c(b[2]), c(b[3])] }
        60 | 61 => { let b = rd(0, 4)?; [b[0] as u32, b[1] as u32, b[2] as u32, b[3] as u32] }
        14 => { let b = rd(0, 2)?; [u(b[0] as f32 / 255.0), u(b[1] as f32 / 255.0), 0, one] }
        1 => { let b = rd(0, 1)?; [u(b[0] as f32 / 255.0), 0, 0, one] }
        27 | 28 => { let b = rd(0, 4)?; [u16::from_le_bytes([b[0], b[1]]) as u32, u16::from_le_bytes([b[2], b[3]]) as u32, 0, 1] }
        69 | 70 => { let b = rd(0, 8)?; let c = |i: usize| u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]) as u32; [c(0), c(1), c(2), c(3)] }
        _ => return Err(Error::Unsupported("buffer format", fmt)),
    })
}

/// V# fields.
fn vsharp(l: &Lane, srsrc: usize) -> (u64, u64, u32) {
    let w0 = l.s[srsrc] as u64;
    let w1 = l.s[srsrc + 1];
    let base = w0 | (((w1 & 0xFFFF) as u64) << 32);
    let stride = ((w1 >> 16) & 0x3FFF) as u64;
    let fmt = (l.s[srsrc + 3] >> 12) & 0x7F;
    (base, stride, fmt)
}

fn bfe(x: u64, off: u32, width: u32) -> u64 {
    if width == 0 {
        return 0;
    }
    let v = x >> (off & 63);
    if width >= 64 { v } else { v & ((1u64 << width) - 1) }
}

/// Runs the shader for one lane until s_endpgm.
pub fn run(code: &[u32], lane: &mut Lane, ok: MemCheck) -> Result<(), Error> {
    let insts = decode_shader(code);
    let by_pc: std::collections::HashMap<u32, usize> = insts.iter().enumerate().map(|(i, x)| (x.pc, i)).collect();
    let mut i = 0usize;
    let mut steps = 0u32;
    while i < insts.len() {
        steps += 1;
        if steps > 200_000 {
            return Err(Error::Runaway);
        }
        let inst = &insts[i];
        let next_pc = inst.pc + inst.words.len() as u32 * 4;
        match step(inst, lane, ok)? {
            Flow::Next => i += 1,
            Flow::End => return Ok(()),
            Flow::Branch(rel) => {
                let target = (next_pc as i64 + rel as i64 * 4) as u32;
                i = *by_pc.get(&target).ok_or(Error::Unsupported("branch target", target))?;
            }
        }
    }
    Ok(())
}

enum Flow {
    Next,
    End,
    Branch(i16),
}

fn step(inst: &Inst, l: &mut Lane, ok: MemCheck) -> Result<Flow, Error> {
    let w0 = inst.w0();
    let lit = inst.literal;
    let n = inst.name;
    let un = || Err(Error::Unsupported(n, inst.pc));
    match inst.fmt {
        Fmt::Sopp => {
            let imm = (w0 & 0xFFFF) as i16;
            return Ok(match n {
                "s_endpgm" => Flow::End,
                "s_branch" => Flow::Branch(imm),
                "s_cbranch_scc0" if !l.scc => Flow::Branch(imm),
                "s_cbranch_scc1" if l.scc => Flow::Branch(imm),
                "s_cbranch_vccz" if l.s[VCC] & 1 == 0 => Flow::Branch(imm),
                "s_cbranch_vccnz" if l.s[VCC] & 1 != 0 => Flow::Branch(imm),
                "s_cbranch_execz" if !l.exec_on() => Flow::Branch(imm),
                "s_cbranch_execnz" if l.exec_on() => Flow::Branch(imm),
                _ => Flow::Next, // waits, nops, prefetch, sendmsg (GS alloc), barriers
            });
        }
        Fmt::Sop2 => {
            let (d, a, b) = (((w0 >> 16) & 0x7F) as u16, (w0 & 0xFF) as u16, ((w0 >> 8) & 0xFF) as u16);
            let (x, y) = (l.src(a, lit), l.src(b, lit));
            let (x64, y64) = (l.src64(a, lit), l.src64(b, lit));
            match n {
                "s_add_u32" => { let r = x as u64 + y as u64; l.scc = r > 0xFFFF_FFFF; l.set_sdst(d, r as u32) }
                "s_sub_u32" => { l.scc = y > x; l.set_sdst(d, x.wrapping_sub(y)) }
                "s_add_i32" => { let r = (x as i32).wrapping_add(y as i32); l.scc = (x as i32).checked_add(y as i32).is_none(); l.set_sdst(d, r as u32) }
                "s_sub_i32" => { let r = (x as i32).wrapping_sub(y as i32); l.scc = (x as i32).checked_sub(y as i32).is_none(); l.set_sdst(d, r as u32) }
                "s_addc_u32" => { let r = x as u64 + y as u64 + l.scc as u64; l.scc = r > 0xFFFF_FFFF; l.set_sdst(d, r as u32) }
                "s_and_b32" => { let r = x & y; l.scc = r != 0; l.set_sdst(d, r) }
                "s_or_b32" => { let r = x | y; l.scc = r != 0; l.set_sdst(d, r) }
                "s_xor_b32" => { let r = x ^ y; l.scc = r != 0; l.set_sdst(d, r) }
                "s_andn2_b32" => { let r = x & !y; l.scc = r != 0; l.set_sdst(d, r) }
                "s_nor_b32" => { let r = !(x | y); l.scc = r != 0; l.set_sdst(d, r) }
                "s_xnor_b32" => { let r = !(x ^ y); l.scc = r != 0; l.set_sdst(d, r) }
                "s_nand_b32" => { let r = !(x & y); l.scc = r != 0; l.set_sdst(d, r) }
                "s_and_b64" => { let r = x64 & y64; l.scc = r != 0; l.set_sdst64(d, r) }
                "s_or_b64" => { let r = x64 | y64; l.scc = r != 0; l.set_sdst64(d, r) }
                "s_xor_b64" => { let r = x64 ^ y64; l.scc = r != 0; l.set_sdst64(d, r) }
                "s_andn2_b64" => { let r = x64 & !y64; l.scc = r != 0; l.set_sdst64(d, r) }
                "s_orn2_b64" => { let r = x64 | !y64; l.scc = r != 0; l.set_sdst64(d, r) }
                "s_lshl_b32" => { let r = x << (y & 31); l.scc = r != 0; l.set_sdst(d, r) }
                "s_lshr_b32" => { let r = x >> (y & 31); l.scc = r != 0; l.set_sdst(d, r) }
                "s_ashr_i32" => { let r = ((x as i32) >> (y & 31)) as u32; l.scc = r != 0; l.set_sdst(d, r) }
                "s_lshl_b64" => { let r = x64 << (y & 63); l.scc = r != 0; l.set_sdst64(d, r) }
                "s_lshr_b64" => { let r = x64 >> (y & 63); l.scc = r != 0; l.set_sdst64(d, r) }
                "s_lshl1_add_u32" | "s_lshl2_add_u32" | "s_lshl3_add_u32" | "s_lshl4_add_u32" => {
                    let sh = n.as_bytes()[6] - b'0';
                    let r = ((x as u64) << sh) + y as u64;
                    l.scc = r > 0xFFFF_FFFF;
                    l.set_sdst(d, r as u32)
                }
                "s_mul_i32" => l.set_sdst(d, (x as i32).wrapping_mul(y as i32) as u32),
                "s_mul_hi_u32" => l.set_sdst(d, ((x as u64 * y as u64) >> 32) as u32),
                "s_mul_hi_i32" => l.set_sdst(d, ((x as i32 as i64 * y as i32 as i64) >> 32) as u32),
                "s_min_u32" => { l.scc = x < y; l.set_sdst(d, x.min(y)) }
                "s_max_u32" => { l.scc = x > y; l.set_sdst(d, x.max(y)) }
                "s_min_i32" => { l.scc = (x as i32) < (y as i32); l.set_sdst(d, (x as i32).min(y as i32) as u32) }
                "s_max_i32" => { l.scc = (x as i32) > (y as i32); l.set_sdst(d, (x as i32).max(y as i32) as u32) }
                "s_cselect_b32" => l.set_sdst(d, if l.scc { x } else { y }),
                "s_cselect_b64" => l.set_sdst64(d, if l.scc { x64 } else { y64 }),
                "s_bfe_u32" => { let r = bfe(x as u64, y & 31, (y >> 16) & 0x7F) as u32; l.scc = r != 0; l.set_sdst(d, r) }
                "s_bfe_i32" => {
                    let (o, wd) = (y & 31, (y >> 16) & 0x7F);
                    let r = if wd == 0 { 0 } else { (((x << (32 - (o + wd).min(32))) as i32) >> (32 - wd.min(32))) as u32 };
                    l.scc = r != 0;
                    l.set_sdst(d, r)
                }
                "s_bfe_u64" => { let r = bfe(x64, y & 63, (y >> 16) & 0x7F); l.scc = r != 0; l.set_sdst64(d, r) }
                "s_bfm_b32" => l.set_sdst(d, (((1u64 << (x & 31)) - 1) as u32) << (y & 31)),
                "s_pack_ll_b32_b16" => l.set_sdst(d, (x & 0xFFFF) | (y << 16)),
                "s_pack_lh_b32_b16" => l.set_sdst(d, (x & 0xFFFF) | (y & 0xFFFF_0000)),
                "s_absdiff_i32" => { let r = ((x as i32).wrapping_sub(y as i32)).unsigned_abs(); l.scc = r != 0; l.set_sdst(d, r) }
                _ => return un(),
            }
        }
        Fmt::Sopk => {
            let d = ((w0 >> 16) & 0x7F) as u16;
            let k = w0 & 0xFFFF;
            let cur = l.src(d, None);
            match n {
                "s_movk_i32" => l.set_sdst(d, k as i16 as i32 as u32),
                "s_cmovk_i32" => if l.scc { l.set_sdst(d, k as i16 as i32 as u32) },
                "s_addk_i32" => l.set_sdst(d, (cur as i32).wrapping_add(k as i16 as i32) as u32),
                "s_mulk_i32" => l.set_sdst(d, (cur as i32).wrapping_mul(k as i16 as i32) as u32),
                "s_cmpk_eq_i32" => l.scc = cur as i32 == k as i16 as i32,
                "s_cmpk_lg_i32" => l.scc = cur as i32 != k as i16 as i32,
                "s_cmpk_lt_i32" => l.scc = (cur as i32) < k as i16 as i32,
                "s_cmpk_le_u32" => l.scc = cur <= k,
                "s_cmpk_gt_u32" => l.scc = cur > k,
                "s_cmpk_eq_u32" => l.scc = cur == k,
                "s_cmpk_lg_u32" => l.scc = cur != k,
                "s_getreg_b32" => l.set_sdst(d, 0),
                "s_setreg_b32" | "s_setreg_imm32_b32" => {}
                _ => return un(),
            }
        }
        Fmt::Sop1 => {
            let (d, a) = (((w0 >> 16) & 0x7F) as u16, (w0 & 0xFF) as u16);
            let x = l.src(a, lit);
            let x64 = l.src64(a, lit);
            match n {
                "s_mov_b32" => l.set_sdst(d, x),
                "s_mov_b64" => l.set_sdst64(d, x64),
                "s_not_b32" => { l.scc = !x != 0; l.set_sdst(d, !x) }
                "s_wqm_b64" | "s_wqm_b32" => { l.scc = x64 != 0; l.set_sdst64(d, if x64 & 0xF != 0 { x64 | 0xF } else { x64 }) }
                "s_brev_b32" => l.set_sdst(d, x.reverse_bits()),
                "s_and_saveexec_b64" => { let e = l.get64(EXEC); l.set_sdst64(d, e); let r = x64 & e; l.set64(EXEC, r); l.scc = r != 0 }
                "s_or_saveexec_b64" => { let e = l.get64(EXEC); l.set_sdst64(d, e); let r = x64 | e; l.set64(EXEC, r); l.scc = r != 0 }
                "s_andn2_saveexec_b64" => { let e = l.get64(EXEC); l.set_sdst64(d, e); let r = x64 & !e; l.set64(EXEC, r); l.scc = r != 0 }
                "s_bcnt1_i32_b64" => { let r = x64.count_ones(); l.scc = r != 0; l.set_sdst(d, r) }
                "s_ff1_i32_b64" => l.set_sdst(d, if x64 == 0 { u32::MAX } else { x64.trailing_zeros() }),
                "s_sext_i32_i16" => l.set_sdst(d, x as i16 as i32 as u32),
                "s_getpc_b64" => l.set_sdst64(d, (inst.pc + 4) as u64),
                "s_setpc_b64" | "s_swappc_b64" => return un(),
                _ => return un(),
            }
        }
        Fmt::Sopc => {
            let (a, b) = ((w0 & 0xFF) as u16, ((w0 >> 8) & 0xFF) as u16);
            let (x, y) = (l.src(a, lit), l.src(b, lit));
            l.scc = match n {
                "s_cmp_eq_u32" | "s_cmp_eq_i32" => x == y,
                "s_cmp_lg_u32" | "s_cmp_lg_i32" => x != y,
                "s_cmp_lt_u32" => x < y,
                "s_cmp_le_u32" => x <= y,
                "s_cmp_gt_u32" => x > y,
                "s_cmp_ge_u32" => x >= y,
                "s_cmp_lt_i32" => (x as i32) < (y as i32),
                "s_cmp_le_i32" => (x as i32) <= (y as i32),
                "s_cmp_gt_i32" => (x as i32) > (y as i32),
                "s_cmp_ge_i32" => (x as i32) >= (y as i32),
                "s_bitcmp0_b32" => x & (1 << (y & 31)) == 0,
                "s_bitcmp1_b32" => x & (1 << (y & 31)) != 0,
                "s_cmp_eq_u64" => l.src64(a, lit) == l.src64(b, lit),
                "s_cmp_lg_u64" => l.src64(a, lit) != l.src64(b, lit),
                _ => return un(),
            };
        }
        Fmt::Smem => {
            let sbase = ((w0 & 0x3F) * 2) as usize;
            let sdata = ((w0 >> 6) & 0x7F) as usize;
            let w1 = inst.w1();
            let mut off = ((w1 & 0x1F_FFFF) as i32) << 11 >> 11;
            let soff = (w1 >> 25) as u16;
            if soff != 125 {
                off = off.wrapping_add(l.src(soff, None) as i32);
            }
            let count = match n {
                _ if n.ends_with("dwordx16") => 16,
                _ if n.ends_with("dwordx8") => 8,
                _ if n.ends_with("dwordx4") => 4,
                _ if n.ends_with("dwordx2") => 2,
                _ if n.ends_with("dword") => 1,
                _ => return un(),
            };
            let base = if n.starts_with("s_buffer_load") {
                let w1v = l.s[sbase + 1];
                l.s[sbase] as u64 | (((w1v & 0xFFFF) as u64) << 32)
            } else {
                l.get64(sbase) & 0xFFFF_FFFF_FFFF
            };
            let addr = (base as i64 + off as i64) as u64 & !3;
            for k in 0..count {
                l.s[sdata + k] = read_u32(addr + k as u64 * 4, ok)?;
            }
        }
        Fmt::Mubuf | Fmt::Mtbuf => {
            if !l.exec_on() {
                return Ok(Flow::Next);
            }
            let w1 = inst.w1();
            let inst_off = (w0 & 0xFFF) as u64;
            let offen = w0 & (1 << 12) != 0;
            let idxen = w0 & (1 << 13) != 0;
            let vaddr = (w1 & 0xFF) as usize;
            let vdata = ((w1 >> 8) & 0xFF) as usize;
            let srsrc = (((w1 >> 16) & 0x1F) * 4) as usize;
            let soff = l.src(((w1 >> 24) & 0xFF) as u16, None) as u64;
            let (base, stride, vfmt) = vsharp(l, srsrc);
            let mut va = vaddr;
            let mut addr = base + soff + inst_off;
            if idxen {
                addr += l.v[va] as u64 * stride;
                va += 1;
            }
            if offen {
                addr += l.v[va] as u64;
            }
            let fmt = if inst.fmt == Fmt::Mtbuf { (w0 >> 19) & 0x7F } else { vfmt };
            let comps = if n.contains("xyzw") { 4 } else if n.contains("xyz") { 3 } else if n.contains("xy") { 2 } else { 1 };
            if n.contains("load_format") {
                let c = load_format(addr, fmt, ok)?;
                for k in 0..comps {
                    l.v[vdata + k] = c[k];
                }
            } else if n.starts_with("buffer_load_dword") || n.starts_with("buffer_load_dwordx") {
                let cnt = if n.ends_with("x4") { 4 } else if n.ends_with("x3") { 3 } else if n.ends_with("x2") { 2 } else { 1 };
                for k in 0..cnt {
                    l.v[vdata + k] = read_u32(addr + k as u64 * 4, ok)?;
                }
            } else if n.starts_with("buffer_store") || n.starts_with("tbuffer_store") {
                // Vertex shaders do not store; ignore.
            } else {
                return un();
            }
        }
        Fmt::Exp => {
            let w1 = inst.w1();
            let en = w0 & 0xF;
            let target = (w0 >> 4) & 0x3F;
            let compr = w0 & (1 << 10) != 0;
            let regs = [w1 & 0xFF, (w1 >> 8) & 0xFF, (w1 >> 16) & 0xFF, (w1 >> 24) & 0xFF];
            let mut value = [0f32; 4];
            if compr {
                let lo = l.v[regs[0] as usize];
                let hi = l.v[if regs[1] == regs[0] { regs[2] } else { regs[1] } as usize];
                let h = |x: u32| half::f16::from_bits(x as u16).to_f32();
                value = [h(lo), h(lo >> 16), h(hi), h(hi >> 16)];
            } else {
                for k in 0..4 {
                    value[k] = f(l.v[regs[k] as usize]);
                }
            }
            if l.exec_on() {
                l.exports.push(Export { target, value, mask: en });
            }
        }
        Fmt::Vop1 | Fmt::Vop2 | Fmt::Vopc | Fmt::Vop3 => {
            if !l.exec_on() && inst.fmt != Fmt::Vopc && !n.starts_with("v_readfirstlane") {
                return Ok(Flow::Next);
            }
            return vop(inst, l).map(|_| Flow::Next);
        }
        _ => return un(),
    }
    Ok(Flow::Next)
}

/// Source with SDWA selection applied (sel: 0..3 byte, 4/5 word, 6 dword).
fn sdwa_sel(x: u32, sel: u32, sext: bool) -> u32 {
    match sel {
        0..=3 => { let b = (x >> (sel * 8)) & 0xFF; if sext { b as u8 as i8 as i32 as u32 } else { b } }
        4 | 5 => { let w = (x >> ((sel - 4) * 16)) & 0xFFFF; if sext { w as u16 as i16 as i32 as u32 } else { w } }
        _ => x,
    }
}

fn vop(inst: &Inst, l: &mut Lane) -> Result<(), Error> {
    let w0 = inst.w0();
    let w1 = inst.w1();
    let lit = inst.literal;
    let full = inst.name;
    let n = full.trim_end_matches("_e32").trim_end_matches("_e64").trim_end_matches("_sdwa");
    // Operands: (src0, src1, src2) raw values, destination, VOP3 modifiers.
    let (mut a, mut b, mut c, dst, vop3) = match inst.fmt {
        Fmt::Vop1 => (l.src((w0 & 0x1FF) as u16, lit), 0, 0, ((w0 >> 17) & 0xFF) as usize, false),
        Fmt::Vop2 => {
            let v1 = l.v[((w0 >> 9) & 0xFF) as usize];
            let d = ((w0 >> 17) & 0xFF) as usize;
            let s0 = l.src((w0 & 0x1FF) as u16, lit);
            // madmk: d = s0 * K + v1 ; madak: d = s0 * v1 + K ; mac: d = s0 * v1 + d
            match n {
                "v_madmk_f32" | "v_fmamk_f32" => (s0, lit.unwrap_or(0), v1, d, false),
                "v_madak_f32" | "v_fmaak_f32" => (s0, v1, lit.unwrap_or(0), d, false),
                _ => (s0, v1, l.v[d], d, false),
            }
        }
        Fmt::Vopc => (l.src((w0 & 0x1FF) as u16, lit), l.v[((w0 >> 9) & 0xFF) as usize], 0, 0, false),
        _ => (
            l.src((w1 & 0x1FF) as u16, lit),
            l.src(((w1 >> 9) & 0x1FF) as u16, lit),
            l.src(((w1 >> 18) & 0x1FF) as u16, lit),
            (w0 & 0xFF) as usize,
            true,
        ),
    };
    let mut clamp = false;
    let mut omod = 0;
    let mut sdwa_dst: Option<(u32, u32)> = None;
    if inst.ext == Ext::Sdwa {
        let s = inst.words[1];
        let src0 = s & 0xFF;
        a = if s & (1 << 23) != 0 { l.src(src0 as u16, None) } else { l.v[src0 as usize] };
        // S1: the second source is an SGPR / constant instead of a VGPR.
        if s & (1 << 31) != 0 && inst.fmt != Fmt::Vop1 {
            b = l.src(((w0 >> 9) & 0xFF) as u16, None);
        }
        a = sdwa_sel(a, (s >> 16) & 7, s & (1 << 19) != 0);
        let s1sel = (s >> 24) & 7;
        b = sdwa_sel(b, s1sel, s & (1 << 27) != 0);
        if s & (1 << 21) != 0 { a = u(f(a).abs()) }
        if s & (1 << 20) != 0 { a = u(-f(a)) }
        if s & (1 << 29) != 0 { b = u(f(b).abs()) }
        if s & (1 << 28) != 0 { b = u(-f(b)) }
        if inst.fmt != Fmt::Vopc {
            sdwa_dst = Some(((s >> 8) & 7, (s >> 11) & 3));
            clamp = s & (1 << 13) != 0;
        }
    }
    let is_float = n.contains("_f32") || n.starts_with("v_rcp") || n.starts_with("v_rsq") || n.contains("_legacy_f32");
    if vop3 && is_float {
        let abs = (w0 >> 8) & 7;
        let neg = w1 >> 29;
        let m = |x: u32, k: u32| -> u32 {
            let mut v = f(x);
            if abs & (1 << k) != 0 { v = v.abs() }
            if neg & (1 << k) != 0 { v = -v }
            u(v)
        };
        a = m(a, 0);
        b = m(b, 1);
        c = m(c, 2);
        clamp = w0 & (1 << 15) != 0;
        omod = (w1 >> 27) & 3;
    }
    // VOPC / v_cmp: produce a lane bit.
    if n.starts_with("v_cmp") || n.starts_with("v_cmpx") {
        let (fa, fb) = (f(a), f(b));
        let (ia, ib) = (a as i32, b as i32);
        let op = n.trim_start_matches("v_cmpx_").trim_start_matches("v_cmp_");
        let r = match op {
            "lt_f32" => fa < fb, "eq_f32" => fa == fb, "le_f32" => fa <= fb, "gt_f32" => fa > fb,
            "lg_f32" => fa < fb || fa > fb, "ge_f32" => fa >= fb, "neq_f32" => !(fa == fb), "nlt_f32" => !(fa < fb),
            "o_f32" => !fa.is_nan() && !fb.is_nan(), "u_f32" => fa.is_nan() || fb.is_nan(),
            "lt_i32" => ia < ib, "eq_i32" => ia == ib, "le_i32" => ia <= ib, "gt_i32" => ia > ib, "ne_i32" => ia != ib, "ge_i32" => ia >= ib,
            "lt_u32" => a < b, "eq_u32" => a == b, "le_u32" => a <= b, "gt_u32" => a > b, "ne_u32" => a != b, "ge_u32" => a >= b,
            _ => return Err(Error::Unsupported(full, inst.pc)),
        };
        let sdst = if vop3 { (w0 & 0xFF) as usize } else if inst.ext == Ext::Sdwa && inst.words[1] & (1 << 15) != 0 { ((inst.words[1] >> 8) & 0x7F) as usize } else { VCC };
        if n.starts_with("v_cmpx") {
            l.set_mask(EXEC, r);
        }
        if sdst != 125 {
            l.set_mask(sdst, r);
        }
        return Ok(());
    }
    let fa = f(a);
    let fb = f(b);
    let fc = f(c);
    let r: u32 = match n {
        "v_mov_b32" => a,
        "v_add_f32" => u(fa + fb),
        "v_sub_f32" => u(fa - fb),
        "v_subrev_f32" => u(fb - fa),
        "v_mul_f32" => u(fa * fb),
        "v_mul_legacy_f32" => u(if fa == 0.0 || fb == 0.0 { 0.0 } else { fa * fb }),
        "v_mac_f32" | "v_fmac_f32" | "v_mad_f32" | "v_fma_f32" | "v_madmk_f32" | "v_madak_f32" | "v_fmamk_f32" | "v_fmaak_f32" => u(fa * fb + fc),
        "v_mac_legacy_f32" | "v_fmac_legacy_f32" | "v_mad_legacy_f32" => u(if fa == 0.0 || fb == 0.0 { fc } else { fa * fb + fc }),
        "v_min_f32" => u(fa.min(fb)),
        "v_max_f32" => u(fa.max(fb)),
        "v_med3_f32" => u(fa.max(fb).min(fa.min(fb).max(fc))),
        "v_rcp_f32" | "v_rcp_iflag_f32" => u(1.0 / fa),
        "v_rsq_f32" => u(1.0 / fa.sqrt()),
        "v_sqrt_f32" => u(fa.sqrt()),
        "v_floor_f32" => u(fa.floor()),
        "v_ceil_f32" => u(fa.ceil()),
        "v_fract_f32" => u(fa - fa.floor()),
        "v_trunc_f32" => u(fa.trunc()),
        "v_rndne_f32" => u(fa.round_ties_even()),
        "v_exp_f32" => u(fa.exp2()),
        "v_log_f32" => u(fa.log2()),
        "v_sin_f32" => u((fa * std::f32::consts::TAU).sin()),
        "v_cos_f32" => u((fa * std::f32::consts::TAU).cos()),
        "v_cvt_f32_i32" => u(a as i32 as f32),
        "v_cvt_f32_u32" => u(a as f32),
        "v_cvt_i32_f32" => (fa as i32) as u32,
        "v_cvt_u32_f32" => fa.max(0.0) as u32,
        "v_cvt_f32_f16" => u(half::f16::from_bits(a as u16).to_f32()),
        "v_cvt_f16_f32" => half::f16::from_f32(fa).to_bits() as u32,
        "v_cvt_pkrtz_f16_f32" => half::f16::from_f32(fa).to_bits() as u32 | ((half::f16::from_f32(fb).to_bits() as u32) << 16),
        "v_cvt_f32_ubyte0" => u((a & 0xFF) as f32),
        "v_cvt_f32_ubyte1" => u(((a >> 8) & 0xFF) as f32),
        "v_cvt_f32_ubyte2" => u(((a >> 16) & 0xFF) as f32),
        "v_cvt_f32_ubyte3" => u((a >> 24) as f32),
        "v_add_nc_u32" | "v_add_u32" => a.wrapping_add(b),
        "v_sub_nc_u32" => a.wrapping_sub(b),
        "v_subrev_nc_u32" => b.wrapping_sub(a),
        "v_add3_u32" => a.wrapping_add(b).wrapping_add(c),
        "v_lshl_add_u32" => (a << (b & 31)).wrapping_add(c),
        "v_add_lshl_u32" => a.wrapping_add(b) << (c & 31),
        "v_lshl_or_b32" => (a << (b & 31)) | c,
        "v_and_or_b32" => (a & b) | c,
        "v_or3_b32" => a | b | c,
        "v_xor3_b32" => a ^ b ^ c,
        "v_xad_u32" => (a ^ b).wrapping_add(c),
        "v_mul_lo_u32" => a.wrapping_mul(b),
        "v_mul_hi_u32" => ((a as u64 * b as u64) >> 32) as u32,
        "v_mul_u32_u24" => (a & 0xFF_FFFF).wrapping_mul(b & 0xFF_FFFF),
        "v_mul_hi_u32_u24" => (((a & 0xFF_FFFF) as u64 * (b & 0xFF_FFFF) as u64) >> 32) as u32,
        "v_mul_i32_i24" => ((((a << 8) as i32) >> 8).wrapping_mul(((b << 8) as i32) >> 8)) as u32,
        "v_and_b32" => a & b,
        "v_or_b32" => a | b,
        "v_xor_b32" => a ^ b,
        "v_xnor_b32" => !(a ^ b),
        "v_not_b32" => !a,
        "v_lshlrev_b32" => b << (a & 31),
        "v_lshrrev_b32" => b >> (a & 31),
        "v_ashrrev_i32" => ((b as i32) >> (a & 31)) as u32,
        "v_min_i32" => (a as i32).min(b as i32) as u32,
        "v_max_i32" => (a as i32).max(b as i32) as u32,
        "v_min_u32" => a.min(b),
        "v_max_u32" => a.max(b),
        "v_bfe_u32" => bfe(a as u64, b & 31, c & 31) as u32,
        "v_bfi_b32" => (a & b) | (!a & c),
        "v_cndmask_b32" => {
            let mask = if vop3 { l.src(((w1 >> 18) & 0x1FF) as u16, None) } else if inst.ext == Ext::Sdwa { l.s[VCC] } else { l.s[VCC] };
            if mask & 1 != 0 { b } else { a }
        }
        "v_readfirstlane_b32" => {
            let d = ((w0 >> 17) & 0xFF) as u16;
            l.set_sdst(d, a);
            return Ok(());
        }
        _ => return Err(Error::Unsupported(full, inst.pc)),
    };
    let mut r = r;
    if is_float {
        let mut v = f(r);
        v *= match omod { 1 => 2.0, 2 => 4.0, 3 => 0.5, _ => 1.0 };
        if clamp {
            v = v.clamp(0.0, 1.0);
        }
        r = u(v);
    }
    if let Some((sel, unused)) = sdwa_dst {
        let old = l.v[dst];
        r = match sel {
            0..=3 => {
                let sh = sel * 8;
                let keep = if unused == 2 { old & !(0xFF << sh) } else { 0 };
                keep | ((r & 0xFF) << sh)
            }
            4 | 5 => {
                let sh = (sel - 4) * 16;
                let keep = if unused == 2 { old & !(0xFFFF << sh) } else { 0 };
                keep | ((r & 0xFFFF) << sh)
            }
            _ => r,
        };
    }
    l.v[dst] = r;
    Ok(())
}

/// SGPR-like operand codes a vector instruction reads, in a fixed order. The pixel shader
/// translator gives each one a push-constant slot; the scalar pass fills the slots.
pub fn scalar_slots(inst: &Inst) -> Vec<u16> {
    let is_sgpr = |c: u16| c <= 105 || c == 106 || c == 107 || c == 124;
    let w0 = inst.w0();
    let w1 = inst.w1();
    let mut out = vec![];
    match inst.fmt {
        Fmt::Vop1 | Fmt::Vop2 | Fmt::Vopc => {
            let s0 = if inst.ext == Ext::Sdwa {
                let s = inst.words[1];
                if s & (1 << 23) != 0 { (s & 0xFF) as u16 } else { 256 }
            } else {
                (w0 & 0x1FF) as u16
            };
            if is_sgpr(s0) {
                out.push(s0);
            }
            if inst.ext == Ext::Sdwa && inst.fmt != Fmt::Vop1 && inst.words[1] & (1 << 31) != 0 {
                let s1 = ((w0 >> 9) & 0xFF) as u16;
                if is_sgpr(s1) {
                    out.push(s1);
                }
            }
        }
        Fmt::Vop3 | Fmt::Vop3p => {
            for k in 0..3 {
                let c = ((w1 >> (k * 9)) & 0x1FF) as u16;
                if is_sgpr(c) {
                    out.push(c);
                }
            }
        }
        _ => {}
    }
    out
}

/// What the scalar pass learns about one draw's pixel shader.
#[derive(Default, Clone)]
pub struct ScalarInfo {
    /// Values of the vector instructions' scalar operands, in slot order.
    pub slots: Vec<u32>,
    /// T# (8 dwords) and S# (4 dwords) of each image instruction, in program order.
    pub images: Vec<([u32; 8], [u32; 4])>,
}

/// Runs only the scalar part of a shader (uniform for the whole draw).
pub fn run_scalar(code: &[u32], lane: &mut Lane, ok: MemCheck) -> Result<ScalarInfo, Error> {
    let insts = decode_shader(code);
    let by_pc: std::collections::HashMap<u32, usize> = insts.iter().enumerate().map(|(i, x)| (x.pc, i)).collect();
    let mut info = ScalarInfo::default();
    let mut i = 0usize;
    let mut steps = 0u32;
    while i < insts.len() {
        steps += 1;
        if steps > 200_000 {
            return Err(Error::Runaway);
        }
        let inst = &insts[i];
        let next_pc = inst.pc + inst.words.len() as u32 * 4;
        match inst.fmt {
            Fmt::Vop1 | Fmt::Vop2 | Fmt::Vopc | Fmt::Vop3 | Fmt::Vop3p => {
                for c in scalar_slots(inst) {
                    info.slots.push(lane.src(c, inst.literal));
                }
                i += 1;
                continue;
            }
            Fmt::Mimg => {
                let w1 = inst.w1();
                let srsrc = (((w1 >> 16) & 0x1F) * 4) as usize;
                let ssamp = (((w1 >> 21) & 0x1F) * 4) as usize;
                let mut t = [0u32; 8];
                t.copy_from_slice(&lane.s[srsrc..srsrc + 8]);
                let mut sm = [0u32; 4];
                sm.copy_from_slice(&lane.s[ssamp..ssamp + 4]);
                info.images.push((t, sm));
                i += 1;
                continue;
            }
            Fmt::Vintrp | Fmt::Exp | Fmt::Mubuf | Fmt::Mtbuf | Fmt::Ds | Fmt::Flat | Fmt::Global | Fmt::Scratch => {
                i += 1;
                continue;
            }
            _ => {}
        }
        match step(inst, lane, ok)? {
            Flow::Next => i += 1,
            Flow::End => break,
            Flow::Branch(rel) => {
                let target = (next_pc as i64 + rel as i64 * 4) as u32;
                // A jump to the end skips pixels that were all discarded: the translated
                // shader still runs the rest, so the slots must still be collected.
                if target >= insts.last().map(|x| x.pc).unwrap_or(0) {
                    i += 1;
                } else {
                    i = *by_pc.get(&target).ok_or(Error::Unsupported("branch target", target))?;
                }
            }
        }
    }
    Ok(info)
}

/// Runs a pixel shader for one pixel on the CPU and records the instructions it executes
/// (`pcs`, in order) together with the scalar slot values and texture descriptors along that
/// path. `attrs` are the interpolated attributes; texture reads return zero.
pub fn trace_ps(code: &[u32], lane: &mut Lane, attrs: &[[f32; 4]; 8], ok: MemCheck) -> Result<(Vec<u32>, ScalarInfo), Error> {
    let insts = decode_shader(code);
    let by_pc: std::collections::HashMap<u32, usize> = insts.iter().enumerate().map(|(i, x)| (x.pc, i)).collect();
    let end_pc = insts.last().map(|x| x.pc).unwrap_or(0);
    let mut info = ScalarInfo::default();
    let mut pcs = vec![];
    let mut i = 0usize;
    let mut steps = 0u32;
    while i < insts.len() {
        steps += 1;
        if steps > 100_000 {
            return Err(Error::Runaway);
        }
        let inst = &insts[i];
        let next_pc = inst.pc + inst.words.len() as u32 * 4;
        pcs.push(inst.pc);
        match inst.fmt {
            Fmt::Vop1 | Fmt::Vop2 | Fmt::Vopc | Fmt::Vop3 | Fmt::Vop3p => {
                for c in scalar_slots(inst) {
                    info.slots.push(lane.src(c, inst.literal));
                }
            }
            Fmt::Vintrp => {
                let w0 = inst.w0();
                let vdst = ((w0 >> 18) & 0xFF) as usize;
                let attr = ((w0 >> 10) & 0x3F) as usize;
                let chan = ((w0 >> 8) & 3) as usize;
                lane.v[vdst] = attrs.get(attr).map(|a| a[chan]).unwrap_or(0.0).to_bits();
                i += 1;
                continue;
            }
            Fmt::Mimg => {
                let w0 = inst.w0();
                let w1 = inst.w1();
                let srsrc = (((w1 >> 16) & 0x1F) * 4) as usize;
                let ssamp = (((w1 >> 21) & 0x1F) * 4) as usize;
                let mut t = [0u32; 8];
                t.copy_from_slice(&lane.s[srsrc..srsrc + 8]);
                let mut sm = [0u32; 4];
                sm.copy_from_slice(&lane.s[ssamp..ssamp + 4]);
                info.images.push((t, sm));
                let vdata = ((w1 >> 8) & 0xFF) as usize;
                let n = ((w0 >> 8) & 0xF).count_ones() as usize;
                for k in 0..n {
                    lane.v[vdata + k] = 0;
                }
                i += 1;
                continue;
            }
            Fmt::Exp | Fmt::Mubuf | Fmt::Mtbuf | Fmt::Ds | Fmt::Flat | Fmt::Global | Fmt::Scratch => {
                i += 1;
                continue;
            }
            _ => {}
        }
        match step(inst, lane, ok)? {
            Flow::Next => i += 1,
            Flow::End => break,
            Flow::Branch(rel) => {
                let target = (next_pc as i64 + rel as i64 * 4) as u32;
                if target >= end_pc {
                    i += 1; // "all pixels discarded" shortcut: the translated path keeps going
                } else {
                    i = *by_pc.get(&target).ok_or(Error::Unsupported("branch target", target))?;
                }
            }
        }
    }
    Ok((pcs, info))
}
