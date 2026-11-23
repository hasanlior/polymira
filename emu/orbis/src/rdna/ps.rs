//! PS5 pixel shader → SPIR-V fragment shader.
//!
//! Only the vector part is translated. Everything uniform for a draw (scalar registers,
//! constant-buffer loads, texture descriptors) is computed per draw on the CPU by
//! `interp::run_scalar` and handed to the shader:
//!   * each scalar operand of a vector instruction is a push-constant slot (`slots[i]`),
//!   * each image instruction samples the combined image sampler bound at set 0, binding 0.
//! Interpolated attributes arrive as `vec4 attr[0..8]` (locations 0..7) from the CPU-run
//! vertex shader; the colour export goes to location 0.

use std::collections::HashMap;

use rspirv::dr::{Builder, Operand};
use rspirv::spirv::{self, Word};

use super::interp::scalar_slots;
use super::{decode_shader, Ext, Fmt, Inst};

pub const MAX_SLOTS: usize = 64;
pub const MAX_ATTRS: usize = 8;

/// GLSL.std.450 instruction numbers.
mod glsl {
    pub const ROUND_EVEN: u32 = 2;
    pub const TRUNC: u32 = 3;
    pub const FABS: u32 = 4;
    pub const FLOOR: u32 = 8;
    pub const CEIL: u32 = 9;
    pub const FRACT: u32 = 10;
    pub const SIN: u32 = 13;
    pub const COS: u32 = 14;
    pub const EXP2: u32 = 29;
    pub const LOG2: u32 = 30;
    pub const SQRT: u32 = 31;
    pub const INVERSE_SQRT: u32 = 32;
    pub const FMIN: u32 = 37;
    pub const UMIN: u32 = 38;
    pub const SMIN: u32 = 39;
    pub const FMAX: u32 = 40;
    pub const UMAX: u32 = 41;
    pub const SMAX: u32 = 42;
    pub const FCLAMP: u32 = 43;
    pub const FMA: u32 = 50;
    pub const PACK_HALF_2X16: u32 = 58;
    pub const UNPACK_HALF_2X16: u32 = 62;
}

pub struct Translated {
    pub spirv: Vec<u32>,
    pub slots: usize,
    pub images: usize,
}

struct T {
    b: Builder,
    glsl: Word,
    t_bool: Word,
    t_u32: Word,
    t_f32: Word,
    t_v2f: Word,
    t_v4f: Word,
    p_priv_u32: Word,
    p_pc_u32: Word,
    vregs: Word,
    pc_block: Word,
    attrs: Vec<Word>,
    frag_coord: Word,
    out_color: Word,
    texture: Word,
    t_sampled: Word,
    vcc: Word,
    masks: HashMap<u16, Word>,
    p_priv_bool: Word,
    consts: HashMap<u32, Word>,
}

impl T {
    fn cu(&mut self, v: u32) -> Word {
        if let Some(&c) = self.consts.get(&v) {
            return c;
        }
        let c = self.b.constant_bit32(self.t_u32, v);
        self.consts.insert(v, c);
        c
    }
    fn cf(&mut self, v: f32) -> Word {
        let c = self.cu(v.to_bits());
        self.b.bitcast(self.t_f32, None, c).unwrap()
    }
    fn vptr(&mut self, r: u32) -> Word {
        let idx = self.cu(r);
        self.b.access_chain(self.p_priv_u32, None, self.vregs, [idx]).unwrap()
    }
    fn get_v(&mut self, r: u32) -> Word {
        let p = self.vptr(r);
        self.b.load(self.t_u32, None, p, None, []).unwrap()
    }
    fn set_v(&mut self, r: u32, val: Word) {
        let p = self.vptr(r);
        self.b.store(p, val, None, []).unwrap();
    }
    fn slot(&mut self, i: usize) -> Word {
        let zero = self.cu(0);
        let idx = self.cu(i as u32);
        let p = self.b.access_chain(self.p_pc_u32, None, self.pc_block, [zero, idx]).unwrap();
        self.b.load(self.t_u32, None, p, None, []).unwrap()
    }
    fn f(&mut self, x: Word) -> Word {
        self.b.bitcast(self.t_f32, None, x).unwrap()
    }
    fn u(&mut self, x: Word) -> Word {
        self.b.bitcast(self.t_u32, None, x).unwrap()
    }
    fn ext(&mut self, ty: Word, op: u32, args: &[Word]) -> Word {
        let ops: Vec<Operand> = args.iter().map(|&a| Operand::IdRef(a)).collect();
        self.b.ext_inst(ty, None, self.glsl, op, ops).unwrap()
    }
    fn mask_var(&mut self, r: u16) -> Word {
        if let Some(&v) = self.masks.get(&r) {
            return v;
        }
        let v = self.b.variable(self.p_priv_bool, None, spirv::StorageClass::Private, None);
        self.masks.insert(r, v);
        v
    }
    /// Low 16 bits as half → f32.
    fn half_lo(&mut self, x: Word) -> Word {
        let v = self.ext(self.t_v2f, glsl::UNPACK_HALF_2X16, &[x]);
        self.b.composite_extract(self.t_f32, None, v, [0]).unwrap()
    }
    fn half_hi(&mut self, x: Word) -> Word {
        let v = self.ext(self.t_v2f, glsl::UNPACK_HALF_2X16, &[x]);
        self.b.composite_extract(self.t_f32, None, v, [1]).unwrap()
    }
    fn pack_half(&mut self, lo: Word, hi: Word) -> Word {
        let v = self.b.composite_construct(self.t_v2f, None, [lo, hi]).unwrap();
        self.ext(self.t_u32, glsl::PACK_HALF_2X16, &[v])
    }
}

