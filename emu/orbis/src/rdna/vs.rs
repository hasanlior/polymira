//! RDNA2 NGG vertex shader → Vulkan SPIR-V 1.3 translator.
//!
//! Runs scalar/SMEM instructions once per draw on the CPU (`extract_gpu_vs_draw`) to resolve
//! constant buffers and vertex-buffer `V#` descriptors, then translates the vector body
//! (`Vop1`/`Vop2`/`Vop3`/`Vopc`/`Mubuf`/`Mtbuf`/`Exp`) into a native Vulkan SPIR-V vertex
//! shader that runs directly on the GPU.
//!
//! Descriptor Set 1 (VERTEX stage):
//!   * binding 0: `STORAGE_BUFFER_DYNAMIC` — `uint u[]` per-draw uniforms:
//!       u[0..4]  = viewport (xs, xo, ys, yo) as f32 bits
//!       u[4..8]  = (2.0/tw, 2.0/th, have_vp, 0)
//!       u[8..24] = up to 8 Mubuf (rel_off_bytes, stride_bytes) pairs
//!       u[24..]  = scalar operand slots in instruction order
//!   * binding 1..=4: `STORAGE_BUFFER` — `uint d[]` raw guest vertex buffers (up to 4).

use super::interp::{CompiledShader, Lane, MemCheck};
use super::{Ext, Fmt};
use rspirv::binary::Assemble;
use rspirv::dr::{Builder, Operand};
use rspirv::spirv::{self, Word};
use std::collections::HashMap;

const MAX_ATTRS: usize = 8;
const MAX_VBS: usize = 4;
const MAX_MUBUFS: usize = 8;
const SLOT_BASE: u32 = 24;

mod glsl {
    pub const FABS: u32 = 4;
    pub const FLOOR: u32 = 8;
    pub const FRACT: u32 = 10;
    pub const SQRT: u32 = 31;
    pub const INVERSE_SQRT: u32 = 32;
    pub const FMIN: u32 = 37;
    pub const FMAX: u32 = 40;
    pub const FCLAMP: u32 = 43;
    pub const FMA: u32 = 50;
    pub const UNPACK_SNORM2X16: u32 = 60;
    pub const UNPACK_UNORM2X16: u32 = 61;
    pub const UNPACK_HALF2X16: u32 = 62;
    pub const UNPACK_SNORM4X8: u32 = 63;
    pub const UNPACK_UNORM4X8: u32 = 64;
}

/// Per-draw scalar + VB bindings extracted on the CPU for a GPU-translated vertex shader.
pub struct GpuVsDrawData {
    /// Pipeline specialization key: format + vb_slot of each Mubuf in program order.
    pub mubuf_key: [u16; MAX_MUBUFS],
    pub n_mubuf: u8,
    /// Guest vertex buffers `(base_addr, byte_len)` for bindings 1..=4.
    pub vbs: [(usize, usize); MAX_VBS],
    pub n_vbs: u8,
    /// Uniform `u32` array (`u[0..]`): viewport + mubuf (rel_off, stride) + scalar slots.
    pub params: Vec<u32>,
}

/// Returns true if a `CompiledShader` is eligible for GPU SPIR-V VS translation.
/// Allows branch-free shaders AND simple forward `v_cmpx_*` + `s_cbranch_execz` predication
/// (e.g. Minecraft's 184-inst transparent `RenderChunk` VS).
pub fn is_gpu_vs_eligible(sh: &CompiledShader) -> bool {
    sh.gpu_vs_eligible
}

