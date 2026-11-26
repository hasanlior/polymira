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
    pub v: [u32; 256],
    pub scc: bool,
    pub exports: Vec<Export>,
    /// Memory writes (buffer/image stores) take effect: compute shaders only.
    pub stores: bool,
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
        let mut l = Lane { s: [0; 128], v: [0; 256], scc: false, exports: Vec::with_capacity(16), stores: false };
        l.set64(EXEC, 1);
        l
    }

    pub fn reset(&mut self) {
        self.s.fill(0);
        self.v.fill(0);
        self.scc = false;
        self.exports.clear();
        self.set64(EXEC, 1);
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

    /// Public wrapper for `src` used by `rdna::vs`.
    pub fn src_pub(&self, code: u16, lit: Option<u32>) -> u32 {
        self.src(code, lit)
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
    let rd = |o: u64, n: usize| -> Result<[u8; 16], Error> {
        let mut buf = [0u8; 16];
        if !ok((addr + o) as usize, n) {
            return Err(Error::Memory(addr + o));
        }
        unsafe { std::ptr::copy_nonoverlapping((addr + o) as *const u8, buf.as_mut_ptr(), n) };
        Ok(buf)
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

/// Pre-decoded RDNA2 shader with O(1) branch table and precomputed scalar slots.
pub struct CompiledShader {
    pub insts: Vec<Inst>,
    pub pc_to_idx: Vec<u32>,
    pub slots: Vec<[u16; 3]>,
    pub slot_lens: Vec<u8>,
    pub end_pc: u32,
    /// Leading contiguous pure-scalar (VGPR-independent, branch-free) instructions that can be
    /// evaluated once per draw and shared across all vertices.
    pub scalar_prefix_len: usize,
    /// True when the entire shader is branch-free and has no vector->scalar writes (no Vopc,
    /// no readlane), so ALL scalar/SMEM instructions can be hoisted once per draw via `VsDrawPlan`.
    pub can_hoist_scalar: bool,
    /// True when eligible for GPU SPIR-V VS translation (`rdna::vs::translate`).
    pub gpu_vs_eligible: bool,
}

impl CompiledShader {
    pub fn new(code: &[u32]) -> Self {
        let insts = decode_shader(code);
        let end_pc = insts.last().map(|x| x.pc).unwrap_or(0);
        let table_len = (end_pc as usize / 4) + 1;
        let mut pc_to_idx = vec![u32::MAX; table_len];
        let mut slots = Vec::with_capacity(insts.len());
        let mut slot_lens = Vec::with_capacity(insts.len());
        for (i, inst) in insts.iter().enumerate() {
            let slot_idx = (inst.pc / 4) as usize;
            if slot_idx < table_len {
                pc_to_idx[slot_idx] = i as u32;
            }
            let mut s = [0u16; 3];
            let mut len = 0u8;
            if matches!(inst.fmt, Fmt::Vop1 | Fmt::Vop2 | Fmt::Vopc | Fmt::Vop3 | Fmt::Vop3p) {
                for c in scalar_slots(inst) {
                    if (len as usize) < 3 {
                        s[len as usize] = c;
                        len += 1;
                    }
                }
            }
            slots.push(s);
            slot_lens.push(len);
        }
        let mut scalar_prefix_len = 0usize;
        for inst in &insts {
            let pure_scalar = match inst.fmt {
                Fmt::Sop1 | Fmt::Sop2 | Fmt::Sopc | Fmt::Sopk | Fmt::Smem => true,
                Fmt::Sopp => !matches!(
                    inst.name,
                    "s_endpgm"
                        | "s_branch"
                        | "s_cbranch_scc0"
                        | "s_cbranch_scc1"
                        | "s_cbranch_vccz"
                        | "s_cbranch_vccnz"
                        | "s_cbranch_execz"
                        | "s_cbranch_execnz"
                ),
                _ => false,
            };
            if !pure_scalar {
                break;
            }
            scalar_prefix_len += 1;
        }
        let mut has_exp = false;
        let can_hoist_scalar = insts.iter().all(|inst| {
            match inst.fmt {
                Fmt::Sopp => !matches!(
                    inst.name,
                    "s_cbranch_vccz" | "s_cbranch_vccnz"
                ),
                Fmt::Vopc | Fmt::Ds | Fmt::Mimg | Fmt::Flat | Fmt::Global | Fmt::Scratch | Fmt::Vintrp | Fmt::Vop3p => false,
                Fmt::Vop1 | Fmt::Vop2 | Fmt::Vop3 => {
                    !inst.name.starts_with("v_cmp")
                        && !inst.name.starts_with("v_readfirstlane")
                        && !inst.name.starts_with("v_readlane")
                        && inst.ext != Ext::Dpp16
                        && inst.ext != Ext::Dpp8
                }
                Fmt::Exp => {
                    has_exp = true;
                    true
                }
                _ => true,
            }
        }) && has_exp;
        let mut has_exp_pos = false;
        let gpu_vs_eligible = insts.iter().all(|inst| match inst.fmt {
            Fmt::Sopp => match inst.name {
                "s_cbranch_execz" => ((inst.w0() & 0xFFFF) as i16) >= 0,
                "s_branch"
                | "s_cbranch_scc0"
                | "s_cbranch_scc1"
                | "s_cbranch_vccz"
                | "s_cbranch_vccnz"
                | "s_cbranch_execnz" => false,
                _ => true,
            },
            Fmt::Ds | Fmt::Mimg | Fmt::Flat | Fmt::Global | Fmt::Scratch | Fmt::Vintrp | Fmt::Vop3p => false,
            Fmt::Vopc => inst.name.starts_with("v_cmpx_"),
            Fmt::Vop1 | Fmt::Vop2 | Fmt::Vop3 => {
                !inst.name.starts_with("v_cmp")
                    && !inst.name.starts_with("v_readfirstlane")
                    && !inst.name.starts_with("v_readlane")
                    && inst.ext != Ext::Dpp16
                    && inst.ext != Ext::Dpp8
            }
            Fmt::Exp => {
                if ((inst.w0() >> 4) & 0x3F) == 12 {
                    has_exp_pos = true;
                }
                true
            }
            _ => true,
        }) && has_exp_pos;
        Self { insts, pc_to_idx, slots, slot_lens, end_pc, scalar_prefix_len, can_hoist_scalar, gpu_vs_eligible }
    }

    #[inline]
    fn idx_of(&self, pc: u32) -> Option<usize> {
        if pc & 3 != 0 {
            return None;
        }
        let v = *self.pc_to_idx.get((pc / 4) as usize)?;
        if v == u32::MAX { None } else { Some(v as usize) }
    }
}

/// Runs the uniform scalar prefix `0..sh.scalar_prefix_len` once per draw.
pub fn run_compiled_prefix(sh: &CompiledShader, lane: &mut Lane, ok: MemCheck) -> Result<(), Error> {
    for inst in &sh.insts[..sh.scalar_prefix_len] {
        match step(inst, lane, ok)? {
            Flow::Next => {}
            Flow::End | Flow::Branch(_) => break,
        }
    }
    Ok(())
}

/// Runs a pre-decoded shader for one lane starting at instruction index `start` until s_endpgm.
pub fn run_compiled_from(sh: &CompiledShader, start: usize, lane: &mut Lane, ok: MemCheck) -> Result<(), Error> {
    let insts = &sh.insts;
    let mut i = start;
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
                i = sh.idx_of(target).ok_or(Error::Unsupported("branch target", target))?;
            }
        }
    }
    Ok(())
}