fn inline_const(code: u16) -> Option<u32> {
    Some(match code {
        128..=192 => (code - 128) as u32,
        193..=208 => (-((code - 192) as i32)) as u32,
        240 => 0.5f32.to_bits(),
        241 => (-0.5f32).to_bits(),
        242 => 1.0f32.to_bits(),
        243 => (-1.0f32).to_bits(),
        244 => 2.0f32.to_bits(),
        245 => (-2.0f32).to_bits(),
        246 => 4.0f32.to_bits(),
        247 => (-4.0f32).to_bits(),
        248 => 0x3E22_F983,
        125 => 0,
        _ => return None,
    })
}

/// Translates a pixel shader. `ps_input_addr` is SPI_PS_INPUT_ADDR (which VGPRs hold what).
pub fn translate(code: &[u32], ps_input_addr: u32) -> Result<Translated, String> {
    let insts = decode_shader(code);
    // Branches are only allowed when they skip to the end of the program ("every pixel was
    // discarded" shortcuts); per-pixel discard is done with OpKill instead.
    let end_pc = insts.last().map(|i| i.pc).unwrap_or(0);
    for i in &insts {
        if i.fmt == Fmt::Sopp && (i.name.starts_with("s_cbranch") || i.name == "s_branch") {
            let rel = (i.w0() & 0xFFFF) as u16 as i16 as i64;
            let target = i.pc as i64 + 4 + rel * 4;
            if target < end_pc as i64 {
                return Err("control flow".into());
            }
        }
    }
    let mut b = Builder::new();
    b.set_version(1, 3);
    b.capability(spirv::Capability::Shader);
    let glsl = b.ext_inst_import("GLSL.std.450");
    b.memory_model(spirv::AddressingModel::Logical, spirv::MemoryModel::GLSL450);
    let t_void = b.type_void();
    let t_bool = b.type_bool();
    let t_u32 = b.type_int(32, 0);
    let t_f32 = b.type_float(32);
    let t_v2f = b.type_vector(t_f32, 2);
    let t_v4f = b.type_vector(t_f32, 4);
    let c256 = b.constant_bit32(t_u32, 256);
    let t_varr = b.type_array(t_u32, c256);
    let p_priv_arr = b.type_pointer(None, spirv::StorageClass::Private, t_varr);
    let p_priv_u32 = b.type_pointer(None, spirv::StorageClass::Private, t_u32);
    let p_priv_bool = b.type_pointer(None, spirv::StorageClass::Private, t_bool);
    let vregs = b.variable(p_priv_arr, None, spirv::StorageClass::Private, None);

    // Push constants: uint slots[MAX_SLOTS]
    let cslots = b.constant_bit32(t_u32, MAX_SLOTS as u32);
    let t_sarr = b.type_array(t_u32, cslots);
    b.decorate(t_sarr, spirv::Decoration::ArrayStride, [Operand::LiteralBit32(4)]);
    let t_pc = b.type_struct([t_sarr]);
    b.decorate(t_pc, spirv::Decoration::Block, []);
    b.member_decorate(t_pc, 0, spirv::Decoration::Offset, [Operand::LiteralBit32(0)]);
    let p_pc = b.type_pointer(None, spirv::StorageClass::PushConstant, t_pc);
    let p_pc_u32 = b.type_pointer(None, spirv::StorageClass::PushConstant, t_u32);
    let pc_block = b.variable(p_pc, None, spirv::StorageClass::PushConstant, None);

    let p_in_v4 = b.type_pointer(None, spirv::StorageClass::Input, t_v4f);
    let mut attrs = vec![];
    for k in 0..MAX_ATTRS {
        let v = b.variable(p_in_v4, None, spirv::StorageClass::Input, None);
        b.decorate(v, spirv::Decoration::Location, [Operand::LiteralBit32(k as u32)]);
        attrs.push(v);
    }
    let frag_coord = b.variable(p_in_v4, None, spirv::StorageClass::Input, None);
    b.decorate(frag_coord, spirv::Decoration::BuiltIn, [Operand::BuiltIn(spirv::BuiltIn::FragCoord)]);
    let p_out_v4 = b.type_pointer(None, spirv::StorageClass::Output, t_v4f);
    let out_color = b.variable(p_out_v4, None, spirv::StorageClass::Output, None);
    b.decorate(out_color, spirv::Decoration::Location, [Operand::LiteralBit32(0)]);

    let t_img = b.type_image(t_f32, spirv::Dim::Dim2D, 0, 0, 0, 1, spirv::ImageFormat::Unknown, None);
    let t_sampled = b.type_sampled_image(t_img);
    let p_tex = b.type_pointer(None, spirv::StorageClass::UniformConstant, t_sampled);
    let texture = b.variable(p_tex, None, spirv::StorageClass::UniformConstant, None);
    b.decorate(texture, spirv::Decoration::DescriptorSet, [Operand::LiteralBit32(0)]);
    b.decorate(texture, spirv::Decoration::Binding, [Operand::LiteralBit32(0)]);
    let vcc = b.variable(p_priv_bool, None, spirv::StorageClass::Private, None);

    let t_fn = b.type_function(t_void, []);
    let mut t = T {
        b,
        glsl,
        t_bool,
        t_u32,
        t_f32,
        t_v2f,
        t_v4f,
        p_priv_u32,
        p_pc_u32,
        vregs,
        pc_block,
        attrs: attrs.clone(),
        frag_coord,
        out_color,
        texture,
        t_sampled,
        vcc,
        masks: HashMap::new(),
        p_priv_bool,
        consts: HashMap::new(),
    };
    // Pre-create mask variables for every SGPR a VOP3 compare may write (globals must exist
    // before the function starts).
    for inst in &insts {
        if inst.fmt == Fmt::Vop3 && inst.name.starts_with("v_cmp") {
            let r = (inst.w0() & 0xFF) as u16;
            t.mask_var(r);
        }
        if inst.fmt == Fmt::Vopc && inst.ext == Ext::Sdwa && inst.words[1] & (1 << 15) != 0 {
            let r = ((inst.words[1] >> 8) & 0x7F) as u16;
            t.mask_var(r);
        }
    }

    let main = t.b.begin_function(t_void, None, spirv::FunctionControl::NONE, t_fn).unwrap();
    t.b.begin_block(None).unwrap();

    // Initial VGPRs from SPI_PS_INPUT_ADDR: barycentrics first (unused here), then position.
    let mut vgpr = 0u32;
    let sizes = [2, 2, 2, 3, 2, 2, 2, 1];
    for (bit, n) in sizes.iter().enumerate() {
        if ps_input_addr & (1 << bit) != 0 {
            vgpr += n;
        }
    }
    let fc = t.b.load(t_v4f, None, frag_coord, None, []).unwrap();
    for k in 0..4u32 {
        if ps_input_addr & (1 << (8 + k)) != 0 {
            let mut c = t.b.composite_extract(t_f32, None, fc, [k]).unwrap();
            if k == 3 {
                let one = t.cf(1.0);
                c = t.b.f_div(t_f32, None, one, c).unwrap();
            }
            let cu = t.u(c);
            t.set_v(vgpr, cu);
            vgpr += 1;
        }
    }

    let mut slot = 0usize;
    let mut images = 0usize;
    let mut exec_saves: Vec<u16> = vec![];
    let mut wrote_color = false;
    for inst in &insts {
        let slots_here = scalar_slots(inst);
        let base = slot;
        slot += slots_here.len();
        if slot > MAX_SLOTS {
            return Err("too many scalar operands".into());
        }
        match inst.fmt {
            Fmt::Sop1 => {
                // s_mov_b64 sX, exec: remember the saved exec mask.
                let w0 = inst.w0();
                if inst.name == "s_mov_b64" && (w0 & 0xFF) == 126 {
                    exec_saves.push(((w0 >> 16) & 0x7F) as u16);
                }
                if inst.name == "s_and_saveexec_b64" {
                    return Err("s_and_saveexec_b64".into());
                }
                continue;
            }
            Fmt::Sop2 => {
                // s_andn2_b64 m, m, vcc|sN with m = exec or a saved exec: discard where the mask is set.
                let w0 = inst.w0();
                let (d, s0, s1) = (((w0 >> 16) & 0x7F) as u16, (w0 & 0xFF) as u16, ((w0 >> 8) & 0xFF) as u16);
                if inst.name == "s_andn2_b64" && d == s0 && (d == 126 || exec_saves.contains(&d)) {
                    let var = if s1 == 106 { Some(t.vcc) } else { t.masks.get(&s1).copied() };
                    let Some(var) = var else { return Err("s_andn2_b64 mask".into()) };
                    let cond = t.b.load(t.t_bool, None, var, None, []).unwrap();
                    let kill = t.b.id();
                    let merge = t.b.id();
                    t.b.selection_merge(merge, spirv::SelectionControl::NONE).unwrap();
                    t.b.branch_conditional(cond, kill, merge, []).unwrap();
                    t.b.begin_block(Some(kill)).unwrap();
                    t.b.kill().unwrap();
                    t.b.begin_block(Some(merge)).unwrap();
                } else if d == 126 || d == 106 {
                    // other writes to exec/vcc would change which pixels run
                    if !(inst.name == "s_wqm_b64") {
                        return Err(format!("{} to exec/vcc", inst.name));
                    }
                }
                continue;
            }
            Fmt::Sopk | Fmt::Sopc | Fmt::Sopp | Fmt::Smem => continue,
            Fmt::Vintrp => {
                let w0 = inst.w0();
                let vdst = (w0 >> 18) & 0xFF;
                let attr = ((w0 >> 10) & 0x3F) as usize;
                let chan = (w0 >> 8) & 3;
                if inst.name.starts_with("v_interp_p1") {
                    continue;
                }
                if attr >= MAX_ATTRS {
                    return Err(format!("attribute {attr}"));
                }
                let a = t.b.load(t_v4f, None, attrs[attr], None, []).unwrap();
                let c = t.b.composite_extract(t_f32, None, a, [chan]).unwrap();
                let cu = t.u(c);
                t.set_v(vdst, cu);
            }
            Fmt::Mimg => {
                let w0 = inst.w0();
                let w1 = inst.w1();
                let dmask = (w0 >> 8) & 0xF;
                let vaddr = w1 & 0xFF;
                let vdata = (w1 >> 8) & 0xFF;
                let n = inst.name;
                if !(n == "image_sample" || n == "image_sample_lz" || n == "image_sample_l" || n == "image_sample_b") {
                    return Err(format!("{n}"));
                }
                if images > 0 {
                    // One texture per draw for now: reuse binding 0.
                }
                images += 1;
                let x = t.get_v(vaddr);
                let x = t.f(x);
                let lod_first = n == "image_sample_l" || n == "image_sample_b";
                let (cx, cy) = if lod_first && n == "image_sample_b" { (vaddr + 1, vaddr + 2) } else { (vaddr, vaddr + 1) };
                let _ = x;
                let xx = t.get_v(cx);
                let xx = t.f(xx);
                let yy = t.get_v(cy);
                let yy = t.f(yy);
                let coord = t.b.composite_construct(t_v2f, None, [xx, yy]).unwrap();
                let si = t.b.load(t.t_sampled, None, t.texture, None, []).unwrap();
                let texel = if n == "image_sample_l" {
                    let lod = t.get_v(vaddr + 2);
                    let lod = t.f(lod);
                    t.b.image_sample_explicit_lod(t_v4f, None, si, coord, spirv::ImageOperands::LOD, [Operand::IdRef(lod)]).unwrap()
                } else if n == "image_sample_lz" {
                    let z = t.cf(0.0);
                    t.b.image_sample_explicit_lod(t_v4f, None, si, coord, spirv::ImageOperands::LOD, [Operand::IdRef(z)]).unwrap()
                } else {
                    t.b.image_sample_implicit_lod(t_v4f, None, si, coord, None, []).unwrap()
                };
                let mut d = vdata;
                for k in 0..4u32 {
                    if dmask & (1 << k) != 0 {
                        let c = t.b.composite_extract(t_f32, None, texel, [k]).unwrap();
                        let cu = t.u(c);
                        t.set_v(d, cu);
                        d += 1;
                    }
                }
            }
            Fmt::Exp => {
                let w0 = inst.w0();
                let w1 = inst.w1();
                let en = w0 & 0xF;
                let target = (w0 >> 4) & 0x3F;
                if target != 0 {
                    continue; // only MRT0 for now
                }
                let compr = w0 & (1 << 10) != 0;
                let regs = [w1 & 0xFF, (w1 >> 8) & 0xFF, (w1 >> 16) & 0xFF, (w1 >> 24) & 0xFF];
                let comps: Vec<Word> = if compr {
                    // Compressed: (R,G) in the first source, (B,A) in the second distinct one
                    // (the assembler repeats each register: "v1, v1, v0, v0").
                    let lo = t.get_v(regs[0]);
                    let hi = t.get_v(if regs[1] == regs[0] { regs[2] } else { regs[1] });
                    let a = t.half_lo(lo);
                    let bb = t.half_hi(lo);
                    let c = t.half_lo(hi);
                    let d = t.half_hi(hi);
                    vec![a, bb, c, d]
                } else {
                    let mut v = vec![];
                    for k in 0..4 {
                        if en & (1 << k) != 0 {
                            let x = t.get_v(regs[k]);
                            v.push(t.f(x));
                        } else {
                            v.push(t.cf(if k == 3 { 1.0 } else { 0.0 }));
                        }
                    }
                    v
                };
                let col = t.b.composite_construct(t_v4f, None, comps).unwrap();
                t.b.store(out_color, col, None, []).unwrap();
                wrote_color = true;
            }
            Fmt::Vop1 | Fmt::Vop2 | Fmt::Vopc | Fmt::Vop3 | Fmt::Vop3p => {
                vop(&mut t, inst, base, &slots_here)?;
            }
            _ => return Err(format!("{}", inst.name)),
        }
    }
    if !wrote_color {
        let z = t.cf(0.0);
        let col = t.b.composite_construct(t_v4f, None, [z, z, z, z]).unwrap();
        t.b.store(out_color, col, None, []).unwrap();
    }
    t.b.ret().unwrap();
    t.b.end_function().unwrap();
    let mut iface = attrs.clone();
    iface.push(frag_coord);
    iface.push(out_color);
    t.b.entry_point(spirv::ExecutionModel::Fragment, main, "main", iface);
    t.b.execution_mode(main, spirv::ExecutionMode::OriginUpperLeft, []);
    let module = t.b.module();
    use rspirv::binary::Assemble;
    Ok(Translated { spirv: module.assemble(), slots: slot, images })
}