/// Runs the scalar/SMEM instructions of `sh` once on the CPU and packs all per-draw
/// uniforms + vertex-buffer bindings for the GPU SPIR-V vertex shader.
pub fn extract_gpu_vs_draw(
    sh: &CompiledShader,
    vs_ud: &[u32; 24],
    vp: [u32; 8],
    ok: MemCheck,
) -> Option<GpuVsDrawData> {
    if !is_gpu_vs_eligible(sh) {
        return None;
    }
    let mut lane = Lane::new();
    lane.s[3] = 0x101;
    lane.s[8..32].copy_from_slice(vs_ud);

    let mut mubuf_key = [0u16; MAX_MUBUFS];
    let mut n_mubuf = 0usize;
    let mut vbs: [(usize, usize); MAX_VBS] = [(0, 0); MAX_VBS];
    let mut n_vbs = 0usize;
    let mut params = Vec::with_capacity(SLOT_BASE as usize + 64);
    params.resize(SLOT_BASE as usize, 0);
    params[0..8].copy_from_slice(&vp);

    for inst in &sh.insts {
        let w0 = inst.w0();
        let w1 = inst.w1();
        let lit = inst.literal;
        match inst.fmt {
            Fmt::Sop1 | Fmt::Sop2 | Fmt::Sopc | Fmt::Sopk | Fmt::Smem => {
                super::interp::step_scalar_for_gpu(inst, &mut lane, ok)?;
            }
            Fmt::Sopp => {
                if inst.name == "s_endpgm" {
                    break;
                }
            }
            Fmt::Mubuf | Fmt::Mtbuf => {
                if n_mubuf >= MAX_MUBUFS {
                    return None;
                }
                let inst_off = (w0 & 0xFFF) as u64;
                let offen = w0 & (1 << 12) != 0;
                let idxen = w0 & (1 << 13) != 0;
                if !idxen || offen {
                    return None;
                }
                let srsrc = (((w1 >> 16) & 0x1F) * 4) as usize;
                let soff = lane.src_pub(((w1 >> 24) & 0xFF) as u16, None) as u64;
                let bw0 = lane.s[srsrc] as u64;
                let bw1 = lane.s[srsrc + 1];
                let bw2 = lane.s[srsrc + 2];
                let base = bw0 | (((bw1 & 0xFFFF) as u64) << 32);
                let stride = ((bw1 >> 16) & 0x3FFF) as u64;
                let vfmt = (lane.s[srsrc + 3] >> 12) & 0x7F;
                let fmt = if inst.fmt == Fmt::Mtbuf { (w0 >> 19) & 0x7F } else { vfmt };
                let base_addr = base.wrapping_add(soff).wrapping_add(inst_off) as usize;
                let raw_need = if bw2 > 0 && bw2 < 0x1000_0000 {
                    if stride > 0 {
                        (bw2 as usize).saturating_mul(stride as usize)
                    } else {
                        bw2 as usize
                    }
                } else {
                    return None;
                };
                let need = raw_need.min(64 << 20).saturating_add(16);
                if base_addr < 0x1_0000_0000 || !ok(base_addr, (stride as usize).max(16)) {
                    return None;
                }
                // Group nearby Mubuf bases (e.g. interleaved vertex attributes at +0/+12/+16/+20)
                // into the same VB slot so RenderChunk uses a single GPU VkBuffer!
                let mut chosen_slot = None;
                let mut rel_off = 0usize;
                for s in 0..n_vbs {
                    let (vb_a, vb_len) = vbs[s];
                    if base_addr >= vb_a && base_addr - vb_a < 256 {
                        rel_off = base_addr - vb_a;
                        let new_end = rel_off + need;
                        if new_end > vb_len {
                            vbs[s].1 = new_end;
                        }
                        chosen_slot = Some(s);
                        break;
                    } else if vb_a > base_addr && vb_a - base_addr < 256 {
                        // Earlier attribute offset: shift VB base down? Keep simpler: separate slot or
                        // align VB base to 4 bytes; RenderChunk loads +0 first so base_addr >= vb_a.
                    }
                }
                let slot = match chosen_slot {
                    Some(s) => s,
                    None => {
                        if n_vbs >= MAX_VBS {
                            return None;
                        }
                        let s = n_vbs;
                        let aligned = base_addr & !3;
                        rel_off = base_addr - aligned;
                        vbs[s] = (aligned, rel_off + need);
                        n_vbs += 1;
                        s
                    }
                };
                mubuf_key[n_mubuf] = ((slot as u16) << 8) | (fmt as u16 & 0xFF);
                params[8 + n_mubuf * 2] = rel_off as u32;
                params[8 + n_mubuf * 2 + 1] = stride as u32;
                n_mubuf += 1;
            }
            Fmt::Vopc => {
                let s0 = (w0 & 0x1FF) as u16;
                if s0 < 256 {
                    params.push(lane.src_pub(s0, lit));
                }
            }
            Fmt::Vop1 | Fmt::Vop2 | Fmt::Vop3 => {
                let n = inst.name.trim_end_matches("_e32").trim_end_matches("_e64").trim_end_matches("_sdwa");
                if inst.ext == Ext::Sdwa {
                    let s = inst.words[1];
                    let src0 = (s & 0xFF) as u16;
                    if s & (1 << 23) != 0 {
                        params.push(lane.src_pub(src0, None));
                    }
                    if inst.fmt != Fmt::Vop1 && s & (1 << 31) != 0 {
                        let src1 = ((w0 >> 9) & 0xFF) as u16;
                        params.push(lane.src_pub(src1, None));
                    }
                    continue;
                }
                match inst.fmt {
                    Fmt::Vop1 => {
                        let s0 = (w0 & 0x1FF) as u16;
                        if s0 < 256 {
                            params.push(lane.src_pub(s0, lit));
                        }
                    }
                    Fmt::Vop2 => {
                        let s0 = (w0 & 0x1FF) as u16;
                        if s0 < 256 {
                            params.push(lane.src_pub(s0, lit));
                        }
                        if matches!(n, "v_madmk_f32" | "v_fmamk_f32" | "v_madak_f32" | "v_fmaak_f32") {
                            params.push(lit.unwrap_or(0));
                        }
                        if n == "v_cndmask_b32" {
                            params.push(lane.s[106] & 1);
                        }
                    }
                    Fmt::Vop3 => {
                        let s0 = (w1 & 0x1FF) as u16;
                        let s1 = ((w1 >> 9) & 0x1FF) as u16;
                        let s2 = ((w1 >> 18) & 0x1FF) as u16;
                        if s0 < 256 {
                            params.push(lane.src_pub(s0, lit));
                        }
                        if s1 < 256 {
                            params.push(lane.src_pub(s1, lit));
                        }
                        if n == "v_cndmask_b32" {
                            let bit = if (s2 as usize) < 128 { lane.s[s2 as usize] & 1 } else { return None };
                            params.push(bit);
                        } else if s2 < 256 {
                            params.push(lane.src_pub(s2, lit));
                        }
                    }
                    _ => {}
                }
            }
            Fmt::Exp => {}
            _ => return None,
        }
    }

    Some(GpuVsDrawData { mubuf_key, n_mubuf: n_mubuf as u8, vbs, n_vbs: n_vbs as u8, params })
}

struct VsBuilder {
    b: Builder,
    glsl: Word,
    t_bool: Word,
    t_u32: Word,
    t_i32: Word,
    t_f32: Word,
    t_v2f: Word,
    t_v4f: Word,
    p_priv_u32: Word,
    p_sb_u32: Word,
    vregs: Word,
    params_var: Word,
    vb_vars: [Word; MAX_VBS],
    exec_var: Word,
    exec_predicated: bool,
    slot_idx: u32,
    consts: HashMap<u32, Word>,
}