/// Runs a pre-decoded shader for one lane until s_endpgm.
pub fn run_compiled(sh: &CompiledShader, lane: &mut Lane, ok: MemCheck) -> Result<(), Error> {
    run_compiled_from(sh, 0, lane, ok)
}

/// Runs the shader for one lane until s_endpgm.
pub fn run(code: &[u32], lane: &mut Lane, ok: MemCheck) -> Result<(), Error> {
    let sh = CompiledShader::new(code);
    run_compiled(&sh, lane, ok)
}

enum Flow {
    Next,
    End,
    Branch(i16),
}

/// Executes a single scalar/SMEM instruction for `rdna::vs::extract_gpu_vs_draw`.
pub fn step_scalar_for_gpu(inst: &Inst, l: &mut Lane, ok: MemCheck) -> Option<()> {
    step(inst, l, ok).ok().map(|_| ())
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
                if l.stores {
                    if n.contains("store_format") {
                        let mut vals = [0u32; 4];
                        for k in 0..comps {
                            vals[k] = l.v[vdata + k];
                        }
                        let (bytes, bpe) = pack_element(&vals, fmt).ok_or(Error::Unsupported("buffer format", fmt))?;
                        write_bytes(addr, &bytes[..bpe], ok)?;
                    } else if n.starts_with("buffer_store_byte") {
                        write_bytes(addr, &[l.v[vdata] as u8], ok)?;
                    } else if n.starts_with("buffer_store_short") {
                        write_bytes(addr, &(l.v[vdata] as u16).to_le_bytes(), ok)?;
                    } else {
                        let cnt = if n.ends_with("x4") { 4 } else if n.ends_with("x3") { 3 } else if n.ends_with("x2") { 2 } else { 1 };
                        let mut b = [0u8; 16];
                        for k in 0..cnt {
                            b[k * 4..k * 4 + 4].copy_from_slice(&l.v[vdata + k].to_le_bytes());
                        }
                        write_bytes(addr, &b[..cnt * 4], ok)?;
                    }
                }
            } else if n.starts_with("buffer_load_ubyte") {
                if !ok(addr as usize, 1) {
                    return Err(Error::Memory(addr));
                }
                l.v[vdata] = unsafe { *(addr as *const u8) } as u32;
            } else if n.starts_with("buffer_load_ushort") {
                if !ok(addr as usize, 2) {
                    return Err(Error::Memory(addr));
                }
                l.v[vdata] = unsafe { (addr as *const u16).read_unaligned() } as u32;
            } else {
                return un();
            }
        }
        Fmt::Mimg if n.starts_with("image_load") || n.starts_with("image_store") => {
            if l.exec_on() {
                image_op(inst, l, ok)?;
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

/// Runs only the scalar part of a pre-decoded shader (uniform for the whole draw).
pub fn run_scalar_compiled(sh: &CompiledShader, lane: &mut Lane, ok: MemCheck) -> Result<ScalarInfo, Error> {
    let insts = &sh.insts;
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
                let len = sh.slot_lens[i] as usize;
                for &c in &sh.slots[i][..len] {
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
                if target >= sh.end_pc {
                    i += 1;
                } else {
                    i = sh.idx_of(target).ok_or(Error::Unsupported("branch target", target))?;
                }
            }
        }
    }
    Ok(info)
}

/// Runs only the scalar part of a shader (uniform for the whole draw).
pub fn run_scalar(code: &[u32], lane: &mut Lane, ok: MemCheck) -> Result<ScalarInfo, Error> {
    let sh = CompiledShader::new(code);
    run_scalar_compiled(&sh, lane, ok)
}