/// Reads a vector-instruction source operand (raw 32-bit value).
fn read_src(t: &mut T, code: u16, lit: Option<u32>, slot_of: &mut dyn FnMut(u16) -> Option<usize>) -> Result<Word, String> {
    if code >= 256 {
        return Ok(t.get_v((code - 256) as u32));
    }
    if code == 255 {
        let c = t.cu(lit.unwrap_or(0));
        return Ok(c);
    }
    if let Some(v) = inline_const(code) {
        return Ok(t.cu(v));
    }
    if let Some(s) = slot_of(code) {
        return Ok(t.slot(s));
    }
    Err(format!("source operand {code}"))
}

fn vop(t: &mut T, inst: &Inst, base: usize, slots_here: &[u16]) -> Result<(), String> {
    let w0 = inst.w0();
    let w1 = inst.w1();
    let lit = inst.literal;
    let full = inst.name;
    let n = full.trim_end_matches("_e32").trim_end_matches("_e64").trim_end_matches("_sdwa");
    let mut used = vec![false; slots_here.len()];
    let mut slot_of = |code: u16| -> Option<usize> {
        for (k, &c) in slots_here.iter().enumerate() {
            if c == code && !used[k] {
                used[k] = true;
                return Some(base + k);
            }
        }
        slots_here.iter().position(|&c| c == code).map(|k| base + k)
    };
    let vop3 = matches!(inst.fmt, Fmt::Vop3 | Fmt::Vop3p);
    let sdwa = inst.ext == Ext::Sdwa;
    if inst.ext == Ext::Dpp16 || inst.ext == Ext::Dpp8 {
        return Err(format!("{full} (dpp)"));
    }
    // Raw operand codes.
    let (c0, c1, c2): (u16, u16, u16) = match inst.fmt {
        Fmt::Vop1 => ((w0 & 0x1FF) as u16, 0, 0),
        Fmt::Vop2 | Fmt::Vopc => ((w0 & 0x1FF) as u16, (256 + ((w0 >> 9) & 0xFF)) as u16, 0),
        _ => ((w1 & 0x1FF) as u16, ((w1 >> 9) & 0x1FF) as u16, ((w1 >> 18) & 0x1FF) as u16),
    };
    let dst = match inst.fmt {
        Fmt::Vop1 | Fmt::Vop2 => (w0 >> 17) & 0xFF,
        _ => w0 & 0xFF,
    };
    let mut a = if sdwa {
        let s = inst.words[1];
        let code = if s & (1 << 23) != 0 { (s & 0xFF) as u16 } else { 256 + (s & 0xFF) as u16 };
        read_src(t, code, None, &mut slot_of)?
    } else {
        read_src(t, c0, lit, &mut slot_of)?
    };
    let c1 = if sdwa && inst.fmt != Fmt::Vop1 && inst.words[1] & (1 << 31) != 0 { ((w0 >> 9) & 0xFF) as u16 } else { c1 };
    let mut bv = if matches!(inst.fmt, Fmt::Vop1) { t.cu(0) } else { read_src(t, c1, if sdwa { None } else { lit }, &mut slot_of)? };
    let is_mac = n.starts_with("v_mac") || n.starts_with("v_fmac") || n.starts_with("v_dot2c");
    let needs_c = vop3 && !is_mac && !(n.starts_with("v_cndmask") || n.starts_with("v_cmp"));
    let mut cv = if needs_c { read_src(t, c2, lit, &mut slot_of)? } else { t.cu(0) };
    // v_madmk: s0 * K + v1 ; v_madak: s0 * v1 + K ; mac/fmac: s0 * v1 + vdst.
    if inst.fmt == Fmt::Vop2 {
        if n.contains("mk_f") {
            let v1 = bv;
            bv = t.cu(lit.unwrap_or(0));
            cv = v1;
        } else if n.contains("ak_f") {
            cv = t.cu(lit.unwrap_or(0));
        }
    }
    if is_mac {
        cv = t.get_v(dst);
    }
    // SDWA source selection.
    if sdwa {
        let s = inst.words[1];
        let sel = |t: &mut T, x: Word, sel: u32, sext: bool| -> Word {
            if sel >= 6 {
                return x;
            }
            let (sh, bits) = if sel < 4 { (sel * 8, 8) } else { ((sel - 4) * 16, 16) };
            let shc = t.cu(sh);
            let v = t.b.shift_right_logical(t.t_u32, None, x, shc).unwrap();
            let m = t.cu(if bits == 8 { 0xFF } else { 0xFFFF });
            let v = t.b.bitwise_and(t.t_u32, None, v, m).unwrap();
            if sext {
                let k = t.cu(32 - bits);
                let l = t.b.shift_left_logical(t.t_u32, None, v, k).unwrap();
                t.b.shift_right_arithmetic(t.t_u32, None, l, k).unwrap()
            } else {
                v
            }
        };
        a = sel(t, a, (s >> 16) & 7, s & (1 << 19) != 0);
        bv = sel(t, bv, (s >> 24) & 7, s & (1 << 27) != 0);
        let fmods = |t: &mut T, x: Word, abs: bool, neg: bool| -> Word {
            if !abs && !neg {
                return x;
            }
            let mut f = t.f(x);
            if abs {
                f = t.ext(t.t_f32, glsl::FABS, &[f]);
            }
            if neg {
                f = t.b.f_negate(t.t_f32, None, f).unwrap();
            }
            t.u(f)
        };
        a = fmods(t, a, s & (1 << 21) != 0, s & (1 << 20) != 0);
        bv = fmods(t, bv, s & (1 << 29) != 0, s & (1 << 28) != 0);
    }
    let is_f32 = n.ends_with("_f32") || n.contains("_f32_");
    let mut clamp = false;
    let mut omod = 0;
    if vop3 && inst.fmt == Fmt::Vop3 && (is_f32 || n.ends_with("_f16")) {
        let abs = (w0 >> 8) & 7;
        let neg = w1 >> 29;
        let fl = |t: &mut T, x: Word, k: u32| -> Word {
            if abs & (1 << k) == 0 && neg & (1 << k) == 0 {
                return x;
            }
            let mut f = t.f(x);
            if abs & (1 << k) != 0 {
                f = t.ext(t.t_f32, glsl::FABS, &[f]);
            }
            if neg & (1 << k) != 0 {
                f = t.b.f_negate(t.t_f32, None, f).unwrap();
            }
            t.u(f)
        };
        if is_f32 {
            a = fl(t, a, 0);
            bv = fl(t, bv, 1);
            cv = fl(t, cv, 2);
        }
        clamp = w0 & (1 << 15) != 0;
        omod = (w1 >> 27) & 3;
    }

    // Compares.
    if n.starts_with("v_cmp") {
        let op = n.trim_start_matches("v_cmpx_").trim_start_matches("v_cmp_");
        let (fa, fb) = (t.f(a), t.f(bv));
        let tb = t.t_bool;
        let r = match op {
            "lt_f32" => t.b.f_ord_less_than(tb, None, fa, fb),
            "le_f32" => t.b.f_ord_less_than_equal(tb, None, fa, fb),
            "gt_f32" => t.b.f_ord_greater_than(tb, None, fa, fb),
            "ge_f32" => t.b.f_ord_greater_than_equal(tb, None, fa, fb),
            "eq_f32" => t.b.f_ord_equal(tb, None, fa, fb),
            "neq_f32" => t.b.f_unord_not_equal(tb, None, fa, fb),
            "lg_f32" => t.b.f_ord_not_equal(tb, None, fa, fb),
            "nlt_f32" => t.b.f_unord_greater_than_equal(tb, None, fa, fb),
            "u_f32" => t.b.f_unord_not_equal(tb, None, fa, fa),
            "eq_u32" | "eq_i32" => t.b.i_equal(tb, None, a, bv),
            "ne_u32" | "ne_i32" => t.b.i_not_equal(tb, None, a, bv),
            "lt_u32" => t.b.u_less_than(tb, None, a, bv),
            "le_u32" => t.b.u_less_than_equal(tb, None, a, bv),
            "gt_u32" => t.b.u_greater_than(tb, None, a, bv),
            "ge_u32" => t.b.u_greater_than_equal(tb, None, a, bv),
            "lt_i32" => t.b.s_less_than(tb, None, a, bv),
            "le_i32" => t.b.s_less_than_equal(tb, None, a, bv),
            "gt_i32" => t.b.s_greater_than(tb, None, a, bv),
            "ge_i32" => t.b.s_greater_than_equal(tb, None, a, bv),
            _ => return Err(full.to_string()),
        }
        .unwrap();
        let sdst: u16 = if vop3 {
            (w0 & 0xFF) as u16
        } else if sdwa && inst.words[1] & (1 << 15) != 0 {
            ((inst.words[1] >> 8) & 0x7F) as u16
        } else {
            106
        };
        if n.starts_with("v_cmpx") {
            // exec &= cond: a pixel whose lane turns off is discarded.
            let kill = t.b.id();
            let merge = t.b.id();
            t.b.selection_merge(merge, spirv::SelectionControl::NONE).unwrap();
            t.b.branch_conditional(r, merge, kill, []).unwrap();
            t.b.begin_block(Some(kill)).unwrap();
            t.b.kill().unwrap();
            t.b.begin_block(Some(merge)).unwrap();
        }
        if sdst == 106 {
            t.b.store(t.vcc, r, None, []).unwrap();
        } else if sdst != 125 && sdst != 126 {
            let m = t.mask_var(sdst);
            t.b.store(m, r, None, []).unwrap();
        }
        return Ok(());
    }

    let tu = t.t_u32;
    let tf = t.t_f32;
    let fa = t.f(a);
    let fb = t.f(bv);
    let fc = t.f(cv);
    macro_rules! fr {
        ($e:expr) => {{
            let r = $e;
            Some(r)
        }};
    }
    let rf: Option<Word> = match n {
        "v_add_f32" => fr!(t.b.f_add(tf, None, fa, fb).unwrap()),
        "v_sub_f32" => fr!(t.b.f_sub(tf, None, fa, fb).unwrap()),
        "v_subrev_f32" => fr!(t.b.f_sub(tf, None, fb, fa).unwrap()),
        "v_mul_f32" | "v_mul_legacy_f32" => fr!(t.b.f_mul(tf, None, fa, fb).unwrap()),
        "v_mac_f32" | "v_mad_f32" | "v_madmk_f32" | "v_madak_f32" | "v_fmac_f32" | "v_fma_f32" | "v_fmamk_f32" | "v_fmaak_f32"
        | "v_mac_legacy_f32" | "v_mad_legacy_f32" | "v_fmac_legacy_f32" => {
            let m = t.b.f_mul(tf, None, fa, fb).unwrap();
            fr!(t.b.f_add(tf, None, m, fc).unwrap())
        }
        "v_min_f32" => fr!(t.ext(tf, glsl::FMIN, &[fa, fb])),
        "v_max_f32" => fr!(t.ext(tf, glsl::FMAX, &[fa, fb])),
        "v_med3_f32" => {
            let lo = t.ext(tf, glsl::FMIN, &[fa, fb]);
            let hi = t.ext(tf, glsl::FMAX, &[fa, fb]);
            let m = t.ext(tf, glsl::FMAX, &[lo, fc]);
            fr!(t.ext(tf, glsl::FMIN, &[hi, m]))
        }
        "v_rcp_f32" | "v_rcp_iflag_f32" => {
            let one = t.cf(1.0);
            fr!(t.b.f_div(tf, None, one, fa).unwrap())
        }
        "v_rsq_f32" => fr!(t.ext(tf, glsl::INVERSE_SQRT, &[fa])),
        "v_sqrt_f32" => fr!(t.ext(tf, glsl::SQRT, &[fa])),
        "v_floor_f32" => fr!(t.ext(tf, glsl::FLOOR, &[fa])),
        "v_ceil_f32" => fr!(t.ext(tf, glsl::CEIL, &[fa])),
        "v_fract_f32" => fr!(t.ext(tf, glsl::FRACT, &[fa])),
        "v_trunc_f32" => fr!(t.ext(tf, glsl::TRUNC, &[fa])),
        "v_rndne_f32" => fr!(t.ext(tf, glsl::ROUND_EVEN, &[fa])),
        "v_exp_f32" => fr!(t.ext(tf, glsl::EXP2, &[fa])),
        "v_log_f32" => fr!(t.ext(tf, glsl::LOG2, &[fa])),
        "v_sin_f32" | "v_cos_f32" => {
            let tau = t.cf(std::f32::consts::TAU);
            let x = t.b.f_mul(tf, None, fa, tau).unwrap();
            fr!(t.ext(tf, if n == "v_sin_f32" { glsl::SIN } else { glsl::COS }, &[x]))
        }
        "v_cvt_f32_i32" => fr!(t.b.convert_s_to_f(tf, None, a).unwrap()),
        "v_cvt_f32_u32" => fr!(t.b.convert_u_to_f(tf, None, a).unwrap()),
        "v_cvt_f32_f16" => fr!(t.half_lo(a)),
        "v_cvt_f32_ubyte0" | "v_cvt_f32_ubyte1" | "v_cvt_f32_ubyte2" | "v_cvt_f32_ubyte3" => {
            let k = (n.as_bytes()[n.len() - 1] - b'0') as u32;
            let sh = t.cu(k * 8);
            let x = t.b.shift_right_logical(tu, None, a, sh).unwrap();
            let m = t.cu(0xFF);
            let x = t.b.bitwise_and(tu, None, x, m).unwrap();
            fr!(t.b.convert_u_to_f(tf, None, x).unwrap())
        }
        "v_dot2c_f32_f16" => {
            let a0 = t.half_lo(a);
            let a1 = t.half_hi(a);
            let b0 = t.half_lo(bv);
            let b1 = t.half_hi(bv);
            let p0 = t.b.f_mul(tf, None, a0, b0).unwrap();
            let p1 = t.b.f_mul(tf, None, a1, b1).unwrap();
            let s = t.b.f_add(tf, None, p0, p1).unwrap();
            fr!(t.b.f_add(tf, None, s, fc).unwrap())
        }
        _ => None,
    };
    let r: Word = if let Some(mut x) = rf {
        if omod != 0 {
            let k = t.cf(match omod { 1 => 2.0, 2 => 4.0, _ => 0.5 });
            x = t.b.f_mul(tf, None, x, k).unwrap();
        }
        if clamp || (sdwa && inst.words[1] & (1 << 13) != 0) {
            let z = t.cf(0.0);
            let o = t.cf(1.0);
            x = t.ext(tf, glsl::FCLAMP, &[x, z, o]);
        }
        t.u(x)
    } else {
        // 16-bit float ops: operate on the low halves.
        let h16 = |t: &mut T, op: &str| -> Option<Word> {
            let x = t.half_lo(a);
            let y = t.half_lo(bv);
            let z = t.half_lo(cv);
            let tf = t.t_f32;
            let r = match op {
                "add" => t.b.f_add(tf, None, x, y).unwrap(),
                "sub" => t.b.f_sub(tf, None, x, y).unwrap(),
                "subrev" => t.b.f_sub(tf, None, y, x).unwrap(),
                "mul" => t.b.f_mul(tf, None, x, y).unwrap(),
                "min" => t.ext(tf, glsl::FMIN, &[x, y]),
                "max" => t.ext(tf, glsl::FMAX, &[x, y]),
                "fma" => {
                    let m = t.b.f_mul(tf, None, x, y).unwrap();
                    t.b.f_add(tf, None, m, z).unwrap()
                }
                "ldexp" => {
                    let e = t.b.convert_s_to_f(tf, None, bv).unwrap();
                    let two = t.cf(2.0);
                    let p = t.ext(tf, 26 /* Pow */, &[two, e]);
                    t.b.f_mul(tf, None, x, p).unwrap()
                }
                _ => return None,
            };
            let zero = t.cf(0.0);
            Some(t.pack_half(r, zero))
        };
        let iv = |t: &mut T, op: &str| -> Option<Word> {
            let tu = t.t_u32;
            let k31 = t.cu(31);
            Some(match op {
                "v_mov_b32" => a,
                "v_and_b32" => t.b.bitwise_and(tu, None, a, bv).unwrap(),
                "v_or_b32" => t.b.bitwise_or(tu, None, a, bv).unwrap(),
                "v_xor_b32" => t.b.bitwise_xor(tu, None, a, bv).unwrap(),
                "v_xnor_b32" => {
                    let x = t.b.bitwise_xor(tu, None, a, bv).unwrap();
                    t.b.not(tu, None, x).unwrap()
                }
                "v_not_b32" => t.b.not(tu, None, a).unwrap(),
                "v_and_or_b32" => {
                    let x = t.b.bitwise_and(tu, None, a, bv).unwrap();
                    t.b.bitwise_or(tu, None, x, cv).unwrap()
                }
                "v_or3_b32" => {
                    let x = t.b.bitwise_or(tu, None, a, bv).unwrap();
                    t.b.bitwise_or(tu, None, x, cv).unwrap()
                }
                "v_xor3_b32" => {
                    let x = t.b.bitwise_xor(tu, None, a, bv).unwrap();
                    t.b.bitwise_xor(tu, None, x, cv).unwrap()
                }
                "v_add_nc_u32" | "v_add_u32" => t.b.i_add(tu, None, a, bv).unwrap(),
                "v_sub_nc_u32" => t.b.i_sub(tu, None, a, bv).unwrap(),
                "v_subrev_nc_u32" => t.b.i_sub(tu, None, bv, a).unwrap(),
                "v_add3_u32" => {
                    let x = t.b.i_add(tu, None, a, bv).unwrap();
                    t.b.i_add(tu, None, x, cv).unwrap()
                }
                "v_mul_lo_u32" | "v_mul_u32_u24" => t.b.i_mul(tu, None, a, bv).unwrap(),
                "v_lshlrev_b32" => {
                    let s = t.b.bitwise_and(tu, None, a, k31).unwrap();
                    t.b.shift_left_logical(tu, None, bv, s).unwrap()
                }
                "v_lshrrev_b32" => {
                    let s = t.b.bitwise_and(tu, None, a, k31).unwrap();
                    t.b.shift_right_logical(tu, None, bv, s).unwrap()
                }
                "v_ashrrev_i32" => {
                    let s = t.b.bitwise_and(tu, None, a, k31).unwrap();
                    t.b.shift_right_arithmetic(tu, None, bv, s).unwrap()
                }
                "v_lshl_add_u32" => {
                    let s = t.b.bitwise_and(tu, None, bv, k31).unwrap();
                    let x = t.b.shift_left_logical(tu, None, a, s).unwrap();
                    t.b.i_add(tu, None, x, cv).unwrap()
                }
                "v_lshl_or_b32" => {
                    let s = t.b.bitwise_and(tu, None, bv, k31).unwrap();
                    let x = t.b.shift_left_logical(tu, None, a, s).unwrap();
                    t.b.bitwise_or(tu, None, x, cv).unwrap()
                }
                "v_bfe_u32" => {
                    let o = t.b.bitwise_and(tu, None, bv, k31).unwrap();
                    let w = t.b.bitwise_and(tu, None, cv, k31).unwrap();
                    t.b.bit_field_u_extract(tu, None, a, o, w).unwrap()
                }
                "v_bfi_b32" => {
                    let x = t.b.bitwise_and(tu, None, a, bv).unwrap();
                    let na = t.b.not(tu, None, a).unwrap();
                    let y = t.b.bitwise_and(tu, None, na, cv).unwrap();
                    t.b.bitwise_or(tu, None, x, y).unwrap()
                }
                "v_min_u32" => t.ext(tu, glsl::UMIN, &[a, bv]),
                "v_max_u32" => t.ext(tu, glsl::UMAX, &[a, bv]),
                "v_min_i32" => t.ext(tu, glsl::SMIN, &[a, bv]),
                "v_max_i32" => t.ext(tu, glsl::SMAX, &[a, bv]),
                "v_cvt_i32_f32" => {
                    let f = t.f(a);
                    let tu2 = t.t_u32;
                    t.b.convert_f_to_s(tu2, None, f).unwrap()
                }
                "v_cvt_u32_f32" => {
                    let f = t.f(a);
                    let tu2 = t.t_u32;
                    t.b.convert_f_to_u(tu2, None, f).unwrap()
                }
                "v_cvt_pkrtz_f16_f32" => {
                    let x = t.f(a);
                    let y = t.f(bv);
                    t.pack_half(x, y)
                }
                "v_cvt_f16_f32" => {
                    let x = t.f(a);
                    let z = t.cf(0.0);
                    t.pack_half(x, z)
                }
                "v_cndmask_b32" => {
                    let cond = if vop3 {
                        let m = c2;
                        if let Some(&var) = t.masks.get(&m) {
                            let tb = t.t_bool;
                            t.b.load(tb, None, var, None, []).unwrap()
                        } else if m == 106 {
                            let tb = t.t_bool;
                            t.b.load(tb, None, t.vcc, None, []).unwrap()
                        } else {
                            return None;
                        }
                    } else {
                        let tb = t.t_bool;
                        t.b.load(tb, None, t.vcc, None, []).unwrap()
                    };
                    t.b.select(tu, None, cond, bv, a).unwrap()
                }
                _ => return None,
            })
        };
        let op16 = n.strip_prefix("v_").and_then(|s| s.strip_suffix("_f16"));
        let r16 = match op16 {
            Some("add") | Some("sub") | Some("subrev") | Some("mul") | Some("min") | Some("max") => h16(t, op16.unwrap()),
            Some("fmac") | Some("fma") | Some("mac") | Some("mad") | Some("fmamk") | Some("fmaak") | Some("madmk") | Some("madak") => h16(t, "fma"),
            Some("ldexp") => h16(t, "ldexp"),
            _ => None,
        };
        match r16.or_else(|| iv(t, n)) {
            Some(x) => x,
            None => return Err(full.to_string()),
        }
    };
    // SDWA destination selection.
    let mut r = r;
    if sdwa && inst.fmt != Fmt::Vopc {
        let s = inst.words[1];
        let sel = (s >> 8) & 7;
        let unused = (s >> 11) & 3;
        if sel < 6 {
            let (sh, mask) = if sel < 4 { (sel * 8, 0xFFu32) } else { ((sel - 4) * 16, 0xFFFF) };
            let m = t.cu(mask);
            let x = t.b.bitwise_and(tu, None, r, m).unwrap();
            let shc = t.cu(sh);
            let x = t.b.shift_left_logical(tu, None, x, shc).unwrap();
            r = if unused == 2 {
                let old = t.get_v(dst);
                let keep = t.cu(!(mask << sh));
                let o = t.b.bitwise_and(tu, None, old, keep).unwrap();
                t.b.bitwise_or(tu, None, o, x).unwrap()
            } else {
                x
            };
        }
    }
    t.set_v(dst, r);
    Ok(())
}