impl VsBuilder {
    fn next_slot(&mut self) -> Word {
        let v = self.load_param(self.slot_idx);
        self.slot_idx += 1;
        v
    }
    fn next_src(&mut self, code: u16) -> Word {
        if code >= 256 && code <= 511 {
            self.get_v((code - 256) as u32)
        } else {
            self.next_slot()
        }
    }
    fn cu(&mut self, v: u32) -> Word {
        *self.consts.entry(v).or_insert_with(|| self.b.constant_bit32(self.t_u32, v))
    }
    fn cf(&mut self, v: f32) -> Word {
        let u = self.cu(v.to_bits());
        self.f(u)
    }
    fn f(&mut self, x: Word) -> Word {
        self.b.bitcast(self.t_f32, None, x).unwrap()
    }
    fn u(&mut self, x: Word) -> Word {
        self.b.bitcast(self.t_u32, None, x).unwrap()
    }
    fn get_v(&mut self, r: u32) -> Word {
        let idx = self.cu(r);
        let p = self.b.access_chain(self.p_priv_u32, None, self.vregs, [idx]).unwrap();
        self.b.load(self.t_u32, None, p, None, []).unwrap()
    }
    fn set_v(&mut self, r: u32, val: Word) {
        let idx = self.cu(r);
        let p = self.b.access_chain(self.p_priv_u32, None, self.vregs, [idx]).unwrap();
        let final_v = if self.exec_predicated {
            let cond = self.b.load(self.t_bool, None, self.exec_var, None, []).unwrap();
            let old = self.b.load(self.t_u32, None, p, None, []).unwrap();
            self.b.select(self.t_u32, None, cond, val, old).unwrap()
        } else {
            val
        };
        self.b.store(p, final_v, None, []).unwrap();
    }
    fn load_param(&mut self, idx: u32) -> Word {
        let z = self.cu(0);
        let i = self.cu(idx);
        let p = self.b.access_chain(self.p_sb_u32, None, self.params_var, [z, i]).unwrap();
        self.b.load(self.t_u32, None, p, None, []).unwrap()
    }
    fn load_vb_u32(&mut self, vb_slot: usize, word_idx: Word) -> Word {
        let z = self.cu(0);
        let p = self.b.access_chain(self.p_sb_u32, None, self.vb_vars[vb_slot], [z, word_idx]).unwrap();
        self.b.load(self.t_u32, None, p, None, []).unwrap()
    }
    /// Reads an unaligned u32 at `byte_addr` from `vb_slot` (handles arbitrary 1/2/4-byte alignment).
    fn load_vb_unaligned_u32(&mut self, vb_slot: usize, byte_addr: Word) -> Word {
        let two = self.cu(2);
        let three = self.cu(3);
        let one = self.cu(1);
        let zero = self.cu(0);
        let c32 = self.cu(32);
        let widx = self.b.shift_right_logical(self.t_u32, None, byte_addr, two).unwrap();
        let rem = self.b.bitwise_and(self.t_u32, None, byte_addr, three).unwrap();
        let lo = self.load_vb_u32(vb_slot, widx);
        let is_aligned = self.b.i_equal(self.t_bool, None, rem, zero).unwrap();
        let widx1 = self.b.i_add(self.t_u32, None, widx, one).unwrap();
        let hi = self.load_vb_u32(vb_slot, widx1);
        let sh_lo = self.b.shift_left_logical(self.t_u32, None, rem, three).unwrap();
        let sh_hi_raw = self.b.i_sub(self.t_u32, None, c32, sh_lo).unwrap();
        let c31 = self.cu(31);
        let sh_hi = self.b.bitwise_and(self.t_u32, None, sh_hi_raw, c31).unwrap();
        let p0 = self.b.shift_right_logical(self.t_u32, None, lo, sh_lo).unwrap();
        let p1 = self.b.shift_left_logical(self.t_u32, None, hi, sh_hi).unwrap();
        let unaligned = self.b.bitwise_or(self.t_u32, None, p0, p1).unwrap();
        self.b.select(self.t_u32, None, is_aligned, lo, unaligned).unwrap()
    }
    fn load_vb_u16(&mut self, vb_slot: usize, byte_addr: Word) -> Word {
        let two = self.cu(2);
        let three = self.cu(3);
        let mask16 = self.cu(0xFFFF);
        let widx = self.b.shift_right_logical(self.t_u32, None, byte_addr, two).unwrap();
        let low_b = self.b.bitwise_and(self.t_u32, None, byte_addr, two).unwrap();
        let bit_off = self.b.shift_left_logical(self.t_u32, None, low_b, three).unwrap();
        let w = self.load_vb_u32(vb_slot, widx);
        let s = self.b.shift_right_logical(self.t_u32, None, w, bit_off).unwrap();
        self.b.bitwise_and(self.t_u32, None, s, mask16).unwrap()
    }
    fn load_vb_u8(&mut self, vb_slot: usize, byte_addr: Word) -> Word {
        let two = self.cu(2);
        let three = self.cu(3);
        let mask8 = self.cu(0xFF);
        let widx = self.b.shift_right_logical(self.t_u32, None, byte_addr, two).unwrap();
        let low_b = self.b.bitwise_and(self.t_u32, None, byte_addr, three).unwrap();
        let bit_off = self.b.shift_left_logical(self.t_u32, None, low_b, three).unwrap();
        let w = self.load_vb_u32(vb_slot, widx);
        let s = self.b.shift_right_logical(self.t_u32, None, w, bit_off).unwrap();
        self.b.bitwise_and(self.t_u32, None, s, mask8).unwrap()
    }
    fn ext(&mut self, ty: Word, op: u32, args: &[Word]) -> Word {
        let ops: Vec<Operand> = args.iter().map(|&x| Operand::IdRef(x)).collect();
        self.b.ext_inst(ty, None, self.glsl, op, ops).unwrap()
    }
    fn fmod(&mut self, x: Word, abs: bool, neg: bool) -> Word {
        let mut v = self.f(x);
        if abs {
            v = self.ext(self.t_f32, glsl::FABS, &[v]);
        }
        if neg {
            v = self.b.f_negate(self.t_f32, None, v).unwrap();
        }
        v
    }
    fn omod_clamp(&mut self, mut v: Word, omod: u32, clamp: bool) -> Word {
        if omod != 0 {
            let k = self.cf(match omod {
                1 => 2.0,
                2 => 4.0,
                _ => 0.5,
            });
            v = self.b.f_mul(self.t_f32, None, v, k).unwrap();
        }
        if clamp {
            let z = self.cf(0.0);
            let o = self.cf(1.0);
            v = self.ext(self.t_f32, glsl::FCLAMP, &[v, z, o]);
        }
        v
    }
    fn sdwa_sel(&mut self, x: Word, sel: u32, sext: bool) -> Word {
        match sel {
            0..=3 => {
                let sh = self.cu(sel * 8);
                let ff = self.cu(0xFF);
                let v = self.b.shift_right_logical(self.t_u32, None, x, sh).unwrap();
                let b = self.b.bitwise_and(self.t_u32, None, v, ff).unwrap();
                if sext {
                    let c24 = self.cu(24);
                    let s = self.b.shift_left_logical(self.t_u32, None, b, c24).unwrap();
                    self.b.shift_right_arithmetic(self.t_u32, None, s, c24).unwrap()
                } else {
                    b
                }
            }
            4 | 5 => {
                let sh = self.cu((sel - 4) * 16);
                let ffff = self.cu(0xFFFF);
                let v = self.b.shift_right_logical(self.t_u32, None, x, sh).unwrap();
                let w = self.b.bitwise_and(self.t_u32, None, v, ffff).unwrap();
                if sext {
                    let c16 = self.cu(16);
                    let s = self.b.shift_left_logical(self.t_u32, None, w, c16).unwrap();
                    self.b.shift_right_arithmetic(self.t_u32, None, s, c16).unwrap()
                } else {
                    w
                }
            }
            _ => x,
        }
    }
}