/// Runs a pre-decoded pixel shader for one pixel on the CPU and records the instructions it
/// executes (`pcs`, in order) together with the scalar slot values and texture descriptors.
pub fn trace_ps_compiled(sh: &CompiledShader, lane: &mut Lane, attrs: &[[f32; 4]; 8], ok: MemCheck) -> Result<(Vec<u32>, ScalarInfo), Error> {
    let insts = &sh.insts;
    let end_pc = sh.end_pc;
    let mut info = ScalarInfo::default();
    let mut pcs = Vec::with_capacity(insts.len());
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
                let len = sh.slot_lens[i] as usize;
                for &c in &sh.slots[i][..len] {
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
                    i = sh.idx_of(target).ok_or(Error::Unsupported("branch target", target))?;
                }
            }
        }
    }
    Ok((pcs, info))
}

/// Runs a pixel shader for one pixel on the CPU and records the instructions it executes
/// (`pcs`, in order) together with the scalar slot values and texture descriptors along that
/// path. `attrs` are the interpolated attributes; texture reads return zero.
pub fn trace_ps(code: &[u32], lane: &mut Lane, attrs: &[[f32; 4]; 8], ok: MemCheck) -> Result<(Vec<u32>, ScalarInfo), Error> {
    let sh = CompiledShader::new(code);
    trace_ps_compiled(&sh, lane, attrs, ok)
}

// ── memory writes and image access (compute shaders run on the CPU) ─────────────────────

/// Bytes per element, component count and component kind of a GFX10 unified format.
#[derive(Clone, Copy, PartialEq)]
enum Comp {
    Unorm8,
    Uint8,
    Unorm16,
    Uint16,
    F16,
    Raw32,
}

fn format_info(fmt: u32) -> Option<(usize, usize, Comp)> {
    Some(match fmt {
        1 | 2 => (1, 1, Comp::Unorm8),
        5 | 6 => (1, 1, Comp::Uint8),
        7 | 8 => (2, 1, Comp::Unorm16),
        11 | 12 => (2, 1, Comp::Uint16),
        13 => (2, 1, Comp::F16),
        14 | 15 => (2, 2, Comp::Unorm8),
        18 | 19 => (2, 2, Comp::Uint8),
        20 | 21 | 22 => (4, 1, Comp::Raw32),
        23 | 24 => (4, 2, Comp::Unorm16),
        27 | 28 => (4, 2, Comp::Uint16),
        29 => (4, 2, Comp::F16),
        56 | 57 => (4, 4, Comp::Unorm8),
        60 | 61 => (4, 4, Comp::Uint8),
        64 | 65 | 66 => (8, 2, Comp::Raw32),
        69 | 70 => (8, 4, Comp::Uint16),
        71 => (8, 4, Comp::F16),
        72 | 73 | 74 => (12, 3, Comp::Raw32),
        75 | 76 | 77 => (16, 4, Comp::Raw32),
        _ => return None,
    })
}

/// Packs register values into one element of `fmt` (little endian), `bytes` long.
fn pack_element(vals: &[u32; 4], fmt: u32) -> Option<([u8; 16], usize)> {
    let (bpe, n, kind) = format_info(fmt)?;
    let mut out = [0u8; 16];
    let csz = bpe / n;
    for k in 0..n {
        let v = vals[k];
        match kind {
            Comp::Unorm8 => out[k] = (f(v).clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            Comp::Uint8 => out[k] = v as u8,
            Comp::Unorm16 => out[k * 2..k * 2 + 2].copy_from_slice(&((f(v).clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).to_le_bytes()),
            Comp::Uint16 => out[k * 2..k * 2 + 2].copy_from_slice(&(v as u16).to_le_bytes()),
            Comp::F16 => out[k * 2..k * 2 + 2].copy_from_slice(&half::f16::from_f32(f(v)).to_bits().to_le_bytes()),
            Comp::Raw32 => out[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes()),
        }
        let _ = csz;
    }
    Some((out, bpe))
}

/// Unpacks one element of `fmt` into register values (missing components: 0, 0, 0, 1).
fn unpack_element(b: &[u8], fmt: u32) -> Option<[u32; 4]> {
    let (_, n, kind) = format_info(fmt)?;
    let mut out = [0, 0, 0, u(1.0)];
    if matches!(kind, Comp::Uint8 | Comp::Uint16 | Comp::Raw32) {
        out[3] = 1;
    }
    for k in 0..n {
        out[k] = match kind {
            Comp::Unorm8 => u(b[k] as f32 / 255.0),
            Comp::Uint8 => b[k] as u32,
            Comp::Unorm16 => u(u16::from_le_bytes([b[k * 2], b[k * 2 + 1]]) as f32 / 65535.0),
            Comp::Uint16 => u16::from_le_bytes([b[k * 2], b[k * 2 + 1]]) as u32,
            Comp::F16 => u(half::f16::from_bits(u16::from_le_bytes([b[k * 2], b[k * 2 + 1]])).to_f32()),
            Comp::Raw32 => u32::from_le_bytes([b[k * 4], b[k * 4 + 1], b[k * 4 + 2], b[k * 4 + 3]]),
        };
    }
    Some(out)
}

fn write_bytes(addr: u64, data: &[u8], ok: MemCheck) -> Result<(), Error> {
    if !ok(addr as usize, data.len()) {
        return Err(Error::Memory(addr));
    }
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len()) };
    Ok(())
}

/// Byte address of texel (x, y, slice) of the image a T# describes.
fn image_texel_addr(t: &[u32], x: u32, y: u32, slice: u32) -> Option<(u64, u32)> {
    let base = ((t[0] as u64) | (((t[1] & 0xFF) as u64) << 32)) << 8;
    let width = ((t[1] >> 30) | ((t[2] & 0xFFF) << 2)) + 1;
    let height = ((t[2] >> 14) & 0x3FFF) + 1;
    let fmt = (t[1] >> 20) & 0x1FF;
    let swiz = (t[3] >> 20) & 0x1F;
    if x >= width || y >= height {
        return None;
    }
    let (bpe, _, _) = format_info(fmt)?;
    let off = crate::tiling::element_offset(x as usize, y as usize, slice as usize, width as usize, height as usize, bpe, swiz)?;
    Some((base + off as u64, fmt))
}