/// Vertex shader for generic draws: location 0 is the clip position, locations 1..=8 are
/// passed through to the fragment shader's attributes 0..7.
pub fn passthrough_vs() -> Vec<u32> {
    let mut b = Builder::new();
    b.set_version(1, 3);
    b.capability(spirv::Capability::Shader);
    b.memory_model(spirv::AddressingModel::Logical, spirv::MemoryModel::GLSL450);
    let t_void = b.type_void();
    let t_f32 = b.type_float(32);
    let t_v4f = b.type_vector(t_f32, 4);
    let p_in = b.type_pointer(None, spirv::StorageClass::Input, t_v4f);
    let p_out = b.type_pointer(None, spirv::StorageClass::Output, t_v4f);
    let mut ins = vec![];
    let mut outs = vec![];
    for k in 0..=MAX_ATTRS as u32 {
        let v = b.variable(p_in, None, spirv::StorageClass::Input, None);
        b.decorate(v, spirv::Decoration::Location, [Operand::LiteralBit32(k)]);
        ins.push(v);
    }
    let pos = b.variable(p_out, None, spirv::StorageClass::Output, None);
    b.decorate(pos, spirv::Decoration::BuiltIn, [Operand::BuiltIn(spirv::BuiltIn::Position)]);
    for k in 0..MAX_ATTRS as u32 {
        let v = b.variable(p_out, None, spirv::StorageClass::Output, None);
        b.decorate(v, spirv::Decoration::Location, [Operand::LiteralBit32(k)]);
        outs.push(v);
    }
    let t_fn = b.type_function(t_void, []);
    let main = b.begin_function(t_void, None, spirv::FunctionControl::NONE, t_fn).unwrap();
    b.begin_block(None).unwrap();
    let p = b.load(t_v4f, None, ins[0], None, []).unwrap();
    b.store(pos, p, None, []).unwrap();
    for k in 0..MAX_ATTRS {
        let a = b.load(t_v4f, None, ins[k + 1], None, []).unwrap();
        b.store(outs[k], a, None, []).unwrap();
    }
    b.ret().unwrap();
    b.end_function().unwrap();
    let mut iface = ins.clone();
    iface.push(pos);
    iface.extend(outs.iter().copied());
    b.entry_point(spirv::ExecutionModel::Vertex, main, "main", iface);
    use rspirv::binary::Assemble;
    b.module().assemble()
}