/// Translates an eligible RDNA2 vertex shader into a native Vulkan SPIR-V 1.3 vertex shader.
/// Returns `(spirv_words, expected_param_u32_count)`.
pub fn translate(sh: &CompiledShader, cntl: &[u32; 8], mubuf_key: &[u16; MAX_MUBUFS]) -> Result<(Vec<u32>, u32), String> {
    if !is_gpu_vs_eligible(sh) {
        return Err("not eligible".into());
    }
    let mut b = Builder::new();
    b.set_version(1, 3);
    b.capability(spirv::Capability::Shader);
    let glsl = b.ext_inst_import("GLSL.std.450");
    b.memory_model(spirv::AddressingModel::Logical, spirv::MemoryModel::GLSL450);

    let t_void = b.type_void();
    let t_bool = b.type_bool();
    let t_u32 = b.type_int(32, 0);
    let t_i32 = b.type_int(32, 1);
    let t_f32 = b.type_float(32);
    let t_v2f = b.type_vector(t_f32, 2);
    let t_v4f = b.type_vector(t_f32, 4);

    // Private VGPR file: uint[256] zero-initialized to match CPU Lane::new()
    let c256 = b.constant_bit32(t_u32, 256);
    let t_vregs = b.type_array(t_u32, c256);
    let null_vregs = b.constant_null(t_vregs);
    let p_priv_vregs = b.type_pointer(None, spirv::StorageClass::Private, t_vregs);
    let p_priv_u32 = b.type_pointer(None, spirv::StorageClass::Private, t_u32);
    let p_priv_bool = b.type_pointer(None, spirv::StorageClass::Private, t_bool);
    let vregs = b.variable(p_priv_vregs, None, spirv::StorageClass::Private, Some(null_vregs));
    let exec_var = b.variable(p_priv_bool, None, spirv::StorageClass::Private, None);

    // StorageBuffer runtime array struct: { uint data[]; }
    let t_rt_u32 = b.type_runtime_array(t_u32);
    b.decorate(t_rt_u32, spirv::Decoration::ArrayStride, [Operand::LiteralBit32(4)]);
    let t_sb_struct = b.type_struct([t_rt_u32]);
    b.decorate(t_sb_struct, spirv::Decoration::Block, []);
    b.member_decorate(t_sb_struct, 0, spirv::Decoration::Offset, [Operand::LiteralBit32(0)]);
    b.member_decorate(t_sb_struct, 0, spirv::Decoration::NonWritable, []);
    let p_sb_struct = b.type_pointer(None, spirv::StorageClass::StorageBuffer, t_sb_struct);
    let p_sb_u32 = b.type_pointer(None, spirv::StorageClass::StorageBuffer, t_u32);

    // Set 1, Binding 0: VsParams (STORAGE_BUFFER_DYNAMIC)
    let params_var = b.variable(p_sb_struct, None, spirv::StorageClass::StorageBuffer, None);
    b.decorate(params_var, spirv::Decoration::DescriptorSet, [Operand::LiteralBit32(1)]);
    b.decorate(params_var, spirv::Decoration::Binding, [Operand::LiteralBit32(0)]);

    // Set 1, Binding 1..=4: Vb0..Vb3 (STORAGE_BUFFER)
    let mut vb_vars = [0u32; MAX_VBS];
    for k in 0..MAX_VBS {
        let v = b.variable(p_sb_struct, None, spirv::StorageClass::StorageBuffer, None);
        b.decorate(v, spirv::Decoration::DescriptorSet, [Operand::LiteralBit32(1)]);
        b.decorate(v, spirv::Decoration::Binding, [Operand::LiteralBit32((k + 1) as u32)]);
        vb_vars[k] = v;
    }

    // Input: gl_VertexIndex
    let p_in_i32 = b.type_pointer(None, spirv::StorageClass::Input, t_i32);
    let v_idx_in = b.variable(p_in_i32, None, spirv::StorageClass::Input, None);
    b.decorate(v_idx_in, spirv::Decoration::BuiltIn, [Operand::BuiltIn(spirv::BuiltIn::VertexIndex)]);

    // Outputs: gl_Position + layout(location = 0..7) out vec4
    let p_out_v4 = b.type_pointer(None, spirv::StorageClass::Output, t_v4f);
    let pos_out = b.variable(p_out_v4, None, spirv::StorageClass::Output, None);
    b.decorate(pos_out, spirv::Decoration::BuiltIn, [Operand::BuiltIn(spirv::BuiltIn::Position)]);
    let mut attr_outs = [0u32; MAX_ATTRS];
    for k in 0..MAX_ATTRS {
        let v = b.variable(p_out_v4, None, spirv::StorageClass::Output, None);
        b.decorate(v, spirv::Decoration::Location, [Operand::LiteralBit32(k as u32)]);
        attr_outs[k] = v;
    }

    let t_fn = b.type_function(t_void, []);
    let main = b.begin_function(t_void, None, spirv::FunctionControl::NONE, t_fn).unwrap();
    b.begin_block(None).unwrap();

    let mut t = VsBuilder {
        b,
        glsl,
        t_bool,
        t_u32,
        t_i32,
        t_f32,
        t_v2f,
        t_v4f,
        p_priv_u32,
        p_sb_u32,
        vregs,
        params_var,
        vb_vars,
        exec_var,
        exec_predicated: false,
        slot_idx: SLOT_BASE,
        consts: HashMap::new(),
    };

    // Initialize exec = true, v[5] = VertexIndex, v[8] = 0, and outputs to vec4(0).
    let c_true = t.b.constant_true(t_bool);
    t.b.store(t.exec_var, c_true, None, []).unwrap();
    let vidx_i = t.b.load(t_i32, None, v_idx_in, None, []).unwrap();
    let vidx_u = t.b.bitcast(t_u32, None, vidx_i).unwrap();
    t.set_v(5, vidx_u);
    let cz = t.cu(0);
    t.set_v(8, cz);
    let fz = t.cf(0.0);
    let v4z = t.b.composite_construct(t_v4f, None, [fz, fz, fz, fz]).unwrap();
    for k in 0..MAX_ATTRS {
        t.b.store(attr_outs[k], v4z, None, []).unwrap();
    }

    let mut mubuf_idx = 0usize;

    for inst in &sh.insts {
        let w0 = inst.w0();
        let w1 = inst.w1();
        match inst.fmt {
            Fmt::Sop1 => {
                // `s_mov_b64 exec, ...` restores exec after v_cmpx_* predication.
                let sdst = (w0 >> 16) & 0x7F;
                if sdst == 126 && t.exec_predicated {
                    t.b.store(t.exec_var, c_true, None, []).unwrap();
                    t.exec_predicated = false;
                }
            }
            Fmt::Sop2 | Fmt::Sopc | Fmt::Sopk | Fmt::Smem => {
                let sdst = (w0 >> 16) & 0x7F;
                if inst.fmt == Fmt::Sop2 && sdst == 126 && inst.name == "s_or_b64" && t.exec_predicated {
                    t.b.store(t.exec_var, c_true, None, []).unwrap();
                    t.exec_predicated = false;
                }
            }
            Fmt::Sopp => {
                if inst.name == "s_endpgm" {
                    break;
                }
            }
            Fmt::Mubuf | Fmt::Mtbuf => {
                if mubuf_idx >= MAX_MUBUFS {
                    return Err("too many mubufs".into());
                }
                let mk = mubuf_key[mubuf_idx];
                let vb_slot = (mk >> 8) as usize;
                let fmt = (mk & 0xFF) as u32;
                let rel_off = t.load_param(8 + (mubuf_idx as u32) * 2);
                let stride = t.load_param(8 + (mubuf_idx as u32) * 2 + 1);
                mubuf_idx += 1;

                let vaddr = w1 & 0xFF;
                let vdata = (w1 >> 8) & 0xFF;
                let idx_val = t.get_v(vaddr);
                let byte_off = t.b.i_mul(t_u32, None, idx_val, stride).unwrap();
                let byte_addr = t.b.i_add(t_u32, None, rel_off, byte_off).unwrap();
                let n = inst.name;
                if n.contains("load_format") {
                    let comps = if n.contains("xyzw") { 4 } else if n.contains("xyz") { 3 } else if n.contains("xy") { 2 } else { 1 };
                    let vals = emit_load_format(&mut t, vb_slot, byte_addr, fmt)?;
                    for k in 0..comps {
                        t.set_v(vdata + k as u32, vals[k]);
                    }
                } else if n.starts_with("buffer_load_dword") {
                    let cnt = if n.ends_with("x4") { 4 } else if n.ends_with("x3") { 3 } else if n.ends_with("x2") { 2 } else { 1 };
                    for k in 0..cnt {
                        let off = t.cu(k * 4);
                        let a = t.b.i_add(t_u32, None, byte_addr, off).unwrap();
                        let val = t.load_vb_unaligned_u32(vb_slot, a);
                        t.set_v(vdata + k, val);
                    }
                } else {
                    return Err(format!("mubuf {n}"));
                }
            }
            Fmt::Vopc => {
                // v_cmpx_<cond>_f32_e32
                let a = t.next_src((w0 & 0x1FF) as u16);
                let b_val = t.get_v((w0 >> 9) & 0xFF);
                let fa = t.f(a);
                let fb = t.f(b_val);
                let n = inst.name.trim_end_matches("_e32").trim_end_matches("_e64");
                let cmp = match n {
                    "v_cmpx_lt_f32" => t.b.f_ord_less_than(t_bool, None, fa, fb).unwrap(),
                    "v_cmpx_le_f32" => t.b.f_ord_less_than_equal(t_bool, None, fa, fb).unwrap(),
                    "v_cmpx_gt_f32" => t.b.f_ord_greater_than(t_bool, None, fa, fb).unwrap(),
                    "v_cmpx_ge_f32" => t.b.f_ord_greater_than_equal(t_bool, None, fa, fb).unwrap(),
                    "v_cmpx_eq_f32" => t.b.f_ord_equal(t_bool, None, fa, fb).unwrap(),
                    "v_cmpx_lg_f32" | "v_cmpx_neq_f32" => t.b.f_unord_not_equal(t_bool, None, fa, fb).unwrap(),
                    _ => return Err(format!("vopc {n}")),
                };
                t.b.store(t.exec_var, cmp, None, []).unwrap();
                t.exec_predicated = true;
            }
            Fmt::Vop1 | Fmt::Vop2 | Fmt::Vop3 => {
                let n = inst.name.trim_end_matches("_e32").trim_end_matches("_e64").trim_end_matches("_sdwa");
                if inst.ext == Ext::Sdwa {
                    let s = inst.words[1];
                    let dst = (w0 >> 17) & 0xFF;
                    let src0 = (s & 0xFF) as u16;
                    let a_raw = if s & (1 << 23) != 0 {
                        t.next_slot()
                    } else {
                        t.get_v(src0 as u32)
                    };
                    let b_raw = if inst.fmt == Fmt::Vop1 {
                        t.cu(0)
                    } else if s & (1 << 31) != 0 {
                        t.next_slot()
                    } else {
                        t.get_v((w0 >> 9) & 0xFF)
                    };
                    let a_s = t.sdwa_sel(a_raw, (s >> 16) & 7, s & (1 << 19) != 0);
                    let b_s = t.sdwa_sel(b_raw, (s >> 24) & 7, s & (1 << 27) != 0);
                    let fa = t.fmod(a_s, s & (1 << 21) != 0, s & (1 << 20) != 0);
                    let fb = t.fmod(b_s, s & (1 << 29) != 0, s & (1 << 28) != 0);
                    let rf = match n {
                        "v_add_f32" => t.b.f_add(t_f32, None, fa, fb).unwrap(),
                        "v_sub_f32" => t.b.f_sub(t_f32, None, fa, fb).unwrap(),
                        "v_subrev_f32" => t.b.f_sub(t_f32, None, fb, fa).unwrap(),
                        "v_mul_f32" => t.b.f_mul(t_f32, None, fa, fb).unwrap(),
                        "v_max_f32" => t.ext(t_f32, glsl::FMAX, &[fa, fb]),
                        "v_min_f32" => t.ext(t_f32, glsl::FMIN, &[fa, fb]),
                        "v_mov_b32" => fa,
                        _ => return Err(format!("sdwa {n}")),
                    };
                    let rf = t.omod_clamp(rf, (s >> 14) & 3, s & (1 << 13) != 0);
                    let mut ru = t.u(rf);
                    let dsel = (s >> 8) & 7;
                    let unused = (s >> 11) & 3;
                    if dsel < 6 {
                        let (mask, sh) = if dsel < 4 { (0xFFu32, dsel * 8) } else { (0xFFFFu32, (dsel - 4) * 16) };
                        let m = t.cu(mask);
                        let shc = t.cu(sh);
                        let x = t.b.bitwise_and(t_u32, None, ru, m).unwrap();
                        let x = t.b.shift_left_logical(t_u32, None, x, shc).unwrap();
                        ru = if unused == 2 {
                            let old = t.get_v(dst);
                            let keep = t.cu(!(mask << sh));
                            let o = t.b.bitwise_and(t_u32, None, old, keep).unwrap();
                            t.b.bitwise_or(t_u32, None, o, x).unwrap()
                        } else {
                            x
                        };
                    }
                    t.set_v(dst, ru);
                    continue;
                }
                let (a, bv, cv, dst, vop3) = match inst.fmt {
                    Fmt::Vop1 => (t.next_src((w0 & 0x1FF) as u16), t.cu(0), t.cu(0), (w0 >> 17) & 0xFF, false),
                    Fmt::Vop2 => {
                        let s0 = t.next_src((w0 & 0x1FF) as u16);
                        let v1 = t.get_v((w0 >> 9) & 0xFF);
                        let d = (w0 >> 17) & 0xFF;
                        if matches!(n, "v_madmk_f32" | "v_fmamk_f32") {
                            let k = t.next_slot();
                            (s0, k, v1, d, false)
                        } else if matches!(n, "v_madak_f32" | "v_fmaak_f32") {
                            let k = t.next_slot();
                            (s0, v1, k, d, false)
                        } else if n == "v_cndmask_b32" {
                            let m = t.next_slot();
                            (s0, v1, m, d, false)
                        } else {
                            (s0, v1, t.get_v(d), d, false)
                        }
                    }
                    _ => {
                        let s0 = t.next_src((w1 & 0x1FF) as u16);
                        let s1 = t.next_src(((w1 >> 9) & 0x1FF) as u16);
                        let s2 = if n == "v_cndmask_b32" {
                            t.next_slot()
                        } else {
                            t.next_src(((w1 >> 18) & 0x1FF) as u16)
                        };
                        (s0, s1, s2, w0 & 0xFF, true)
                    }
                };
                let (abs, neg, omod, clamp) = if vop3 {
                    ((w0 >> 8) & 7, (w1 >> 29) & 7, (w1 >> 27) & 3, w0 & (1 << 15) != 0)
                } else {
                    (0, 0, 0, false)
                };
                let is_float = matches!(
                    n,
                    "v_add_f32"
                        | "v_sub_f32"
                        | "v_subrev_f32"
                        | "v_mul_f32"
                        | "v_mul_legacy_f32"
                        | "v_mac_f32"
                        | "v_fmac_f32"
                        | "v_mad_f32"
                        | "v_madmk_f32"
                        | "v_madak_f32"
                        | "v_fma_f32"
                        | "v_fmamk_f32"
                        | "v_fmaak_f32"
                        | "v_max_f32"
                        | "v_min_f32"
                        | "v_rcp_f32"
                        | "v_rsq_f32"
                        | "v_sqrt_f32"
                        | "v_floor_f32"
                        | "v_fract_f32"
                );
                let out_u = if is_float {
                    let fa = t.fmod(a, abs & 1 != 0, neg & 1 != 0);
                    let fb = t.fmod(bv, abs & 2 != 0, neg & 2 != 0);
                    let fc = t.fmod(cv, abs & 4 != 0, neg & 4 != 0);
                    let rf = match n {
                        "v_add_f32" => t.b.f_add(t_f32, None, fa, fb).unwrap(),
                        "v_sub_f32" => t.b.f_sub(t_f32, None, fa, fb).unwrap(),
                        "v_subrev_f32" => t.b.f_sub(t_f32, None, fb, fa).unwrap(),
                        "v_mul_f32" | "v_mul_legacy_f32" => t.b.f_mul(t_f32, None, fa, fb).unwrap(),
                        "v_mac_f32" | "v_fmac_f32" | "v_mad_f32" | "v_madmk_f32" | "v_madak_f32" => {
                            let p = t.b.f_mul(t_f32, None, fa, fb).unwrap();
                            t.b.f_add(t_f32, None, p, fc).unwrap()
                        }
                        "v_fma_f32" | "v_fmamk_f32" | "v_fmaak_f32" => t.ext(t_f32, glsl::FMA, &[fa, fb, fc]),
                        "v_max_f32" => t.ext(t_f32, glsl::FMAX, &[fa, fb]),
                        "v_min_f32" => t.ext(t_f32, glsl::FMIN, &[fa, fb]),
                        "v_rcp_f32" => {
                            let o = t.cf(1.0);
                            t.b.f_div(t_f32, None, o, fa).unwrap()
                        }
                        "v_rsq_f32" => t.ext(t_f32, glsl::INVERSE_SQRT, &[fa]),
                        "v_sqrt_f32" => t.ext(t_f32, glsl::SQRT, &[fa]),
                        "v_floor_f32" => t.ext(t_f32, glsl::FLOOR, &[fa]),
                        "v_fract_f32" => t.ext(t_f32, glsl::FRACT, &[fa]),
                        _ => unreachable!(),
                    };
                    let rf = t.omod_clamp(rf, omod, clamp);
                    t.u(rf)
                } else {
                    match n {
                        "v_mov_b32" => a,
                        "v_cndmask_b32" => {
                            let zero = t.cu(0);
                            let cond = t.b.i_not_equal(t_bool, None, cv, zero).unwrap();
                            t.b.select(t_u32, None, cond, bv, a).unwrap()
                        }
                        "v_cvt_f32_u32" => {
                            let fv = t.b.convert_u_to_f(t_f32, None, a).unwrap();
                            t.u(fv)
                        }
                        "v_cvt_f32_i32" => {
                            let iv = t.b.bitcast(t_i32, None, a).unwrap();
                            let fv = t.b.convert_s_to_f(t_f32, None, iv).unwrap();
                            t.u(fv)
                        }
                        "v_cvt_u32_f32" => {
                            let fv = t.f(a);
                            t.b.convert_f_to_u(t_u32, None, fv).unwrap()
                        }
                        "v_cvt_i32_f32" => {
                            let fv = t.f(a);
                            let iv = t.b.convert_f_to_s(t_i32, None, fv).unwrap();
                            t.b.bitcast(t_u32, None, iv).unwrap()
                        }
                        "v_and_b32" => t.b.bitwise_and(t_u32, None, a, bv).unwrap(),
                        "v_or_b32" => t.b.bitwise_or(t_u32, None, a, bv).unwrap(),
                        "v_xor_b32" => t.b.bitwise_xor(t_u32, None, a, bv).unwrap(),
                        "v_lshlrev_b32" => {
                            let c31 = t.cu(31);
                            let sh = t.b.bitwise_and(t_u32, None, a, c31).unwrap();
                            t.b.shift_left_logical(t_u32, None, bv, sh).unwrap()
                        }
                        "v_lshrrev_b32" => {
                            let c31 = t.cu(31);
                            let sh = t.b.bitwise_and(t_u32, None, a, c31).unwrap();
                            t.b.shift_right_logical(t_u32, None, bv, sh).unwrap()
                        }
                        "v_add_nc_u32" | "v_add_u32" => t.b.i_add(t_u32, None, a, bv).unwrap(),
                        "v_sub_nc_u32" | "v_sub_u32" => t.b.i_sub(t_u32, None, a, bv).unwrap(),
                        "v_mul_lo_u32" => t.b.i_mul(t_u32, None, a, bv).unwrap(),
                        "v_bfe_u32" => {
                            let c31 = t.cu(31);
                            let off = t.b.bitwise_and(t_u32, None, bv, c31).unwrap();
                            let cnt = t.b.bitwise_and(t_u32, None, cv, c31).unwrap();
                            t.b.bit_field_u_extract(t_u32, None, a, off, cnt).unwrap()
                        }
                        _ => return Err(format!("vop {n}")),
                    }
                };
                t.set_v(dst, out_u);
            }
            Fmt::Exp => {
                let target = (w0 >> 4) & 0x3F;
                if target == 20 {
                    continue;
                }
                let compr = w0 & (1 << 10) != 0;
                let regs = [w1 & 0xFF, (w1 >> 8) & 0xFF, (w1 >> 16) & 0xFF, (w1 >> 24) & 0xFF];
                let vec4 = if compr {
                    let lo = t.get_v(regs[0]);
                    let hi = t.get_v(if regs[1] == regs[0] { regs[2] } else { regs[1] });
                    let p0 = t.ext(t_v2f, glsl::UNPACK_HALF2X16, &[lo]);
                    let p1 = t.ext(t_v2f, glsl::UNPACK_HALF2X16, &[hi]);
                    let c0 = t.b.composite_extract(t_f32, None, p0, [0]).unwrap();
                    let c1 = t.b.composite_extract(t_f32, None, p0, [1]).unwrap();
                    let c2 = t.b.composite_extract(t_f32, None, p1, [0]).unwrap();
                    let c3 = t.b.composite_extract(t_f32, None, p1, [1]).unwrap();
                    t.b.composite_construct(t_v4f, None, [c0, c1, c2, c3]).unwrap()
                } else {
                    let r0 = t.get_v(regs[0]);
                    let c0 = t.f(r0);
                    let r1 = t.get_v(regs[1]);
                    let c1 = t.f(r1);
                    let r2 = t.get_v(regs[2]);
                    let c2 = t.f(r2);
                    let r3 = t.get_v(regs[3]);
                    let c3 = t.f(r3);
                    t.b.composite_construct(t_v4f, None, [c0, c1, c2, c3]).unwrap()
                };
                if target == 12 {
                    // Apply viewport transform to_target(p) right in SPIR-V!
                    let px = t.b.composite_extract(t_f32, None, vec4, [0]).unwrap();
                    let py = t.b.composite_extract(t_f32, None, vec4, [1]).unwrap();
                    let pz = t.b.composite_extract(t_f32, None, vec4, [2]).unwrap();
                    let pw = t.b.composite_extract(t_f32, None, vec4, [3]).unwrap();
                    let xs_u = t.load_param(0);
                    let xs = t.f(xs_u);
                    let xo_u = t.load_param(1);
                    let xo = t.f(xo_u);
                    let ys_u = t.load_param(2);
                    let ys = t.f(ys_u);
                    let yo_u = t.load_param(3);
                    let yo = t.f(yo_u);
                    let inv_tw2_u = t.load_param(4);
                    let inv_tw2 = t.f(inv_tw2_u);
                    let inv_th2_u = t.load_param(5);
                    let inv_th2 = t.f(inv_th2_u);
                    let have_vp_u = t.load_param(6);
                    let cz0 = t.cu(0);
                    let have_vp = t.b.i_not_equal(t_bool, None, have_vp_u, cz0).unwrap();

                    let px_xs = t.b.f_mul(t_f32, None, px, xs).unwrap();
                    let pw_xo = t.b.f_mul(t_f32, None, pw, xo).unwrap();
                    let sx = t.b.f_add(t_f32, None, px_xs, pw_xo).unwrap();
                    let sx_s = t.b.f_mul(t_f32, None, sx, inv_tw2).unwrap();
                    let vx = t.b.f_sub(t_f32, None, sx_s, pw).unwrap();
                    let py_ys = t.b.f_mul(t_f32, None, py, ys).unwrap();
                    let pw_yo = t.b.f_mul(t_f32, None, pw, yo).unwrap();
                    let sy = t.b.f_add(t_f32, None, py_ys, pw_yo).unwrap();
                    let sy_s = t.b.f_mul(t_f32, None, sy, inv_th2).unwrap();
                    let vy = t.b.f_sub(t_f32, None, sy_s, pw).unwrap();
                    let neg_py = t.b.f_negate(t_f32, None, py).unwrap();
                    let fx = t.b.select(t_f32, None, have_vp, vx, px).unwrap();
                    let fy = t.b.select(t_f32, None, have_vp, vy, neg_py).unwrap();
                    let pos_v4 = t.b.composite_construct(t_v4f, None, [fx, fy, pz, pw]).unwrap();
                    t.b.store(pos_out, pos_v4, None, []).unwrap();
                } else if (32..96).contains(&target) {
                    let p = target - 32;
                    for n in 0..MAX_ATTRS {
                        if cntl[n] == p {
                            t.b.store(attr_outs[n], vec4, None, []).unwrap();
                        }
                    }
                }
            }
            _ => return Err(format!("fmt {:?}", inst.fmt)),
        }
    }

    t.b.ret().unwrap();
    t.b.end_function().unwrap();

    let mut iface = vec![v_idx_in, pos_out];
    iface.extend(attr_outs.iter().copied());
    t.b.entry_point(spirv::ExecutionModel::Vertex, main, "main", iface);
    let expected_slots = t.slot_idx;
    Ok((t.b.module().assemble(), expected_slots))
}