/// image_load / image_store for one lane.
fn image_op(inst: &Inst, l: &mut Lane, ok: MemCheck) -> Result<(), Error> {
    let w0 = inst.w0();
    let w1 = inst.w1();
    let dmask = (w0 >> 8) & 0xF;
    let dim = (w0 >> 3) & 7;
    let vdata = ((w1 >> 8) & 0xFF) as usize;
    let srsrc = (((w1 >> 16) & 0x1F) * 4) as usize;
    // Address VGPRs: vaddr, then (NSA) one byte per extra address.
    let mut addrs = vec![(w1 & 0xFF) as usize];
    for w in inst.words.iter().skip(2) {
        for k in 0..4 {
            addrs.push(((w >> (k * 8)) & 0xFF) as usize);
        }
    }
    let coord = |i: usize| -> u32 {
        let r = if addrs.len() > 1 { addrs.get(i).copied().unwrap_or(0) } else { addrs[0] + i };
        l.v[r]
    };
    let (x, y, slice) = match dim {
        0 => (coord(0), 0, 0),
        4 => (coord(0), 0, coord(1)),
        1 | 6 => (coord(0), coord(1), 0),
        _ => (coord(0), coord(1), coord(2)),
    };
    let t: [u32; 8] = std::array::from_fn(|k| l.s[srsrc + k]);
    let Some((addr, fmt)) = image_texel_addr(&t, x, y, slice) else { return Ok(()) };
    let n = inst.name;
    if n.starts_with("image_store") {
        if !l.stores {
            return Ok(());
        }
        let mut vals = [0u32; 4];
        let mut r = vdata;
        for k in 0..4 {
            if dmask & (1 << k) != 0 {
                vals[k] = l.v[r];
                r += 1;
            }
        }
        let Some((bytes, bpe)) = pack_element(&vals, fmt) else { return Err(Error::Unsupported("image format", fmt)) };
        write_bytes(addr, &bytes[..bpe], ok)
    } else if n.starts_with("image_load") {
        let (bpe, _, _) = format_info(fmt).ok_or(Error::Unsupported("image format", fmt))?;
        if !ok(addr as usize, bpe) {
            return Err(Error::Memory(addr));
        }
        let b = unsafe { std::slice::from_raw_parts(addr as *const u8, bpe) };
        let vals = unpack_element(b, fmt).ok_or(Error::Unsupported("image format", fmt))?;
        let mut r = vdata;
        for k in 0..4 {
            if dmask & (1 << k) != 0 {
                l.v[r] = vals[k];
                r += 1;
            }
        }
        Ok(())
    } else {
        Err(Error::Unsupported(n, inst.pc))
    }
}

// ── Scalar-hoisted pre-decoded VS fast path ─────────────────────────────────────────────

#[derive(Clone, Copy)]
pub enum VsSrc {
    V(u8),
    C(u32),
}

impl VsSrc {
    #[inline(always)]
    fn get(self, v: &[u32; 256]) -> u32 {
        match self {
            VsSrc::V(r) => v[r as usize],
            VsSrc::C(c) => c,
        }
    }
    #[inline(always)]
    fn get_f(self, v: &[u32; 256]) -> f32 {
        f32::from_bits(self.get(v))
    }
}

#[derive(Clone, Copy)]
pub enum VsFastOp {
    Mov { dst: u8, a: VsSrc },
    AddF32 { dst: u8, a: VsSrc, b: VsSrc },
    SubF32 { dst: u8, a: VsSrc, b: VsSrc },
    SubRevF32 { dst: u8, a: VsSrc, b: VsSrc },
    MulF32 { dst: u8, a: VsSrc, b: VsSrc },
    MacF32 { dst: u8, a: VsSrc, b: VsSrc },
    MadF32 { dst: u8, a: VsSrc, b: VsSrc, c: VsSrc },
    FmaF32 { dst: u8, a: VsSrc, b: VsSrc, c: VsSrc },
    MadF32Mod { dst: u8, a: VsSrc, b: VsSrc, c: VsSrc, abs: u8, neg: u8, omod: u8, clamp: bool },
    VopF32Mod { op: u8, dst: u8, a: VsSrc, b: VsSrc, abs: u8, neg: u8, omod: u8, clamp: bool },
    MaxF32 { dst: u8, a: VsSrc, b: VsSrc },
    MinF32 { dst: u8, a: VsSrc, b: VsSrc },
    RcpF32 { dst: u8, a: VsSrc },
    RsqF32 { dst: u8, a: VsSrc },
    SqrtF32 { dst: u8, a: VsSrc },
    FloorF32 { dst: u8, a: VsSrc },
    FractF32 { dst: u8, a: VsSrc },
    CvtF32U32 { dst: u8, a: VsSrc },
    CvtF32I32 { dst: u8, a: VsSrc },
    CvtU32F32 { dst: u8, a: VsSrc },
    CvtI32F32 { dst: u8, a: VsSrc },
    AndB32 { dst: u8, a: VsSrc, b: VsSrc },
    OrB32 { dst: u8, a: VsSrc, b: VsSrc },
    XorB32 { dst: u8, a: VsSrc, b: VsSrc },
    LshlRevB32 { dst: u8, a: VsSrc, b: VsSrc },
    LshrRevB32 { dst: u8, a: VsSrc, b: VsSrc },
    AddU32 { dst: u8, a: VsSrc, b: VsSrc },
    SubU32 { dst: u8, a: VsSrc, b: VsSrc },
    MulLoU32 { dst: u8, a: VsSrc, b: VsSrc },
    BfeU32 { dst: u8, a: VsSrc, b: VsSrc, c: VsSrc },
    SdwaF32 { op: u8, dst: u8, a: VsSrc, b: VsSrc, s: u32 },
    BufLoadFmt { base: u64, stride: u64, fmt: u32, vaddr: u8, vdata: u8, comps: u8 },
    BufLoadDw { base: u64, stride: u64, vaddr: u8, vdata: u8, cnt: u8 },
    Exp { target: u8, compr: bool, regs: [u8; 4] },
}

