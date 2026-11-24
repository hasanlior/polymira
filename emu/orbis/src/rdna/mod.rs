//! PS5 GPU (gfx1013) shader instruction decoder — the front end of Orbis' shader recompiler.
//!
//! PS5 shaders are native RDNA2 machine code. This module splits a shader into instructions
//! (format, opcode, length, raw words) and gives typed access to their operand fields. The
//! translator to SPIR-V builds on it.

mod ops;
pub mod interp;
pub mod ps;
pub mod vs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fmt {
    Sop2,
    Sopk,
    Sop1,
    Sopc,
    Sopp,
    Smem,
    Vop2,
    Vop1,
    Vopc,
    Vop3,
    Vop3p,
    Vintrp,
    Ds,
    Mubuf,
    Mtbuf,
    Mimg,
    Exp,
    Flat,
    Global,
    Scratch,
}

/// Extra encoding of a VOP1/VOP2/VOPC instruction (src0 field values 0xF9, 0xFA, 0xE9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ext {
    None,
    Sdwa,
    Dpp16,
    Dpp8,
}

#[derive(Clone, Debug)]
pub struct Inst {
    /// Byte offset in the shader.
    pub pc: u32,
    pub fmt: Fmt,
    pub op: u16,
    pub name: &'static str,
    /// The instruction's dwords (including literal / SDWA / DPP / NSA dwords).
    pub words: Vec<u32>,
    /// 32-bit literal constant, when a source operand is 0xFF (or the opcode carries one).
    pub literal: Option<u32>,
    pub ext: Ext,
}

/// Source operand encodings (9-bit for vector sources, 8-bit for scalar ones).
pub const SRC_VCC_LO: u16 = 106;
pub const SRC_VCC_HI: u16 = 107;
pub const SRC_M0: u16 = 124;
pub const SRC_NULL: u16 = 125;
pub const SRC_EXEC_LO: u16 = 126;
pub const SRC_EXEC_HI: u16 = 127;
pub const SRC_ZERO: u16 = 128;
pub const SRC_LITERAL: u16 = 255;
pub const SRC_VGPR0: u16 = 256;

fn bits(w: u32, lo: u32, n: u32) -> u32 {
    (w >> lo) & ((1u32 << n) - 1)
}

impl Inst {
    pub fn w0(&self) -> u32 {
        self.words[0]
    }
    pub fn w1(&self) -> u32 {
        self.words.get(1).copied().unwrap_or(0)
    }
    /// Field of the 64-bit encoding (bits counted across both dwords).
    pub fn field(&self, lo: u32, n: u32) -> u32 {
        let v = self.w0() as u64 | ((self.w1() as u64) << 32);
        ((v >> lo) & ((1u64 << n) - 1)) as u32
    }
}