fn emit_load_format(t: &mut VsBuilder, vb_slot: usize, byte_addr: Word, fmt: u32) -> Result<[Word; 4], String> {
    let f_one = t.cu(1.0f32.to_bits());
    let u_one = t.cu(1);
    let zero = t.cu(0);
    let c4 = t.cu(4);
    let c8 = t.cu(8);
    let c12 = t.cu(12);
    let c2 = t.cu(2);
    let c6 = t.cu(6);
    let (t_u32, t_f32, t_v2f, t_v4f) = (t.t_u32, t.t_f32, t.t_v2f, t.t_v4f);

    let load_at = |t: &mut VsBuilder, off: Word| -> Word {
        let a = t.b.i_add(t_u32, None, byte_addr, off).unwrap();
        t.load_vb_unaligned_u32(vb_slot, a)
    };
    let ext_u = |t: &mut VsBuilder, vec: Word, idx: u32| -> Word {
        let f = t.b.composite_extract(t_f32, None, vec, [idx]).unwrap();
        t.u(f)
    };
    Ok(match fmt {
        22 => [t.load_vb_unaligned_u32(vb_slot, byte_addr), zero, zero, f_one],
        20 | 21 => [t.load_vb_unaligned_u32(vb_slot, byte_addr), zero, zero, u_one],
        64 => [t.load_vb_unaligned_u32(vb_slot, byte_addr), load_at(t, c4), zero, f_one],
        62 | 63 => [t.load_vb_unaligned_u32(vb_slot, byte_addr), load_at(t, c4), zero, u_one],
        74 => [t.load_vb_unaligned_u32(vb_slot, byte_addr), load_at(t, c4), load_at(t, c8), f_one],
        72 | 73 => [t.load_vb_unaligned_u32(vb_slot, byte_addr), load_at(t, c4), load_at(t, c8), u_one],
        75 | 76 | 77 => [t.load_vb_unaligned_u32(vb_slot, byte_addr), load_at(t, c4), load_at(t, c8), load_at(t, c12)],
        29 => {
            let w = t.load_vb_unaligned_u32(vb_slot, byte_addr);
            let v2 = t.ext(t_v2f, glsl::UNPACK_HALF2X16, &[w]);
            [ext_u(t, v2, 0), ext_u(t, v2, 1), zero, f_one]
        }
        71 => {
            let w0 = t.load_vb_unaligned_u32(vb_slot, byte_addr);
            let w1 = load_at(t, c4);
            let a = t.ext(t_v2f, glsl::UNPACK_HALF2X16, &[w0]);
            let b = t.ext(t_v2f, glsl::UNPACK_HALF2X16, &[w1]);
            [ext_u(t, a, 0), ext_u(t, a, 1), ext_u(t, b, 0), ext_u(t, b, 1)]
        }
        23 => {
            let w = t.load_vb_unaligned_u32(vb_slot, byte_addr);
            let v2 = t.ext(t_v2f, glsl::UNPACK_UNORM2X16, &[w]);
            [ext_u(t, v2, 0), ext_u(t, v2, 1), zero, f_one]
        }
        65 => {
            let w0 = t.load_vb_unaligned_u32(vb_slot, byte_addr);
            let w1 = load_at(t, c4);
            let a = t.ext(t_v2f, glsl::UNPACK_UNORM2X16, &[w0]);
            let b = t.ext(t_v2f, glsl::UNPACK_UNORM2X16, &[w1]);
            [ext_u(t, a, 0), ext_u(t, a, 1), ext_u(t, b, 0), ext_u(t, b, 1)]
        }
        24 => {
            let w = t.load_vb_unaligned_u32(vb_slot, byte_addr);
            let v2 = t.ext(t_v2f, glsl::UNPACK_SNORM2X16, &[w]);
            [ext_u(t, v2, 0), ext_u(t, v2, 1), zero, f_one]
        }
        66 => {
            let w0 = t.load_vb_unaligned_u32(vb_slot, byte_addr);
            let w1 = load_at(t, c4);
            let a = t.ext(t_v2f, glsl::UNPACK_SNORM2X16, &[w0]);
            let b = t.ext(t_v2f, glsl::UNPACK_SNORM2X16, &[w1]);
            [ext_u(t, a, 0), ext_u(t, a, 1), ext_u(t, b, 0), ext_u(t, b, 1)]
        }
        56 => {
            let w = t.load_vb_unaligned_u32(vb_slot, byte_addr);
            let v4 = t.ext(t_v4f, glsl::UNPACK_UNORM4X8, &[w]);
            [ext_u(t, v4, 0), ext_u(t, v4, 1), ext_u(t, v4, 2), ext_u(t, v4, 3)]
        }
        57 => {
            let w = t.load_vb_unaligned_u32(vb_slot, byte_addr);
            let v4 = t.ext(t_v4f, glsl::UNPACK_SNORM4X8, &[w]);
            [ext_u(t, v4, 0), ext_u(t, v4, 1), ext_u(t, v4, 2), ext_u(t, v4, 3)]
        }
        60 | 61 => {
            let w = t.load_vb_unaligned_u32(vb_slot, byte_addr);
            let ff = t.cu(0xFF);
            let c16 = t.cu(16);
            let c24 = t.cu(24);
            let b0 = t.b.bitwise_and(t_u32, None, w, ff).unwrap();
            let s1 = t.b.shift_right_logical(t_u32, None, w, c8).unwrap();
            let b1 = t.b.bitwise_and(t_u32, None, s1, ff).unwrap();
            let s2 = t.b.shift_right_logical(t_u32, None, w, c16).unwrap();
            let b2 = t.b.bitwise_and(t_u32, None, s2, ff).unwrap();
            let b3 = t.b.shift_right_logical(t_u32, None, w, c24).unwrap();
            [b0, b1, b2, b3]
        }
        11 | 12 => [t.load_vb_u16(vb_slot, byte_addr), zero, zero, u_one],
        27 | 28 => {
            let a1 = t.b.i_add(t_u32, None, byte_addr, c2).unwrap();
            [t.load_vb_u16(vb_slot, byte_addr), t.load_vb_u16(vb_slot, a1), zero, u_one]
        }
        69 | 70 => {
            let a1 = t.b.i_add(t_u32, None, byte_addr, c2).unwrap();
            let a2 = t.b.i_add(t_u32, None, byte_addr, c4).unwrap();
            let a3 = t.b.i_add(t_u32, None, byte_addr, c6).unwrap();
            [
                t.load_vb_u16(vb_slot, byte_addr),
                t.load_vb_u16(vb_slot, a1),
                t.load_vb_u16(vb_slot, a2),
                t.load_vb_u16(vb_slot, a3),
            ]
        }
        5 | 6 => [t.load_vb_u8(vb_slot, byte_addr), zero, zero, u_one],
        1 => {
            let b0 = t.load_vb_u8(vb_slot, byte_addr);
            let c255 = t.cf(255.0);
            let uf0 = t.b.convert_u_to_f(t_f32, None, b0).unwrap();
            let f0 = t.b.f_div(t_f32, None, uf0, c255).unwrap();
            [t.u(f0), zero, zero, f_one]
        }
        14 => {
            let b0 = t.load_vb_u8(vb_slot, byte_addr);
            let c1 = t.cu(1);
            let a1 = t.b.i_add(t_u32, None, byte_addr, c1).unwrap();
            let b1 = t.load_vb_u8(vb_slot, a1);
            let inv = t.cf(1.0 / 255.0);
            let uf0 = t.b.convert_u_to_f(t_f32, None, b0).unwrap();
            let uf1 = t.b.convert_u_to_f(t_f32, None, b1).unwrap();
            let f0 = t.b.f_mul(t_f32, None, uf0, inv).unwrap();
            let f1 = t.b.f_mul(t_f32, None, uf1, inv).unwrap();
            [t.u(f0), t.u(f1), zero, f_one]
        }
        _ => return Err(format!("vs buffer format {fmt}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_simple_ngg_vs_to_spirv() {
        // Minimal VS: exp pos0 v0..v3; s_endpgm
        let code = [0xf800_08cf, 0x0302_0100, 0xbf81_0000];
        let sh = CompiledShader::new(&code);
        assert!(is_gpu_vs_eligible(&sh));
        let (spv, slots) = translate(&sh, &[0, 1, 2, 3, 4, 5, 6, 7], &[0; MAX_MUBUFS]).expect("spirv");
        assert_eq!(spv[0], 0x0723_0203); // SPIR-V magic
        assert_eq!(slots, SLOT_BASE);
        assert!(spv.len() > 20);
    }
}