pub struct VsDrawPlan {
    pub ops: Vec<VsFastOp>,
    pub max_vgpr: usize,
}

#[inline(always)]
fn apply_fmod(mut v: f32, abs: bool, neg: bool) -> f32 {
    if abs {
        v = v.abs();
    }
    if neg {
        v = -v;
    }
    v
}

#[inline(always)]
fn apply_omod_clamp(mut v: f32, omod: u8, clamp: bool) -> f32 {
    match omod {
        1 => v *= 2.0,
        2 => v *= 4.0,
        3 => v *= 0.5,
        _ => {}
    }
    if clamp {
        v = v.clamp(0.0, 1.0);
    }
    v
}

#[inline(always)]
fn always_ok(_: usize, _: usize) -> bool {
    true
}

/// Evaluates all scalar/SMEM instructions of a branch-free VS once per draw, folding scalar
/// operands into constants and pre-validating vertex buffer ranges.
pub fn hoist_vs_draw(sh: &CompiledShader, vs_ud: &[u32; 24], max_ix: u32, ok: MemCheck) -> Option<VsDrawPlan> {
    if !sh.can_hoist_scalar {
        return None;
    }
    let mut lane = Lane::new();
    lane.s[3] = 0x101;
    lane.s[8..32].copy_from_slice(vs_ud);
    let mut ops = Vec::with_capacity(sh.insts.len() / 2);
    let mut max_vgpr = 9usize; // v[5], v[8]

    let resolve = |code: u16, lit: Option<u32>, l: &Lane| -> VsSrc {
        if code >= 256 && code <= 511 {
            VsSrc::V((code - 256) as u8)
        } else {
            VsSrc::C(l.src(code, lit))
        }
    };

    let mut ip = 0usize;
    let mut steps = 0usize;
    let max_steps = sh.insts.len().saturating_mul(8).max(64);
    while ip < sh.insts.len() {
        steps += 1;
        if steps > max_steps {
            return None;
        }
        let inst = &sh.insts[ip];
        ip += 1;
        let w0 = inst.w0();
        let w1 = inst.w1();
        let lit = inst.literal;
        match inst.fmt {
            Fmt::Sop1 | Fmt::Sop2 | Fmt::Sopc | Fmt::Sopk | Fmt::Smem => {
                step(inst, &mut lane, ok).ok()?;
                continue;
            }
            Fmt::Sopp => {
                match step(inst, &mut lane, ok).ok()? {
                    Flow::Next => {}
                    Flow::End => break,
                    Flow::Branch(rel) => {
                        let next_pc = (inst.pc as i64) + 4 + (rel as i64) * 4;
                        if next_pc < 0 {
                            return None;
                        }
                        ip = sh.idx_of(next_pc as u32)?;
                    }
                }
                continue;
            }
            Fmt::Exp => {
                let target = ((w0 >> 4) & 0x3F) as u8;
                if target == 20 || !lane.exec_on() {
                    continue;
                }
                let compr = w0 & (1 << 10) != 0;
                let regs = [(w1 & 0xFF) as u8, ((w1 >> 8) & 0xFF) as u8, ((w1 >> 16) & 0xFF) as u8, ((w1 >> 24) & 0xFF) as u8];
                ops.push(VsFastOp::Exp { target, compr, regs });
            }
            Fmt::Mubuf | Fmt::Mtbuf => {
                if !lane.exec_on() {
                    continue;
                }
                let inst_off = (w0 & 0xFFF) as u64;
                let offen = w0 & (1 << 12) != 0;
                let idxen = w0 & (1 << 13) != 0;
                if !idxen || offen {
                    return None;
                }
                let vaddr = (w1 & 0xFF) as u8;
                let vdata = ((w1 >> 8) & 0xFF) as u8;
                let srsrc = (((w1 >> 16) & 0x1F) * 4) as usize;
                let soff = lane.src(((w1 >> 24) & 0xFF) as u16, None) as u64;
                let (base, stride, vfmt) = vsharp(&lane, srsrc);
                let base_addr = base.wrapping_add(soff).wrapping_add(inst_off);
                let fmt = if inst.fmt == Fmt::Mtbuf { (w0 >> 19) & 0x7F } else { vfmt };
                let need = (max_ix as usize + 1).saturating_mul(stride as usize).saturating_add(16);
                if !ok(base_addr as usize, need) {
                    return None;
                }
                let n = inst.name;
                if n.contains("load_format") {
                    let comps = if n.contains("xyzw") { 4 } else if n.contains("xyz") { 3 } else if n.contains("xy") { 2 } else { 1 };
                    max_vgpr = max_vgpr.max(vdata as usize + comps as usize);
                    ops.push(VsFastOp::BufLoadFmt { base: base_addr, stride, fmt, vaddr, vdata, comps });
                } else if n.starts_with("buffer_load_dword") {
                    let cnt = if n.ends_with("x4") { 4 } else if n.ends_with("x3") { 3 } else if n.ends_with("x2") { 2 } else { 1 };
                    max_vgpr = max_vgpr.max(vdata as usize + cnt as usize);
                    ops.push(VsFastOp::BufLoadDw { base: base_addr, stride, vaddr, vdata, cnt });
                } else {
                    return None;
                }
            }
            Fmt::Vop1 | Fmt::Vop2 | Fmt::Vop3 => {
                if !lane.exec_on() {
                    continue;
                }
                let n = inst.name.trim_end_matches("_e32").trim_end_matches("_e64").trim_end_matches("_sdwa");
                if inst.ext == Ext::Sdwa {
                    let s = inst.words[1];
                    let dst = ((w0 >> 17) & 0xFF) as u8;
                    max_vgpr = max_vgpr.max(dst as usize + 1);
                    let src0 = s & 0xFF;
                    let a = if s & (1 << 23) != 0 { VsSrc::C(lane.src(src0 as u16, None)) } else { VsSrc::V(src0 as u8) };
                    let src1 = (w0 >> 9) & 0xFF;
                    let b = if inst.fmt == Fmt::Vop1 {
                        VsSrc::C(0)
                    } else if s & (1 << 31) != 0 {
                        VsSrc::C(lane.src(src1 as u16, None))
                    } else {
                        VsSrc::V(src1 as u8)
                    };
                    let op = match n {
                        "v_add_f32" => 0,
                        "v_sub_f32" => 1,
                        "v_subrev_f32" => 2,
                        "v_mul_f32" => 3,
                        "v_max_f32" => 4,
                        "v_min_f32" => 5,
                        "v_mov_b32" => 6,
                        _ => return None,
                    };
                    ops.push(VsFastOp::SdwaF32 { op, dst, a, b, s });
                    continue;
                }
                let (a, b, c, dst, vop3) = match inst.fmt {
                    Fmt::Vop1 => (resolve((w0 & 0x1FF) as u16, lit, &lane), VsSrc::C(0), VsSrc::C(0), ((w0 >> 17) & 0xFF) as u8, false),
                    Fmt::Vop2 => {
                        let v1 = VsSrc::V(((w0 >> 9) & 0xFF) as u8);
                        let d = ((w0 >> 17) & 0xFF) as u8;
                        let s0 = resolve((w0 & 0x1FF) as u16, lit, &lane);
                        match n {
                            "v_madmk_f32" | "v_fmamk_f32" => (s0, VsSrc::C(lit.unwrap_or(0)), v1, d, false),
                            "v_madak_f32" | "v_fmaak_f32" => (s0, v1, VsSrc::C(lit.unwrap_or(0)), d, false),
                            _ => (s0, v1, VsSrc::V(d), d, false),
                        }
                    }
                    _ => (
                        resolve((w1 & 0x1FF) as u16, lit, &lane),
                        resolve(((w1 >> 9) & 0x1FF) as u16, lit, &lane),
                        resolve(((w1 >> 18) & 0x1FF) as u16, lit, &lane),
                        (w0 & 0xFF) as u8,
                        true,
                    ),
                };
                max_vgpr = max_vgpr.max(dst as usize + 1);
                let (abs, neg, omod, clamp) = if vop3 {
                    (((w0 >> 8) & 7) as u8, ((w1 >> 29) & 7) as u8, ((w1 >> 27) & 3) as u8, w0 & (1 << 15) != 0)
                } else {
                    (0, 0, 0, false)
                };
                let has_mod = abs != 0 || neg != 0 || omod != 0 || clamp;
                let op = match n {
                    "v_cndmask_b32" => {
                        let mask_bit = if vop3 {
                            let s2 = ((w1 >> 18) & 0x1FF) as usize;
                            if s2 < 128 { lane.s[s2] & 1 != 0 } else { return None }
                        } else {
                            lane.s[VCC] & 1 != 0
                        };
                        let chosen = if mask_bit { b } else { a };
                        if has_mod {
                            let m_idx = if mask_bit { 1 } else { 0 };
                            VsFastOp::VopF32Mod { op: 6, dst, a: chosen, b: VsSrc::C(0), abs: (abs >> m_idx) & 1, neg: (neg >> m_idx) & 1, omod, clamp }
                        } else {
                            VsFastOp::Mov { dst, a: chosen }
                        }
                    }
                    "v_mov_b32" if !has_mod => VsFastOp::Mov { dst, a },
                    "v_add_f32" if !has_mod => VsFastOp::AddF32 { dst, a, b },
                    "v_sub_f32" if !has_mod => VsFastOp::SubF32 { dst, a, b },
                    "v_subrev_f32" if !has_mod => VsFastOp::SubRevF32 { dst, a, b },
                    "v_mul_f32" | "v_mul_legacy_f32" if !has_mod => VsFastOp::MulF32 { dst, a, b },
                    "v_mac_f32" | "v_fmac_f32" if !has_mod => VsFastOp::MacF32 { dst, a, b },
                    "v_mad_f32" | "v_madmk_f32" | "v_madak_f32" if !has_mod => VsFastOp::MadF32 { dst, a, b, c },
                    "v_fma_f32" | "v_fmamk_f32" | "v_fmaak_f32" if !has_mod => VsFastOp::FmaF32 { dst, a, b, c },
                    "v_mad_f32" | "v_fma_f32" => VsFastOp::MadF32Mod { dst, a, b, c, abs, neg, omod, clamp },
                    "v_add_f32" => VsFastOp::VopF32Mod { op: 0, dst, a, b, abs, neg, omod, clamp },
                    "v_sub_f32" => VsFastOp::VopF32Mod { op: 1, dst, a, b, abs, neg, omod, clamp },
                    "v_subrev_f32" => VsFastOp::VopF32Mod { op: 2, dst, a, b, abs, neg, omod, clamp },
                    "v_mul_f32" | "v_mul_legacy_f32" => VsFastOp::VopF32Mod { op: 3, dst, a, b, abs, neg, omod, clamp },
                    "v_max_f32" if !has_mod => VsFastOp::MaxF32 { dst, a, b },
                    "v_min_f32" if !has_mod => VsFastOp::MinF32 { dst, a, b },
                    "v_max_f32" => VsFastOp::VopF32Mod { op: 4, dst, a, b, abs, neg, omod, clamp },
                    "v_min_f32" => VsFastOp::VopF32Mod { op: 5, dst, a, b, abs, neg, omod, clamp },
                    "v_rcp_f32" if !has_mod => VsFastOp::RcpF32 { dst, a },
                    "v_rsq_f32" if !has_mod => VsFastOp::RsqF32 { dst, a },
                    "v_sqrt_f32" if !has_mod => VsFastOp::SqrtF32 { dst, a },
                    "v_floor_f32" if !has_mod => VsFastOp::FloorF32 { dst, a },
                    "v_fract_f32" if !has_mod => VsFastOp::FractF32 { dst, a },
                    "v_cvt_f32_u32" if !has_mod => VsFastOp::CvtF32U32 { dst, a },
                    "v_cvt_f32_i32" if !has_mod => VsFastOp::CvtF32I32 { dst, a },
                    "v_cvt_u32_f32" if !has_mod => VsFastOp::CvtU32F32 { dst, a },
                    "v_cvt_i32_f32" if !has_mod => VsFastOp::CvtI32F32 { dst, a },
                    "v_and_b32" if !has_mod => VsFastOp::AndB32 { dst, a, b },
                    "v_or_b32" if !has_mod => VsFastOp::OrB32 { dst, a, b },
                    "v_xor_b32" if !has_mod => VsFastOp::XorB32 { dst, a, b },
                    "v_lshlrev_b32" if !has_mod => VsFastOp::LshlRevB32 { dst, a, b },
                    "v_lshrrev_b32" if !has_mod => VsFastOp::LshrRevB32 { dst, a, b },
                    "v_add_nc_u32" | "v_add_u32" if !has_mod => VsFastOp::AddU32 { dst, a, b },
                    "v_sub_nc_u32" | "v_sub_u32" if !has_mod => VsFastOp::SubU32 { dst, a, b },
                    "v_mul_lo_u32" if !has_mod => VsFastOp::MulLoU32 { dst, a, b },
                    "v_bfe_u32" if !has_mod => VsFastOp::BfeU32 { dst, a, b, c },
                    _ => return None,
                };
                ops.push(op);
            }
            _ => return None,
        }
    }
    Some(VsDrawPlan { ops, max_vgpr: max_vgpr.min(256) })
}