/// Decodes one instruction at `pc` (byte offset). Returns None past the end of the code.
pub fn decode_at(code: &[u32], pc: u32) -> Option<Inst> {
    let i = (pc / 4) as usize;
    let w0 = *code.get(i)?;
    let w1 = code.get(i + 1).copied().unwrap_or(0);
    let (fmt, op, mut len): (Fmt, u16, usize) = if w0 >> 30 == 0b10 {
        // Scalar ALU and SOPK.
        if w0 >> 23 == 0b1_0111_1101 {
            (Fmt::Sop1, bits(w0, 8, 8) as u16, 1)
        } else if w0 >> 23 == 0b1_0111_1110 {
            (Fmt::Sopc, bits(w0, 16, 7) as u16, 1)
        } else if w0 >> 23 == 0b1_0111_1111 {
            (Fmt::Sopp, bits(w0, 16, 7) as u16, 1)
        } else if w0 >> 28 == 0b1011 {
            (Fmt::Sopk, bits(w0, 23, 5) as u16, 1)
        } else {
            (Fmt::Sop2, bits(w0, 23, 7) as u16, 1)
        }
    } else if w0 >> 31 == 0 {
        // VOP1 / VOPC / VOP2.
        if w0 >> 25 == 0b011_1111 {
            (Fmt::Vop1, bits(w0, 9, 8) as u16, 1)
        } else if w0 >> 25 == 0b011_1110 {
            (Fmt::Vopc, bits(w0, 17, 8) as u16, 1)
        } else {
            (Fmt::Vop2, bits(w0, 25, 6) as u16, 1)
        }
    } else {
        match w0 >> 26 {
            0b11_0101 => (Fmt::Vop3, bits(w0, 16, 10) as u16, 2),
            0b11_0010 => (Fmt::Vintrp, bits(w0, 16, 2) as u16, 1),
            0b11_0110 => (Fmt::Ds, bits(w0, 18, 8) as u16, 2),
            0b11_1000 => (Fmt::Mubuf, (bits(w0, 18, 7) | (bits(w0, 25, 1) << 7)) as u16, 2),
            0b11_1010 => (Fmt::Mtbuf, (bits(w0, 16, 3) | (bits(w1, 21, 1) << 3)) as u16, 2),
            0b11_1100 => (Fmt::Mimg, (bits(w0, 18, 7) | (bits(w0, 0, 1) << 7)) as u16, 2),
            0b11_1110 => (Fmt::Exp, 0, 2),
            0b11_1101 => (Fmt::Smem, bits(w0, 18, 8) as u16, 2),
            0b11_0111 => {
                let f = match bits(w0, 14, 2) {
                    0 => Fmt::Flat,
                    1 => Fmt::Scratch,
                    _ => Fmt::Global,
                };
                (f, bits(w0, 18, 7) as u16, 2)
            }
            _ if w0 >> 24 == 0b1100_1100 => (Fmt::Vop3p, bits(w0, 16, 7) as u16, 2),
            _ => return None,
        }
    };

    // Extra dwords: literal constants, SDWA / DPP, MIMG NSA addresses.
    let mut ext = Ext::None;
    let mut literal_at: Option<usize> = None;
    match fmt {
        Fmt::Sop2 => {
            if bits(w0, 0, 8) == 255 || bits(w0, 8, 8) == 255 {
                literal_at = Some(1);
            }
        }
        Fmt::Sop1 => {
            if bits(w0, 0, 8) == 255 {
                literal_at = Some(1);
            }
        }
        Fmt::Sopc => {
            if bits(w0, 0, 8) == 255 || bits(w0, 8, 8) == 255 {
                literal_at = Some(1);
            }
        }
        Fmt::Sopk => {
            if op == 0x15 {
                literal_at = Some(1); // s_setreg_imm32_b32
            }
        }
        Fmt::Vop1 | Fmt::Vop2 | Fmt::Vopc => {
            match bits(w0, 0, 9) {
                0xF9 => ext = Ext::Sdwa,
                0xFA => ext = Ext::Dpp16,
                0xE9 => ext = Ext::Dpp8,
                255 => literal_at = Some(1),
                _ => {}
            }
            // v_madmk/madak/fmamk/fmaak (f32 and f16) always carry a literal.
            let n = ops::name(fmt, op).unwrap_or("");
            if fmt == Fmt::Vop2 && (n.contains("mk_f") || n.contains("ak_f")) {
                literal_at = Some(1);
            }
            if ext != Ext::None {
                len += 1;
            }
        }
        Fmt::Vop3 | Fmt::Vop3p => {
            if bits(w1, 0, 9) == 255 || bits(w1, 9, 9) == 255 || bits(w1, 18, 9) == 255 {
                literal_at = Some(2);
            }
        }
        Fmt::Mimg => {
            len += bits(w0, 1, 2) as usize; // NSA: extra address dwords
        }
        _ => {}
    }
    if let Some(at) = literal_at {
        len = len.max(at + 1);
    }
    let words: Vec<u32> = (0..len).map(|k| code.get(i + k).copied().unwrap_or(0)).collect();
    let literal = literal_at.map(|at| words[at]);
    let name = ops::name(fmt, op).unwrap_or("?");
    Some(Inst { pc, fmt, op, name, words, literal, ext })
}

/// All instructions up to and including the first s_endpgm.
pub fn decode_shader(code: &[u32]) -> Vec<Inst> {
    let mut out = vec![];
    let mut pc = 0u32;
    while let Some(inst) = decode_at(code, pc) {
        pc += inst.words.len() as u32 * 4;
        let end = inst.fmt == Fmt::Sopp && inst.op == 1; // s_endpgm
        out.push(inst);
        if end {
            break;
        }
    }
    out
}