/// Evaluates one vertex using a scalar-hoisted `VsDrawPlan` (zero SMEM, zero VirtualQuery,
/// zero string matching). Reuses caller-provided `v` and clears only `0..plan.max_vgpr`.
pub fn eval_vs_hoisted(plan: &VsDrawPlan, v: &mut [u32; 256], ix: u32, cntl: &[u32; 8]) -> Option<([f32; 4], [[f32; 4]; 8])> {
    v[..plan.max_vgpr].fill(0);
    v[5] = ix;
    let mut pos = None;
    let mut attr = [[0f32; 4]; 8];
    for &op in &plan.ops {
        match op {
            VsFastOp::Mov { dst, a } => v[dst as usize] = a.get(v),
            VsFastOp::AddF32 { dst, a, b } => v[dst as usize] = (a.get_f(v) + b.get_f(v)).to_bits(),
            VsFastOp::SubF32 { dst, a, b } => v[dst as usize] = (a.get_f(v) - b.get_f(v)).to_bits(),
            VsFastOp::SubRevF32 { dst, a, b } => v[dst as usize] = (b.get_f(v) - a.get_f(v)).to_bits(),
            VsFastOp::MulF32 { dst, a, b } => v[dst as usize] = (a.get_f(v) * b.get_f(v)).to_bits(),
            VsFastOp::MacF32 { dst, a, b } => {
                let d = f32::from_bits(v[dst as usize]);
                v[dst as usize] = (a.get_f(v) * b.get_f(v) + d).to_bits();
            }
            VsFastOp::MadF32 { dst, a, b, c } => v[dst as usize] = (a.get_f(v) * b.get_f(v) + c.get_f(v)).to_bits(),
            VsFastOp::FmaF32 { dst, a, b, c } => v[dst as usize] = a.get_f(v).mul_add(b.get_f(v), c.get_f(v)).to_bits(),
            VsFastOp::MadF32Mod { dst, a, b, c, abs, neg, omod, clamp } => {
                let fa = apply_fmod(a.get_f(v), abs & 1 != 0, neg & 1 != 0);
                let fb = apply_fmod(b.get_f(v), abs & 2 != 0, neg & 2 != 0);
                let fc = apply_fmod(c.get_f(v), abs & 4 != 0, neg & 4 != 0);
                v[dst as usize] = apply_omod_clamp(fa * fb + fc, omod, clamp).to_bits();
            }
            VsFastOp::VopF32Mod { op, dst, a, b, abs, neg, omod, clamp } => {
                let fa = apply_fmod(a.get_f(v), abs & 1 != 0, neg & 1 != 0);
                let fb = apply_fmod(b.get_f(v), abs & 2 != 0, neg & 2 != 0);
                let r = match op {
                    0 => fa + fb,
                    1 => fa - fb,
                    2 => fb - fa,
                    3 => fa * fb,
                    4 => fa.max(fb),
                    5 => fa.min(fb),
                    _ => fa,
                };
                v[dst as usize] = apply_omod_clamp(r, omod, clamp).to_bits();
            }
            VsFastOp::MaxF32 { dst, a, b } => v[dst as usize] = a.get_f(v).max(b.get_f(v)).to_bits(),
            VsFastOp::MinF32 { dst, a, b } => v[dst as usize] = a.get_f(v).min(b.get_f(v)).to_bits(),
            VsFastOp::RcpF32 { dst, a } => v[dst as usize] = (1.0f32 / a.get_f(v)).to_bits(),
            VsFastOp::RsqF32 { dst, a } => v[dst as usize] = (1.0f32 / a.get_f(v).sqrt()).to_bits(),
            VsFastOp::SqrtF32 { dst, a } => v[dst as usize] = a.get_f(v).sqrt().to_bits(),
            VsFastOp::FloorF32 { dst, a } => v[dst as usize] = a.get_f(v).floor().to_bits(),
            VsFastOp::FractF32 { dst, a } => {
                let x = a.get_f(v);
                v[dst as usize] = (x - x.floor()).to_bits();
            }
            VsFastOp::CvtF32U32 { dst, a } => v[dst as usize] = (a.get(v) as f32).to_bits(),
            VsFastOp::CvtF32I32 { dst, a } => v[dst as usize] = (a.get(v) as i32 as f32).to_bits(),
            VsFastOp::CvtU32F32 { dst, a } => v[dst as usize] = a.get_f(v).clamp(0.0, u32::MAX as f32) as u32,
            VsFastOp::CvtI32F32 { dst, a } => v[dst as usize] = a.get_f(v).clamp(i32::MIN as f32, i32::MAX as f32) as i32 as u32,
            VsFastOp::AndB32 { dst, a, b } => v[dst as usize] = a.get(v) & b.get(v),
            VsFastOp::OrB32 { dst, a, b } => v[dst as usize] = a.get(v) | b.get(v),
            VsFastOp::XorB32 { dst, a, b } => v[dst as usize] = a.get(v) ^ b.get(v),
            VsFastOp::LshlRevB32 { dst, a, b } => v[dst as usize] = b.get(v) << (a.get(v) & 31),
            VsFastOp::LshrRevB32 { dst, a, b } => v[dst as usize] = b.get(v) >> (a.get(v) & 31),
            VsFastOp::AddU32 { dst, a, b } => v[dst as usize] = a.get(v).wrapping_add(b.get(v)),
            VsFastOp::SubU32 { dst, a, b } => v[dst as usize] = a.get(v).wrapping_sub(b.get(v)),
            VsFastOp::MulLoU32 { dst, a, b } => v[dst as usize] = a.get(v).wrapping_mul(b.get(v)),
            VsFastOp::BfeU32 { dst, a, b, c } => v[dst as usize] = bfe(a.get(v) as u64, b.get(v) & 31, c.get(v) & 31) as u32,
            VsFastOp::SdwaF32 { op, dst, a, b, s } => {
                let mut ra = sdwa_sel(a.get(v), (s >> 16) & 7, s & (1 << 19) != 0);
                if s & (1 << 21) != 0 { ra &= 0x7FFF_FFFF; }
                if s & (1 << 20) != 0 { ra ^= 0x8000_0000; }
                let mut rb = sdwa_sel(b.get(v), (s >> 24) & 7, s & (1 << 27) != 0);
                if s & (1 << 29) != 0 { rb &= 0x7FFF_FFFF; }
                if s & (1 << 28) != 0 { rb ^= 0x8000_0000; }
                let (fa, fb) = (f32::from_bits(ra), f32::from_bits(rb));
                let r = match op {
                    0 => fa + fb,
                    1 => fa - fb,
                    2 => fb - fa,
                    3 => fa * fb,
                    4 => fa.max(fb),
                    5 => fa.min(fb),
                    _ => fa,
                };
                let mut bits = apply_omod_clamp(r, ((s >> 14) & 3) as u8, s & (1 << 13) != 0).to_bits();
                let dsel = (s >> 8) & 7;
                let unused = (s >> 11) & 3;
                let old = v[dst as usize];
                bits = match dsel {
                    0..=3 => {
                        let sh = dsel * 8;
                        let keep = if unused == 2 { old & !(0xFF << sh) } else { 0 };
                        keep | ((bits & 0xFF) << sh)
                    }
                    4 | 5 => {
                        let sh = (dsel - 4) * 16;
                        let keep = if unused == 2 { old & !(0xFFFF << sh) } else { 0 };
                        keep | ((bits & 0xFFFF) << sh)
                    }
                    _ => bits,
                };
                v[dst as usize] = bits;
            }
            VsFastOp::BufLoadFmt { base, stride, fmt, vaddr, vdata, comps } => {
                let addr = base.wrapping_add((v[vaddr as usize] as u64).wrapping_mul(stride));
                let c = load_format(addr, fmt, always_ok).ok()?;
                for k in 0..comps as usize {
                    v[vdata as usize + k] = c[k];
                }
            }
            VsFastOp::BufLoadDw { base, stride, vaddr, vdata, cnt } => {
                let addr = base.wrapping_add((v[vaddr as usize] as u64).wrapping_mul(stride));
                for k in 0..cnt as usize {
                    v[vdata as usize + k] = unsafe { ((addr + k as u64 * 4) as *const u32).read_unaligned() };
                }
            }
            VsFastOp::Exp { target, compr, regs } => {
                let value = if compr {
                    let lo = v[regs[0] as usize];
                    let hi = v[if regs[1] == regs[0] { regs[2] } else { regs[1] } as usize];
                    let h = |x: u32| half::f16::from_bits(x as u16).to_f32();
                    [h(lo), h(lo >> 16), h(hi), h(hi >> 16)]
                } else {
                    [
                        f32::from_bits(v[regs[0] as usize]),
                        f32::from_bits(v[regs[1] as usize]),
                        f32::from_bits(v[regs[2] as usize]),
                        f32::from_bits(v[regs[3] as usize]),
                    ]
                };
                if target == 12 {
                    pos = Some(value);
                } else if (32..96).contains(&target) {
                    let p = (target - 32) as u32;
                    for n in 0..8 {
                        if cntl[n] == p {
                            attr[n] = value;
                        }
                    }
                }
            }
        }
    }
    Some((pos?, attr))
}
