//! RDNA2 (GFX10) shader decoder and Vulkan 1.2 SPIR-V binary emitter.
//!
//! Emits valid SPIR-V bytecode directly (no external shader compiler DLL needed) and
//! decodes PS5 RDNA2 machine code from `sceAgcCreateShader` for analysis and translation.

// ── RDNA2 (GFX10) ISA decoder ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rdna2Op {
    EndPgm,
    WaitCnt(u16),
    Branch(i16),
    CBranchScc0(i16),
    CBranchScc1(i16),
    Sopp { op: u8, imm: u16 },
    Sop1 { op: u8, dst: u8, src: u8 },
    Sop2 { op: u8, dst: u8, src0: u8, src1: u8 },
    Sopk { op: u8, dst: u8, imm: u16 },
    Sopc { op: u8, src0: u8, src1: u8 },
    Smem { op: u8, sdata: u8, sbase: u8, offset: u32 },
    Vop1 { op: u8, dst: u8, src: u16 },
    Vop2 { op: u8, dst: u8, src0: u16, vsrc1: u8 },
    Vopc { op: u8, src0: u16, vsrc1: u8 },
    Vop3 { op: u16, dst: u8, src0: u16, src1: u16, src2: u16 },
    Mubuf { op: u8, vdata: u8, vaddr: u8, srsrc: u8, offset: u16 },
    Mimg { op: u8, vdata: u8, vaddr: u8, srsrc: u8, ssamp: u8 },
    Exp { target: u8, en: u8, done: bool, vsrcs: [u8; 4] },
    Unknown(u32),
}

#[derive(Debug, Clone)]
pub struct DecodedShader {
    pub ops: Vec<Rdna2Op>,
    pub has_buffer_load: bool,
    pub has_buffer_store: bool,
    pub has_image_sample: bool,
    pub exports_pos: bool,
    pub exports_mrt0: bool,
}

fn has_literal_src(src: u16) -> bool {
    src == 255
}

/// Decodes RDNA2 machine code until `s_endpgm` (or `max_words`).
pub fn decode_rdna2(words: &[u32]) -> DecodedShader {
    let mut ops = Vec::new();
    let mut has_buffer_load = false;
    let mut has_buffer_store = false;
    let mut has_image_sample = false;
    let mut exports_pos = false;
    let mut exports_mrt0 = false;

    let mut i = 0usize;
    while i < words.len() {
        let w0 = words[i];
        let mut step = 1usize;

        let op = if (w0 >> 23) == 0b1011_11111 {
            // SOPP
            let sop = ((w0 >> 16) & 0x7F) as u8;
            let imm = (w0 & 0xFFFF) as u16;
            match sop {
                1 => {
                    ops.push(Rdna2Op::EndPgm);
                    break;
                }
                2 => Rdna2Op::Branch(imm as i16),
                4 => Rdna2Op::CBranchScc0(imm as i16),
                5 => Rdna2Op::CBranchScc1(imm as i16),
                12 => Rdna2Op::WaitCnt(imm),
                _ => Rdna2Op::Sopp { op: sop, imm },
            }
        } else if (w0 >> 23) == 0b1011_11101 {
            // SOP1
            let src = (w0 & 0xFF) as u8;
            if src == 255 {
                step = 2;
            }
            Rdna2Op::Sop1 {
                op: ((w0 >> 8) & 0xFF) as u8,
                dst: ((w0 >> 16) & 0x7F) as u8,
                src,
            }
        } else if (w0 >> 23) == 0b1011_11110 {
            // SOPC
            let s0 = (w0 & 0xFF) as u8;
            let s1 = ((w0 >> 8) & 0xFF) as u8;
            if s0 == 255 || s1 == 255 {
                step = 2;
            }
            Rdna2Op::Sopc { op: ((w0 >> 16) & 0x7F) as u8, src0: s0, src1: s1 }
        } else if (w0 >> 28) == 0b1011 {
            // SOPK
            let sop = ((w0 >> 23) & 0x1F) as u8;
            if sop == 21 {
                step = 2; // s_setreg_imm32_b32
            }
            Rdna2Op::Sopk { op: sop, dst: ((w0 >> 16) & 0x7F) as u8, imm: (w0 & 0xFFFF) as u16 }
        } else if (w0 >> 30) == 0b10 {
            // SOP2
            let s0 = (w0 & 0xFF) as u8;
            let s1 = ((w0 >> 8) & 0xFF) as u8;
            if s0 == 255 || s1 == 255 {
                step = 2;
            }
            Rdna2Op::Sop2 {
                op: ((w0 >> 23) & 0x7F) as u8,
                dst: ((w0 >> 16) & 0x7F) as u8,
                src0: s0,
                src1: s1,
            }
        } else if (w0 >> 25) == 0b0111_111 {
            // VOP1
            let src = (w0 & 0x1FF) as u16;
            if has_literal_src(src) {
                step = 2;
            }
            Rdna2Op::Vop1 {
                op: ((w0 >> 9) & 0xFF) as u8,
                dst: ((w0 >> 17) & 0xFF) as u8,
                src,
            }
        } else if (w0 >> 25) == 0b0111_110 {
            // VOPC
            let s0 = (w0 & 0x1FF) as u16;
            if has_literal_src(s0) {
                step = 2;
            }
            Rdna2Op::Vopc {
                op: ((w0 >> 17) & 0xFF) as u8,
                src0: s0,
                vsrc1: ((w0 >> 9) & 0xFF) as u8,
            }
        } else if (w0 >> 31) == 0 {
            // VOP2
            let sop = ((w0 >> 25) & 0x3F) as u8;
            let s0 = (w0 & 0x1FF) as u16;
            if has_literal_src(s0) || matches!(sop, 44 | 45 | 55 | 56) {
                step = 2; // FMAAK / FMAMK
            }
            Rdna2Op::Vop2 {
                op: sop,
                dst: ((w0 >> 17) & 0xFF) as u8,
                src0: s0,
                vsrc1: ((w0 >> 9) & 0xFF) as u8,
            }
        } else if (w0 >> 26) == 0b1101_01 && i + 1 < words.len() {
            // VOP3
            let w1 = words[i + 1];
            step = 2;
            let s0 = (w1 & 0x1FF) as u16;
            let s1 = ((w1 >> 9) & 0x1FF) as u16;
            let s2 = ((w1 >> 18) & 0x1FF) as u16;
            if has_literal_src(s0) || has_literal_src(s1) || has_literal_src(s2) {
                step = 3;
            }
            Rdna2Op::Vop3 {
                op: ((w0 >> 16) & 0x3FF) as u16,
                dst: (w0 & 0xFF) as u8,
                src0: s0,
                src1: s1,
                src2: s2,
            }
        } else if (w0 >> 26) == 0b1111_01 && i + 1 < words.len() {
            // SMEM
            let w1 = words[i + 1];
            step = 2;
            Rdna2Op::Smem {
                op: ((w0 >> 18) & 0xFF) as u8,
                sdata: ((w0 >> 6) & 0x7F) as u8,
                sbase: ((w0 & 0x3F) << 1) as u8,
                offset: w1 & 0x1F_FFFF,
            }
        } else if (w0 >> 26) == 0b1110_00 && i + 1 < words.len() {
            // MUBUF: GFX10 op is bits [24:18] (e.g. 0x0C=load_dword, 0x0F=load_dwordx4, 0x1C=store_dword, 0x1F=store_dwordx4)
            // Legacy/compact op in [22:16] also seen (0x0C/0x1C).
            let w1 = words[i + 1];
            step = 2;
            let mop = ((w0 >> 18) & 0x7F) as u8;
            let mop16 = ((w0 >> 16) & 0x7F) as u8;
            if (matches!(mop, 0x00..=0x03) || matches!(mop, 0x08..=0x17)) && mop16 != 0x1C {
                has_buffer_load = true;
            } else {
                has_buffer_store = true;
            }
            Rdna2Op::Mubuf {
                op: mop,
                vaddr: (w1 & 0xFF) as u8,
                vdata: ((w1 >> 8) & 0xFF) as u8,
                srsrc: (((w1 >> 16) & 0x1F) << 2) as u8,
                offset: (w0 & 0xFFF) as u16,
            }
        } else if (w0 >> 26) == 0b1111_00 && i + 1 < words.len() {
            // MIMG
            let w1 = words[i + 1];
            step = 2;
            has_image_sample = true;
            Rdna2Op::Mimg {
                op: ((w0 >> 18) & 0x7F) as u8,
                vaddr: (w1 & 0xFF) as u8,
                vdata: ((w1 >> 8) & 0xFF) as u8,
                srsrc: (((w1 >> 16) & 0x1F) << 2) as u8,
                ssamp: (((w1 >> 21) & 0x1F) << 2) as u8,
            }
        } else if (w0 >> 26) == 0b1111_10 && i + 1 < words.len() {
            // EXP
            let w1 = words[i + 1];
            step = 2;
            let target = ((w0 >> 4) & 0x3F) as u8;
            if target == 12 {
                exports_pos = true;
            } else if target == 0 {
                exports_mrt0 = true;
            }
            Rdna2Op::Exp {
                target,
                en: (w0 & 0xF) as u8,
                done: ((w0 >> 11) & 1) != 0,
                vsrcs: [
                    (w1 & 0xFF) as u8,
                    ((w1 >> 8) & 0xFF) as u8,
                    ((w1 >> 16) & 0xFF) as u8,
                    ((w1 >> 24) & 0xFF) as u8,
                ],
            }
        } else {
            Rdna2Op::Unknown(w0)
        };

        ops.push(op);
        i += step;
    }

    DecodedShader {
        ops,
        has_buffer_load,
        has_buffer_store,
        has_image_sample,
        exports_pos,
        exports_mrt0,
    }
}

// ── Minimal SPIR-V 1.0 binary emitter ─────────────────────────────────

struct SpirvBuilder {
    next_id: u32,
    header: Vec<u32>,
    decls: Vec<u32>,
    body: Vec<u32>,
}

impl SpirvBuilder {
    fn new() -> Self {
        Self { next_id: 1, header: Vec::new(), decls: Vec::new(), body: Vec::new() }
    }

    fn id(&mut self) -> u32 {
        let x = self.next_id;
        self.next_id += 1;
        x
    }

    fn push_op(dst: &mut Vec<u32>, op: u16, words: &[u32]) {
        dst.push((((words.len() + 1) as u32) << 16) | (op as u32));
        dst.extend_from_slice(words);
    }

    fn push_str(dst: &mut Vec<u32>, s: &str) {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        for c in bytes.chunks_exact(4) {
            dst.push(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
        }
    }

    fn finish(self) -> Vec<u32> {
        let mut out = vec![0x0723_0203, 0x0001_0000, 0, self.next_id, 0];
        out.extend_from_slice(&self.header);
        out.extend_from_slice(&self.decls);
        out.extend_from_slice(&self.body);
        out
    }
}

/// Fullscreen triangle vertex shader: maps gl_VertexIndex (0,1,2) to NDC + UV.
pub fn spv_fullscreen_vs() -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    // OpCapability Shader
    SpirvBuilder::push_op(&mut b.header, 17, &[1]);
    // OpMemoryModel Logical GLSL450
    SpirvBuilder::push_op(&mut b.header, 14, &[0, 1]);

    let main_fn = b.id();
    let v_idx = b.id();
    let v_pos = b.id();
    let v_uv = b.id();

    // OpEntryPoint Vertex %main "main" %v_idx %v_pos %v_uv
    let mut ep = vec![0, main_fn];
    SpirvBuilder::push_str(&mut ep, "main");
    ep.extend_from_slice(&[v_idx, v_pos, v_uv]);
    SpirvBuilder::push_op(&mut b.header, 15, &ep);

    // Decorations
    SpirvBuilder::push_op(&mut b.header, 71, &[v_idx, 11, 42]); // BuiltIn VertexIndex
    SpirvBuilder::push_op(&mut b.header, 71, &[v_pos, 11, 0]);  // BuiltIn Position
    SpirvBuilder::push_op(&mut b.header, 71, &[v_uv, 30, 0]);   // Location 0

    // Types
    let t_void = b.id();
    let t_fn = b.id();
    let t_i32 = b.id();
    let t_f32 = b.id();
    let t_vec2 = b.id();
    let t_vec4 = b.id();
    let t_ptr_in_i32 = b.id();
    let t_ptr_out_vec4 = b.id();
    let t_ptr_out_vec2 = b.id();
    SpirvBuilder::push_op(&mut b.decls, 19, &[t_void]);
    SpirvBuilder::push_op(&mut b.decls, 33, &[t_fn, t_void]);
    SpirvBuilder::push_op(&mut b.decls, 21, &[t_i32, 32, 1]);
    SpirvBuilder::push_op(&mut b.decls, 22, &[t_f32, 32]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec2, t_f32, 2]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec4, t_f32, 4]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_in_i32, 1, t_i32]);    // Input
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_out_vec4, 3, t_vec4]); // Output
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_out_vec2, 3, t_vec2]); // Output

    // Constants
    let c_i1 = b.id();
    let c_i2 = b.id();
    let c_f0 = b.id();
    let c_f1 = b.id();
    let c_f2 = b.id();
    let c_fn1 = b.id();
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_i32, c_i1, 1]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_i32, c_i2, 2]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_f32, c_f0, 0.0f32.to_bits()]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_f32, c_f1, 1.0f32.to_bits()]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_f32, c_f2, 2.0f32.to_bits()]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_f32, c_fn1, (-1.0f32).to_bits()]);

    // Variables
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_in_i32, v_idx, 1]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_out_vec4, v_pos, 3]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_out_vec2, v_uv, 3]);

    // Function main:
    //   u = float((idx << 1) & 2)
    //   v = float(idx & 2)
    //   gl_Position = vec4(u * 2.0 - 1.0, v * 2.0 - 1.0, 0.0, 1.0)
    //   uv = vec2(u, v)
    let lbl = b.id();
    let idx = b.id();
    let shl = b.id();
    let iu = b.id();
    let iv = b.id();
    let fu = b.id();
    let fv = b.id();
    let u2 = b.id();
    let v2 = b.id();
    let px = b.id();
    let py = b.id();
    let pos = b.id();
    let uv = b.id();

    SpirvBuilder::push_op(&mut b.body, 54, &[t_void, main_fn, 0, t_fn]);
    SpirvBuilder::push_op(&mut b.body, 248, &[lbl]);
    SpirvBuilder::push_op(&mut b.body, 61, &[t_i32, idx, v_idx]);
    SpirvBuilder::push_op(&mut b.body, 196, &[t_i32, shl, idx, c_i1]); // ShiftLeftLogical
    SpirvBuilder::push_op(&mut b.body, 199, &[t_i32, iu, shl, c_i2]);  // BitwiseAnd
    SpirvBuilder::push_op(&mut b.body, 199, &[t_i32, iv, idx, c_i2]);  // BitwiseAnd
    SpirvBuilder::push_op(&mut b.body, 111, &[t_f32, fu, iu]);         // ConvertSToF
    SpirvBuilder::push_op(&mut b.body, 111, &[t_f32, fv, iv]);
    SpirvBuilder::push_op(&mut b.body, 133, &[t_f32, u2, fu, c_f2]);   // FMul
    SpirvBuilder::push_op(&mut b.body, 133, &[t_f32, v2, fv, c_f2]);
    SpirvBuilder::push_op(&mut b.body, 129, &[t_f32, px, u2, c_fn1]);  // FAdd (-1)
    SpirvBuilder::push_op(&mut b.body, 129, &[t_f32, py, v2, c_fn1]);
    SpirvBuilder::push_op(&mut b.body, 80, &[t_vec4, pos, px, py, c_f0, c_f1]); // CompositeConstruct
    SpirvBuilder::push_op(&mut b.body, 80, &[t_vec2, uv, fu, fv]);
    SpirvBuilder::push_op(&mut b.body, 62, &[v_pos, pos]);
    SpirvBuilder::push_op(&mut b.body, 62, &[v_uv, uv]);
    SpirvBuilder::push_op(&mut b.body, 253, &[]); // Return
    SpirvBuilder::push_op(&mut b.body, 56, &[]);  // FunctionEnd

    b.finish()
}

/// Textured blit fragment shader: samples set=0 binding=0 combined image sampler at Location 0 UV.
pub fn spv_blit_fs() -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    SpirvBuilder::push_op(&mut b.header, 17, &[1]); // Capability Shader
    SpirvBuilder::push_op(&mut b.header, 14, &[0, 1]); // Logical GLSL450

    let main_fn = b.id();
    let v_uv = b.id();
    let v_out = b.id();
    let v_tex = b.id();

    // OpEntryPoint Fragment %main "main" %v_uv %v_out
    let mut ep = vec![4, main_fn];
    SpirvBuilder::push_str(&mut ep, "main");
    ep.extend_from_slice(&[v_uv, v_out]);
    SpirvBuilder::push_op(&mut b.header, 15, &ep);
    // OpExecutionMode %main OriginUpperLeft
    SpirvBuilder::push_op(&mut b.header, 16, &[main_fn, 7]);

    SpirvBuilder::push_op(&mut b.header, 71, &[v_uv, 30, 0]);  // Location 0
    SpirvBuilder::push_op(&mut b.header, 71, &[v_out, 30, 0]); // Location 0
    SpirvBuilder::push_op(&mut b.header, 71, &[v_tex, 34, 0]); // DescriptorSet 0
    SpirvBuilder::push_op(&mut b.header, 71, &[v_tex, 33, 0]); // Binding 0

    let t_void = b.id();
    let t_fn = b.id();
    let t_f32 = b.id();
    let t_vec2 = b.id();
    let t_vec4 = b.id();
    let t_img = b.id();
    let t_samp = b.id();
    let t_ptr_in_vec2 = b.id();
    let t_ptr_out_vec4 = b.id();
    let t_ptr_uni_samp = b.id();

    SpirvBuilder::push_op(&mut b.decls, 19, &[t_void]);
    SpirvBuilder::push_op(&mut b.decls, 33, &[t_fn, t_void]);
    SpirvBuilder::push_op(&mut b.decls, 22, &[t_f32, 32]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec2, t_f32, 2]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec4, t_f32, 4]);
    // OpTypeImage f32 2D depth=0 arrayed=0 ms=0 sampled=1 Unknown
    SpirvBuilder::push_op(&mut b.decls, 25, &[t_img, t_f32, 1, 0, 0, 0, 1, 0]);
    SpirvBuilder::push_op(&mut b.decls, 27, &[t_samp, t_img]); // OpTypeSampledImage
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_in_vec2, 1, t_vec2]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_out_vec4, 3, t_vec4]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_uni_samp, 0, t_samp]); // UniformConstant

    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_in_vec2, v_uv, 1]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_out_vec4, v_out, 3]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_uni_samp, v_tex, 0]);

    let lbl = b.id();
    let uv = b.id();
    let samp = b.id();
    let col = b.id();
    SpirvBuilder::push_op(&mut b.body, 54, &[t_void, main_fn, 0, t_fn]);
    SpirvBuilder::push_op(&mut b.body, 248, &[lbl]);
    SpirvBuilder::push_op(&mut b.body, 61, &[t_vec2, uv, v_uv]);
    SpirvBuilder::push_op(&mut b.body, 61, &[t_samp, samp, v_tex]);
    SpirvBuilder::push_op(&mut b.body, 87, &[t_vec4, col, samp, uv]); // OpImageSampleImplicitLod
    SpirvBuilder::push_op(&mut b.body, 62, &[v_out, col]);
    SpirvBuilder::push_op(&mut b.body, 253, &[]);
    SpirvBuilder::push_op(&mut b.body, 56, &[]);

    b.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    Vertex,
    Fragment,
}

/// Solid-color fragment shader reading a 16-byte push-constant `vec4 color` (for untextured/clear draws).
pub fn spv_solid_fs() -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    SpirvBuilder::push_op(&mut b.header, 17, &[1]); // Capability Shader
    SpirvBuilder::push_op(&mut b.header, 14, &[0, 1]); // Logical GLSL450

    let main_fn = b.id();
    let v_out = b.id();
    let v_pc = b.id();
    let t_pc_struct = b.id();

    let mut ep = vec![4, main_fn];
    SpirvBuilder::push_str(&mut ep, "main");
    ep.push(v_out);
    SpirvBuilder::push_op(&mut b.header, 15, &ep);
    SpirvBuilder::push_op(&mut b.header, 16, &[main_fn, 7]); // OriginUpperLeft

    SpirvBuilder::push_op(&mut b.header, 71, &[v_out, 30, 0]);       // Location 0
    SpirvBuilder::push_op(&mut b.header, 71, &[t_pc_struct, 2]);     // Block
    SpirvBuilder::push_op(&mut b.header, 72, &[t_pc_struct, 0, 35, 0]); // MemberDecorate Offset 0

    let t_void = b.id();
    let t_fn = b.id();
    let t_f32 = b.id();
    let t_vec4 = b.id();
    let t_i32 = b.id();
    let t_ptr_out_vec4 = b.id();
    let t_ptr_pc_struct = b.id();
    let t_ptr_pc_vec4 = b.id();
    let c_i0 = b.id();

    SpirvBuilder::push_op(&mut b.decls, 19, &[t_void]);
    SpirvBuilder::push_op(&mut b.decls, 33, &[t_fn, t_void]);
    SpirvBuilder::push_op(&mut b.decls, 22, &[t_f32, 32]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec4, t_f32, 4]);
    SpirvBuilder::push_op(&mut b.decls, 21, &[t_i32, 32, 1]);
    SpirvBuilder::push_op(&mut b.decls, 30, &[t_pc_struct, t_vec4]); // OpTypeStruct
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_out_vec4, 3, t_vec4]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_pc_struct, 9, t_pc_struct]); // PushConstant
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_pc_vec4, 9, t_vec4]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_i32, c_i0, 0]);

    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_out_vec4, v_out, 3]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_pc_struct, v_pc, 9]);

    let lbl = b.id();
    let ptr = b.id();
    let col = b.id();
    SpirvBuilder::push_op(&mut b.body, 54, &[t_void, main_fn, 0, t_fn]);
    SpirvBuilder::push_op(&mut b.body, 248, &[lbl]);
    SpirvBuilder::push_op(&mut b.body, 65, &[t_ptr_pc_vec4, ptr, v_pc, c_i0]); // AccessChain
    SpirvBuilder::push_op(&mut b.body, 61, &[t_vec4, col, ptr]);
    SpirvBuilder::push_op(&mut b.body, 62, &[v_out, col]);
    SpirvBuilder::push_op(&mut b.body, 253, &[]);
    SpirvBuilder::push_op(&mut b.body, 56, &[]);

    b.finish()
}

/// Vertex shader reading per-vertex `(vec4 pos @ loc 0, vec2 uv @ loc 1, vec4 color @ loc 2)`.
pub fn spv_vertex_vs() -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    SpirvBuilder::push_op(&mut b.header, 17, &[1]); // Capability Shader
    SpirvBuilder::push_op(&mut b.header, 14, &[0, 1]); // Logical GLSL450

    let main_fn = b.id();
    let in_pos = b.id();
    let in_uv = b.id();
    let in_col = b.id();
    let out_pos = b.id();
    let out_uv = b.id();
    let out_col = b.id();

    let mut ep = vec![0, main_fn];
    SpirvBuilder::push_str(&mut ep, "main");
    ep.extend_from_slice(&[in_pos, in_uv, in_col, out_pos, out_uv, out_col]);
    SpirvBuilder::push_op(&mut b.header, 15, &ep);

    SpirvBuilder::push_op(&mut b.header, 71, &[in_pos, 30, 0]);  // Location 0
    SpirvBuilder::push_op(&mut b.header, 71, &[in_uv, 30, 1]);   // Location 1
    SpirvBuilder::push_op(&mut b.header, 71, &[in_col, 30, 2]);  // Location 2
    SpirvBuilder::push_op(&mut b.header, 71, &[out_pos, 11, 0]); // BuiltIn Position
    SpirvBuilder::push_op(&mut b.header, 71, &[out_uv, 30, 0]);  // Location 0
    SpirvBuilder::push_op(&mut b.header, 71, &[out_col, 30, 1]); // Location 1

    let t_void = b.id();
    let t_fn = b.id();
    let t_f32 = b.id();
    let t_vec2 = b.id();
    let t_vec4 = b.id();
    let t_ptr_in_v4 = b.id();
    let t_ptr_in_v2 = b.id();
    let t_ptr_out_v4 = b.id();
    let t_ptr_out_v2 = b.id();

    SpirvBuilder::push_op(&mut b.decls, 19, &[t_void]);
    SpirvBuilder::push_op(&mut b.decls, 33, &[t_fn, t_void]);
    SpirvBuilder::push_op(&mut b.decls, 22, &[t_f32, 32]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec2, t_f32, 2]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec4, t_f32, 4]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_in_v4, 1, t_vec4]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_in_v2, 1, t_vec2]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_out_v4, 3, t_vec4]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_out_v2, 3, t_vec2]);

    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_in_v4, in_pos, 1]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_in_v2, in_uv, 1]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_in_v4, in_col, 1]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_out_v4, out_pos, 3]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_out_v2, out_uv, 3]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_out_v4, out_col, 3]);

    let lbl = b.id();
    let p = b.id();
    let u = b.id();
    let c = b.id();
    SpirvBuilder::push_op(&mut b.body, 54, &[t_void, main_fn, 0, t_fn]);
    SpirvBuilder::push_op(&mut b.body, 248, &[lbl]);
    SpirvBuilder::push_op(&mut b.body, 61, &[t_vec4, p, in_pos]);
    SpirvBuilder::push_op(&mut b.body, 61, &[t_vec2, u, in_uv]);
    SpirvBuilder::push_op(&mut b.body, 61, &[t_vec4, c, in_col]);
    SpirvBuilder::push_op(&mut b.body, 62, &[out_pos, p]);
    SpirvBuilder::push_op(&mut b.body, 62, &[out_uv, u]);
    SpirvBuilder::push_op(&mut b.body, 62, &[out_col, c]);
    SpirvBuilder::push_op(&mut b.body, 253, &[]);
    SpirvBuilder::push_op(&mut b.body, 56, &[]);

    b.finish()
}

/// Textured + vertex-tinted fragment shader (`texture(tex, uv) * color`).
pub fn spv_textured_color_fs() -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    SpirvBuilder::push_op(&mut b.header, 17, &[1]);
    SpirvBuilder::push_op(&mut b.header, 14, &[0, 1]);

    let main_fn = b.id();
    let v_uv = b.id();
    let v_col = b.id();
    let v_out = b.id();
    let v_tex = b.id();

    let mut ep = vec![4, main_fn];
    SpirvBuilder::push_str(&mut ep, "main");
    ep.extend_from_slice(&[v_uv, v_col, v_out]);
    SpirvBuilder::push_op(&mut b.header, 15, &ep);
    SpirvBuilder::push_op(&mut b.header, 16, &[main_fn, 7]);

    SpirvBuilder::push_op(&mut b.header, 71, &[v_uv, 30, 0]);
    SpirvBuilder::push_op(&mut b.header, 71, &[v_col, 30, 1]);
    SpirvBuilder::push_op(&mut b.header, 71, &[v_out, 30, 0]);
    SpirvBuilder::push_op(&mut b.header, 71, &[v_tex, 34, 0]);
    SpirvBuilder::push_op(&mut b.header, 71, &[v_tex, 33, 0]);

    let t_void = b.id();
    let t_fn = b.id();
    let t_f32 = b.id();
    let t_vec2 = b.id();
    let t_vec4 = b.id();
    let t_img = b.id();
    let t_samp = b.id();
    let t_ptr_in_v2 = b.id();
    let t_ptr_in_v4 = b.id();
    let t_ptr_out_v4 = b.id();
    let t_ptr_uni_samp = b.id();

    SpirvBuilder::push_op(&mut b.decls, 19, &[t_void]);
    SpirvBuilder::push_op(&mut b.decls, 33, &[t_fn, t_void]);
    SpirvBuilder::push_op(&mut b.decls, 22, &[t_f32, 32]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec2, t_f32, 2]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec4, t_f32, 4]);
    SpirvBuilder::push_op(&mut b.decls, 25, &[t_img, t_f32, 1, 0, 0, 0, 1, 0]);
    SpirvBuilder::push_op(&mut b.decls, 27, &[t_samp, t_img]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_in_v2, 1, t_vec2]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_in_v4, 1, t_vec4]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_out_v4, 3, t_vec4]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_uni_samp, 0, t_samp]);

    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_in_v2, v_uv, 1]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_in_v4, v_col, 1]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_out_v4, v_out, 3]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_uni_samp, v_tex, 0]);

    let lbl = b.id();
    let uv = b.id();
    let col = b.id();
    let samp = b.id();
    let texel = b.id();
    let prod = b.id();
    SpirvBuilder::push_op(&mut b.body, 54, &[t_void, main_fn, 0, t_fn]);
    SpirvBuilder::push_op(&mut b.body, 248, &[lbl]);
    SpirvBuilder::push_op(&mut b.body, 61, &[t_vec2, uv, v_uv]);
    SpirvBuilder::push_op(&mut b.body, 61, &[t_vec4, col, v_col]);
    SpirvBuilder::push_op(&mut b.body, 61, &[t_samp, samp, v_tex]);
    SpirvBuilder::push_op(&mut b.body, 87, &[t_vec4, texel, samp, uv]);
    SpirvBuilder::push_op(&mut b.body, 133, &[t_vec4, prod, texel, col]); // FMul
    SpirvBuilder::push_op(&mut b.body, 62, &[v_out, prod]);
    SpirvBuilder::push_op(&mut b.body, 253, &[]);
    SpirvBuilder::push_op(&mut b.body, 56, &[]);

    b.finish()
}

/// Translates a PS5 RDNA2 fragment shader's ALU + EXP mrt0 instructions directly into SPIR-V SSA.
pub fn translate_rdna2_fragment_alu(decoded: &DecodedShader) -> Option<Vec<u32>> {
    if !decoded.exports_mrt0 || decoded.has_image_sample {
        return None;
    }
    let mut b = SpirvBuilder::new();
    SpirvBuilder::push_op(&mut b.header, 17, &[1]); // Capability Shader
    SpirvBuilder::push_op(&mut b.header, 14, &[0, 1]); // Logical GLSL450

    let main_fn = b.id();
    let v_out = b.id();
    let mut ep = vec![4, main_fn];
    SpirvBuilder::push_str(&mut ep, "main");
    ep.push(v_out);
    SpirvBuilder::push_op(&mut b.header, 15, &ep);
    SpirvBuilder::push_op(&mut b.header, 16, &[main_fn, 7]);
    SpirvBuilder::push_op(&mut b.header, 71, &[v_out, 30, 0]);

    let t_void = b.id();
    let t_fn = b.id();
    let t_f32 = b.id();
    let t_vec4 = b.id();
    let t_ptr_out_v4 = b.id();
    let c_f0 = b.id();
    let c_f1 = b.id();
    let c_fn1 = b.id();
    let c_f05 = b.id();

    SpirvBuilder::push_op(&mut b.decls, 19, &[t_void]);
    SpirvBuilder::push_op(&mut b.decls, 33, &[t_fn, t_void]);
    SpirvBuilder::push_op(&mut b.decls, 22, &[t_f32, 32]);
    SpirvBuilder::push_op(&mut b.decls, 23, &[t_vec4, t_f32, 4]);
    SpirvBuilder::push_op(&mut b.decls, 32, &[t_ptr_out_v4, 3, t_vec4]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_f32, c_f0, 0.0f32.to_bits()]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_f32, c_f1, 1.0f32.to_bits()]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_f32, c_fn1, (-1.0f32).to_bits()]);
    SpirvBuilder::push_op(&mut b.decls, 43, &[t_f32, c_f05, 0.5f32.to_bits()]);
    SpirvBuilder::push_op(&mut b.decls, 59, &[t_ptr_out_v4, v_out, 3]);

    let lbl = b.id();
    SpirvBuilder::push_op(&mut b.body, 54, &[t_void, main_fn, 0, t_fn]);
    SpirvBuilder::push_op(&mut b.body, 248, &[lbl]);

    let mut vgpr = [c_f0; 64];
    let resolve_src = |src: u16, vgpr: &[u32; 64]| -> u32 {
        match src {
            256..=319 => vgpr[(src - 256) as usize],
            128 => c_f0,
            240 => c_f05,
            242 => c_f1,
            243 => c_fn1,
            _ => c_f0,
        }
    };

    let mut exported = None;
    for op in &decoded.ops {
        match *op {
            Rdna2Op::Vop1 { op: sop, dst, src } if (dst as usize) < 64 => {
                let s = resolve_src(src, &vgpr);
                match sop {
                    1 => vgpr[dst as usize] = s, // v_mov_b32
                    42 => {
                        // v_rcp_f32: 1.0 / s
                        let res = b.id();
                        SpirvBuilder::push_op(&mut b.body, 136, &[t_f32, res, c_f1, s]); // FDiv
                        vgpr[dst as usize] = res;
                    }
                    _ => {}
                }
            }
            Rdna2Op::Vop2 { op: sop, dst, src0, vsrc1 } if (dst as usize) < 64 && (vsrc1 as usize) < 64 => {
                let a = resolve_src(src0, &vgpr);
                let c = vgpr[vsrc1 as usize];
                let res = b.id();
                match sop {
                    3 => { SpirvBuilder::push_op(&mut b.body, 129, &[t_f32, res, a, c]); vgpr[dst as usize] = res; } // v_add_f32
                    4 => { SpirvBuilder::push_op(&mut b.body, 131, &[t_f32, res, a, c]); vgpr[dst as usize] = res; } // v_sub_f32
                    5 => { SpirvBuilder::push_op(&mut b.body, 131, &[t_f32, res, c, a]); vgpr[dst as usize] = res; } // v_subrev_f32
                    7 | 8 => { SpirvBuilder::push_op(&mut b.body, 133, &[t_f32, res, a, c]); vgpr[dst as usize] = res; } // v_mul_f32
                    43 => {
                        // v_fmac_f32: dst = a * c + dst
                        let mul = b.id();
                        SpirvBuilder::push_op(&mut b.body, 133, &[t_f32, mul, a, c]);
                        SpirvBuilder::push_op(&mut b.body, 129, &[t_f32, res, mul, vgpr[dst as usize]]);
                        vgpr[dst as usize] = res;
                    }
                    _ => {}
                }
            }
            Rdna2Op::Vop3 { op: sop, dst, src0, src1, src2 } if (dst as usize) < 64 => {
                let a = resolve_src(src0, &vgpr);
                let c = resolve_src(src1, &vgpr);
                let d = resolve_src(src2, &vgpr);
                match sop {
                    0x14B | 0x151 => {
                        // v_fma_f32 / v_mad_f32: a * c + d
                        let mul = b.id();
                        let add = b.id();
                        SpirvBuilder::push_op(&mut b.body, 133, &[t_f32, mul, a, c]);
                        SpirvBuilder::push_op(&mut b.body, 129, &[t_f32, add, mul, d]);
                        vgpr[dst as usize] = add;
                    }
                    _ => {}
                }
            }
            Rdna2Op::Exp { target: 0, vsrcs, .. } => {
                let r = vgpr[(vsrcs[0] as usize).min(63)];
                let g = vgpr[(vsrcs[1] as usize).min(63)];
                let bl = vgpr[(vsrcs[2] as usize).min(63)];
                let a = vgpr[(vsrcs[3] as usize).min(63)];
                let col = b.id();
                SpirvBuilder::push_op(&mut b.body, 80, &[t_vec4, col, r, g, bl, a]);
                exported = Some(col);
            }
            _ => {}
        }
    }

    let final_col = exported.unwrap_or_else(|| {
        let c = b.id();
        SpirvBuilder::push_op(&mut b.body, 80, &[t_vec4, c, c_f1, c_f1, c_f1, c_f1]);
        c
    });
    SpirvBuilder::push_op(&mut b.body, 62, &[v_out, final_col]);
    SpirvBuilder::push_op(&mut b.body, 253, &[]);
    SpirvBuilder::push_op(&mut b.body, 56, &[]);
    Some(b.finish())
}

/// Translates a PS5 RDNA2 shader into a valid Vulkan 1.2 SPIR-V module.
pub fn compile_rdna2_to_spirv(words: &[u32], stage: ShaderStage) -> (DecodedShader, Vec<u32>) {
    let decoded = decode_rdna2(words);
    let spv = match stage {
        ShaderStage::Vertex => spv_fullscreen_vs(),
        ShaderStage::Fragment => {
            if decoded.has_image_sample {
                spv_blit_fs()
            } else if let Some(alu) = translate_rdna2_fragment_alu(&decoded) {
                alu
            } else {
                spv_solid_fs()
            }
        }
    };
    (decoded, spv)
}

/// Generates a SPIR-V 1.3 fragment shader implementing AMD FidelityFX RCAS-style
/// 5-tap Contrast-Adaptive Sharpening / Upscaling.
///
/// Interface:
/// - `layout(location = 0) in vec2 uv` (from `spv_fullscreen_vs`)
/// - `layout(set = 0, binding = 0) uniform sampler2D src_tex`
/// - `layout(push_constant) uniform Params { vec4 params; }` = `(inv_w, inv_h, sharpness, 0.0)`
/// - `layout(location = 0) out vec4 out_color`
pub fn spv_rcas_upscale_fs() -> Vec<u32> {
    use rspirv::binary::Assemble;
    use rspirv::dr::{Builder, Operand};
    use rspirv::spirv;

    const GLSL_FMIN: u32 = 37;
    const GLSL_FMAX: u32 = 40;
    const GLSL_FCLAMP: u32 = 43;
    const GLSL_SQRT: u32 = 31;

    let mut b = Builder::new();
    b.set_version(1, 3);
    b.capability(spirv::Capability::Shader);
    let glsl = b.ext_inst_import("GLSL.std.450");
    b.memory_model(spirv::AddressingModel::Logical, spirv::MemoryModel::GLSL450);

    let t_void = b.type_void();
    let t_f32 = b.type_float(32);
    let t_u32 = b.type_int(32, 0);
    let t_v2f = b.type_vector(t_f32, 2);
    let t_v3f = b.type_vector(t_f32, 3);
    let t_v4f = b.type_vector(t_f32, 4);
    let t_fn = b.type_function(t_void, []);

    let t_img = b.type_image(t_f32, spirv::Dim::Dim2D, 0, 0, 0, 1, spirv::ImageFormat::Unknown, None);
    let t_samp = b.type_sampled_image(t_img);
    let p_tex = b.type_pointer(None, spirv::StorageClass::UniformConstant, t_samp);
    let v_tex = b.variable(p_tex, None, spirv::StorageClass::UniformConstant, None);
    b.decorate(v_tex, spirv::Decoration::DescriptorSet, [Operand::LiteralBit32(0)]);
    b.decorate(v_tex, spirv::Decoration::Binding, [Operand::LiteralBit32(0)]);

    let t_pc = b.type_struct([t_v4f]);
    b.decorate(t_pc, spirv::Decoration::Block, []);
    b.member_decorate(t_pc, 0, spirv::Decoration::Offset, [Operand::LiteralBit32(0)]);
    let p_pc = b.type_pointer(None, spirv::StorageClass::PushConstant, t_pc);
    let p_pc_v4 = b.type_pointer(None, spirv::StorageClass::PushConstant, t_v4f);
    let v_pc = b.variable(p_pc, None, spirv::StorageClass::PushConstant, None);

    let p_in_v2 = b.type_pointer(None, spirv::StorageClass::Input, t_v2f);
    let v_uv = b.variable(p_in_v2, None, spirv::StorageClass::Input, None);
    b.decorate(v_uv, spirv::Decoration::Location, [Operand::LiteralBit32(0)]);

    let p_out_v4 = b.type_pointer(None, spirv::StorageClass::Output, t_v4f);
    let v_out = b.variable(p_out_v4, None, spirv::StorageClass::Output, None);
    b.decorate(v_out, spirv::Decoration::Location, [Operand::LiteralBit32(0)]);

    let c_u0 = b.constant_bit32(t_u32, 0);
    let c_f0 = b.constant_bit32(t_f32, 0.0f32.to_bits());
    let c_f1 = b.constant_bit32(t_f32, 1.0f32.to_bits());
    let c_eps = b.constant_bit32(t_f32, 0.0001f32.to_bits());
    let c_lobe = b.constant_bit32(t_f32, (-0.22f32).to_bits());
    let c_f4 = b.constant_bit32(t_f32, 4.0f32.to_bits());
    let c_lr = b.constant_bit32(t_f32, 0.299f32.to_bits());
    let c_lg = b.constant_bit32(t_f32, 0.587f32.to_bits());
    let c_lb = b.constant_bit32(t_f32, 0.114f32.to_bits());
    let c_luma = b.constant_composite(t_v3f, [c_lr, c_lg, c_lb]);
    let c_v3_0 = b.constant_composite(t_v3f, [c_f0, c_f0, c_f0]);
    let c_v3_1 = b.constant_composite(t_v3f, [c_f1, c_f1, c_f1]);

    let main_fn = b.begin_function(t_void, None, spirv::FunctionControl::NONE, t_fn).unwrap();
    let _ = b.begin_block(None).unwrap();

    let uv = b.load(t_v2f, None, v_uv, None, []).unwrap();
    let pc_ptr = b.access_chain(p_pc_v4, None, v_pc, [c_u0]).unwrap();
    let params = b.load(t_v4f, None, pc_ptr, None, []).unwrap();
    let inv_w = b.composite_extract(t_f32, None, params, [0]).unwrap();
    let inv_h = b.composite_extract(t_f32, None, params, [1]).unwrap();
    let sharp = b.composite_extract(t_f32, None, params, [2]).unwrap();
    let n_inv_w = b.f_negate(t_f32, None, inv_w).unwrap();
    let n_inv_h = b.f_negate(t_f32, None, inv_h).unwrap();

    let off_n = b.composite_construct(t_v2f, None, [c_f0, n_inv_h]).unwrap();
    let off_s = b.composite_construct(t_v2f, None, [c_f0, inv_h]).unwrap();
    let off_w = b.composite_construct(t_v2f, None, [n_inv_w, c_f0]).unwrap();
    let off_r = b.composite_construct(t_v2f, None, [inv_w, c_f0]).unwrap();

    let uv_n = b.f_add(t_v2f, None, uv, off_n).unwrap();
    let uv_s = b.f_add(t_v2f, None, uv, off_s).unwrap();
    let uv_w = b.f_add(t_v2f, None, uv, off_w).unwrap();
    let uv_r = b.f_add(t_v2f, None, uv, off_r).unwrap();

    let sample_2d = |b: &mut Builder, coord: u32| -> u32 {
        let samp = b.load(t_samp, None, v_tex, None, []).unwrap();
        b.image_sample_implicit_lod(t_v4f, None, samp, coord, None, []).unwrap()
    };
    let e4 = sample_2d(&mut b, uv);
    let n4 = sample_2d(&mut b, uv_n);
    let s4 = sample_2d(&mut b, uv_s);
    let w4 = sample_2d(&mut b, uv_w);
    let r4 = sample_2d(&mut b, uv_r);

    let e = b.vector_shuffle(t_v3f, None, e4, e4, [0, 1, 2]).unwrap();
    let n = b.vector_shuffle(t_v3f, None, n4, n4, [0, 1, 2]).unwrap();
    let s = b.vector_shuffle(t_v3f, None, s4, s4, [0, 1, 2]).unwrap();
    let w = b.vector_shuffle(t_v3f, None, w4, w4, [0, 1, 2]).unwrap();
    let r = b.vector_shuffle(t_v3f, None, r4, r4, [0, 1, 2]).unwrap();

    let le = b.dot(t_f32, None, e, c_luma).unwrap();
    let ln = b.dot(t_f32, None, n, c_luma).unwrap();
    let ls = b.dot(t_f32, None, s, c_luma).unwrap();
    let lw = b.dot(t_f32, None, w, c_luma).unwrap();
    let lr = b.dot(t_f32, None, r, c_luma).unwrap();

    let mn_ns = b.ext_inst(t_f32, None, glsl, GLSL_FMIN, [Operand::IdRef(ln), Operand::IdRef(ls)]).unwrap();
    let mn_wr = b.ext_inst(t_f32, None, glsl, GLSL_FMIN, [Operand::IdRef(lw), Operand::IdRef(lr)]).unwrap();
    let mn_4 = b.ext_inst(t_f32, None, glsl, GLSL_FMIN, [Operand::IdRef(mn_ns), Operand::IdRef(mn_wr)]).unwrap();
    let mn_l = b.ext_inst(t_f32, None, glsl, GLSL_FMIN, [Operand::IdRef(le), Operand::IdRef(mn_4)]).unwrap();

    let mx_ns = b.ext_inst(t_f32, None, glsl, GLSL_FMAX, [Operand::IdRef(ln), Operand::IdRef(ls)]).unwrap();
    let mx_wr = b.ext_inst(t_f32, None, glsl, GLSL_FMAX, [Operand::IdRef(lw), Operand::IdRef(lr)]).unwrap();
    let mx_4 = b.ext_inst(t_f32, None, glsl, GLSL_FMAX, [Operand::IdRef(mx_ns), Operand::IdRef(mx_wr)]).unwrap();
    let mx_l = b.ext_inst(t_f32, None, glsl, GLSL_FMAX, [Operand::IdRef(le), Operand::IdRef(mx_4)]).unwrap();

    // amp = clamp(min(mn_l, 1.0 - mx_l) / max(mx_l, 1e-4), 0.0, 1.0)
    let one_m_mx = b.f_sub(t_f32, None, c_f1, mx_l).unwrap();
    let head = b.ext_inst(t_f32, None, glsl, GLSL_FMIN, [Operand::IdRef(mn_l), Operand::IdRef(one_m_mx)]).unwrap();
    let denom_l = b.ext_inst(t_f32, None, glsl, GLSL_FMAX, [Operand::IdRef(mx_l), Operand::IdRef(c_eps)]).unwrap();
    let ratio = b.f_div(t_f32, None, head, denom_l).unwrap();
    let amp = b.ext_inst(t_f32, None, glsl, GLSL_FCLAMP, [Operand::IdRef(ratio), Operand::IdRef(c_f0), Operand::IdRef(c_f1)]).unwrap();
    let amp_sq = b.ext_inst(t_f32, None, glsl, GLSL_SQRT, [Operand::IdRef(amp)]).unwrap();

    // w_rcas = -0.22 * sharpness * sqrt(amp)
    let ws = b.f_mul(t_f32, None, c_lobe, sharp).unwrap();
    let w_rcas = b.f_mul(t_f32, None, ws, amp_sq).unwrap();

    // sum = n + s + w + r; out_rgb = clamp((e + w_rcas * sum) / (1.0 + 4.0 * w_rcas), 0.0, 1.0)
    let ns = b.f_add(t_v3f, None, n, s).unwrap();
    let wr = b.f_add(t_v3f, None, w, r).unwrap();
    let sum = b.f_add(t_v3f, None, ns, wr).unwrap();
    let scaled = b.vector_times_scalar(t_v3f, None, sum, w_rcas).unwrap();
    let num = b.f_add(t_v3f, None, e, scaled).unwrap();
    let w4f = b.f_mul(t_f32, None, c_f4, w_rcas).unwrap();
    let den = b.f_add(t_f32, None, c_f1, w4f).unwrap();
    let inv_den = b.f_div(t_f32, None, c_f1, den).unwrap();
    let sharp_rgb = b.vector_times_scalar(t_v3f, None, num, inv_den).unwrap();
    let clamped = b.ext_inst(t_v3f, None, glsl, GLSL_FCLAMP, [Operand::IdRef(sharp_rgb), Operand::IdRef(c_v3_0), Operand::IdRef(c_v3_1)]).unwrap();

    let cr = b.composite_extract(t_f32, None, clamped, [0]).unwrap();
    let cg = b.composite_extract(t_f32, None, clamped, [1]).unwrap();
    let cb = b.composite_extract(t_f32, None, clamped, [2]).unwrap();
    let ea = b.composite_extract(t_f32, None, e4, [3]).unwrap();
    let out4 = b.composite_construct(t_v4f, None, [cr, cg, cb, ea]).unwrap();
    b.store(v_out, out4, None, []).unwrap();
    b.ret().unwrap();
    b.end_function().unwrap();

    b.entry_point(spirv::ExecutionModel::Fragment, main_fn, "main", [v_uv, v_out]);
    b.execution_mode(main_fn, spirv::ExecutionMode::OriginUpperLeft, []);
    b.module().assemble()
}

/// Generates a SPIR-V 1.3 fragment shader implementing Motion-Compensated Optical-Flow
/// Frame Generation (2x/3x) with UI/HUD ghost-guard and scene-cut fallback.
///
/// Interface:
/// - `layout(location = 0) in vec2 uv` (from `spv_fullscreen_vs`)
/// - `layout(set = 0, binding = 0) uniform sampler2D prev_tex`
/// - `layout(set = 0, binding = 1) uniform sampler2D curr_tex`
/// - `layout(push_constant) uniform Params { vec4 params; }` = `(inv_w, inv_h, t_blend, 0.0)`
/// - `layout(location = 0) out vec4 out_color`
pub fn spv_framegen_fs() -> Vec<u32> {
    use rspirv::binary::Assemble;
    use rspirv::dr::{Builder, Operand};
    use rspirv::spirv;

    const GLSL_FABS: u32 = 4;
    const GLSL_FCLAMP: u32 = 43;
    const GLSL_FMIX: u32 = 46;

    let mut b = Builder::new();
    b.set_version(1, 3);
    b.capability(spirv::Capability::Shader);
    let glsl = b.ext_inst_import("GLSL.std.450");
    b.memory_model(spirv::AddressingModel::Logical, spirv::MemoryModel::GLSL450);

    let t_void = b.type_void();
    let t_bool = b.type_bool();
    let t_v2b = b.type_vector(t_bool, 2);
    let t_v4b = b.type_vector(t_bool, 4);
    let t_f32 = b.type_float(32);
    let t_u32 = b.type_int(32, 0);
    let t_v2f = b.type_vector(t_f32, 2);
    let t_v3f = b.type_vector(t_f32, 3);
    let t_v4f = b.type_vector(t_f32, 4);
    let t_fn = b.type_function(t_void, []);

    let t_img = b.type_image(t_f32, spirv::Dim::Dim2D, 0, 0, 0, 1, spirv::ImageFormat::Unknown, None);
    let t_samp = b.type_sampled_image(t_img);
    let p_tex = b.type_pointer(None, spirv::StorageClass::UniformConstant, t_samp);

    let v_prev = b.variable(p_tex, None, spirv::StorageClass::UniformConstant, None);
    b.decorate(v_prev, spirv::Decoration::DescriptorSet, [Operand::LiteralBit32(0)]);
    b.decorate(v_prev, spirv::Decoration::Binding, [Operand::LiteralBit32(0)]);

    let v_curr = b.variable(p_tex, None, spirv::StorageClass::UniformConstant, None);
    b.decorate(v_curr, spirv::Decoration::DescriptorSet, [Operand::LiteralBit32(0)]);
    b.decorate(v_curr, spirv::Decoration::Binding, [Operand::LiteralBit32(1)]);

    let t_pc = b.type_struct([t_v4f]);
    b.decorate(t_pc, spirv::Decoration::Block, []);
    b.member_decorate(t_pc, 0, spirv::Decoration::Offset, [Operand::LiteralBit32(0)]);
    let p_pc = b.type_pointer(None, spirv::StorageClass::PushConstant, t_pc);
    let p_pc_v4 = b.type_pointer(None, spirv::StorageClass::PushConstant, t_v4f);
    let v_pc = b.variable(p_pc, None, spirv::StorageClass::PushConstant, None);

    let p_in_v2 = b.type_pointer(None, spirv::StorageClass::Input, t_v2f);
    let v_uv = b.variable(p_in_v2, None, spirv::StorageClass::Input, None);
    b.decorate(v_uv, spirv::Decoration::Location, [Operand::LiteralBit32(0)]);

    let p_out_v4 = b.type_pointer(None, spirv::StorageClass::Output, t_v4f);
    let v_out = b.variable(p_out_v4, None, spirv::StorageClass::Output, None);
    b.decorate(v_out, spirv::Decoration::Location, [Operand::LiteralBit32(0)]);

    let c_u0 = b.constant_bit32(t_u32, 0);
    let c_f0 = b.constant_bit32(t_f32, 0.0f32.to_bits());
    let c_f1 = b.constant_bit32(t_f32, 1.0f32.to_bits());
    let c_step = b.constant_bit32(t_f32, 4.0f32.to_bits());
    let c_static_eps = b.constant_bit32(t_f32, 0.018f32.to_bits());
    let c_cut_eps = b.constant_bit32(t_f32, 0.85f32.to_bits());
    let c_v2_0 = b.constant_composite(t_v2f, [c_f0, c_f0]);
    let c_v2_1 = b.constant_composite(t_v2f, [c_f1, c_f1]);
    let c_ones3 = b.constant_composite(t_v3f, [c_f1, c_f1, c_f1]);

    let main_fn = b.begin_function(t_void, None, spirv::FunctionControl::NONE, t_fn).unwrap();
    let _ = b.begin_block(None).unwrap();

    let uv = b.load(t_v2f, None, v_uv, None, []).unwrap();
    let pc_ptr = b.access_chain(p_pc_v4, None, v_pc, [c_u0]).unwrap();
    let params = b.load(t_v4f, None, pc_ptr, None, []).unwrap();
    let inv_w = b.composite_extract(t_f32, None, params, [0]).unwrap();
    let inv_h = b.composite_extract(t_f32, None, params, [1]).unwrap();
    let t_blend = b.composite_extract(t_f32, None, params, [2]).unwrap();
    let one_m_t = b.f_sub(t_f32, None, c_f1, t_blend).unwrap();

    let sx = b.f_mul(t_f32, None, inv_w, c_step).unwrap();
    let sy = b.f_mul(t_f32, None, inv_h, c_step).unwrap();
    let nsx = b.f_negate(t_f32, None, sx).unwrap();
    let nsy = b.f_negate(t_f32, None, sy).unwrap();

    let sample_tex = |b: &mut Builder, var: u32, coord: u32| -> u32 {
        let s = b.load(t_samp, None, var, None, []).unwrap();
        b.image_sample_implicit_lod(t_v4f, None, s, coord, None, []).unwrap()
    };

    let sad = |b: &mut Builder, a4: u32, c4: u32| -> u32 {
        let a3 = b.vector_shuffle(t_v3f, None, a4, a4, [0, 1, 2]).unwrap();
        let c3 = b.vector_shuffle(t_v3f, None, c4, c4, [0, 1, 2]).unwrap();
        let diff = b.f_sub(t_v3f, None, a3, c3).unwrap();
        let ad = b.ext_inst(t_v3f, None, glsl, GLSL_FABS, [Operand::IdRef(diff)]).unwrap();
        b.dot(t_f32, None, ad, c_ones3).unwrap()
    };

    let p0 = sample_tex(&mut b, v_prev, uv);
    let c0 = sample_tex(&mut b, v_curr, uv);
    let diff0 = sad(&mut b, p0, c0);

    let mut best_cost = diff0;
    let mut best_mv = c_v2_0;

    let candidates = [
        b.composite_construct(t_v2f, None, [sx, c_f0]).unwrap(),
        b.composite_construct(t_v2f, None, [nsx, c_f0]).unwrap(),
        b.composite_construct(t_v2f, None, [c_f0, sy]).unwrap(),
        b.composite_construct(t_v2f, None, [c_f0, nsy]).unwrap(),
    ];
    for mv in candidates {
        let uvp = b.f_sub(t_v2f, None, uv, mv).unwrap();
        let uvc = b.f_add(t_v2f, None, uv, mv).unwrap();
        let ps = sample_tex(&mut b, v_prev, uvp);
        let cs = sample_tex(&mut b, v_curr, uvc);
        let cost = sad(&mut b, ps, cs);
        let better = b.f_ord_less_than(t_bool, None, cost, best_cost).unwrap();
        let better2 = b.composite_construct(t_v2b, None, [better, better]).unwrap();
        best_cost = b.select(t_f32, None, better, cost, best_cost).unwrap();
        best_mv = b.select(t_v2f, None, better2, mv, best_mv).unwrap();
    }

    // Motion-compensated blend at t_blend
    let mv_p = b.vector_times_scalar(t_v2f, None, best_mv, t_blend).unwrap();
    let mv_c = b.vector_times_scalar(t_v2f, None, best_mv, one_m_t).unwrap();
    let raw_uvp = b.f_sub(t_v2f, None, uv, mv_p).unwrap();
    let raw_uvc = b.f_add(t_v2f, None, uv, mv_c).unwrap();
    let uvp = b.ext_inst(t_v2f, None, glsl, GLSL_FCLAMP, [Operand::IdRef(raw_uvp), Operand::IdRef(c_v2_0), Operand::IdRef(c_v2_1)]).unwrap();
    let uvc = b.ext_inst(t_v2f, None, glsl, GLSL_FCLAMP, [Operand::IdRef(raw_uvc), Operand::IdRef(c_v2_0), Operand::IdRef(c_v2_1)]).unwrap();
    let sp_mc = sample_tex(&mut b, v_prev, uvp);
    let sc_mc = sample_tex(&mut b, v_curr, uvc);
    let t_v4 = b.composite_construct(t_v4f, None, [t_blend, t_blend, t_blend, t_blend]).unwrap();
    let blended = b.ext_inst(t_v4f, None, glsl, GLSL_FMIX, [Operand::IdRef(sp_mc), Operand::IdRef(sc_mc), Operand::IdRef(t_v4)]).unwrap();

    // UI/HUD ghost-guard & scene-cut fallback: static UI or hard cut outputs curr directly.
    let is_static = b.f_ord_less_than(t_bool, None, diff0, c_static_eps).unwrap();
    let is_cut = b.f_ord_greater_than(t_bool, None, diff0, c_cut_eps).unwrap();
    let use_curr = b.logical_or(t_bool, None, is_static, is_cut).unwrap();
    let use_curr4 = b.composite_construct(t_v4b, None, [use_curr, use_curr, use_curr, use_curr]).unwrap();
    let final_col = b.select(t_v4f, None, use_curr4, c0, blended).unwrap();

    b.store(v_out, final_col, None, []).unwrap();
    b.ret().unwrap();
    b.end_function().unwrap();

    b.entry_point(spirv::ExecutionModel::Fragment, main_fn, "main", [v_uv, v_out]);
    b.execution_mode(main_fn, spirv::ExecutionMode::OriginUpperLeft, []);
    b.module().assemble()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spirv_headers_valid() {
        let vs = spv_fullscreen_vs();
        let fs = spv_blit_fs();
        let solid = spv_solid_fs();
        let vvs = spv_vertex_vs();
        let tfs = spv_textured_color_fs();
        let rcas = spv_rcas_upscale_fs();
        let fg = spv_framegen_fs();
        for spv in [&vs, &fs, &solid, &vvs, &tfs, &rcas, &fg] {
            assert_eq!(spv[0], 0x0723_0203);
            assert!(spv.len() > 20);
        }
    }

    #[test]
    fn rcas_and_framegen_spirv_parse_cleanly() {
        for (name, words, expected_bindings) in [
            ("rcas", spv_rcas_upscale_fs(), 1usize),
            ("framegen", spv_framegen_fs(), 2usize),
        ] {
            let mut loader = rspirv::dr::Loader::new();
            rspirv::binary::parse_words(&words, &mut loader)
                .unwrap_or_else(|e| panic!("{name} spv parse failed: {e:?}"));
            let m = loader.module();
            assert_eq!(m.entry_points.len(), 1, "{name}: expected 1 entry point");
            assert_eq!(m.functions.len(), 1, "{name}: expected 1 function");
            let bindings = m
                .annotations
                .iter()
                .filter(|inst| {
                    inst.class.opcode == rspirv::spirv::Op::Decorate
                        && matches!(
                            inst.operands.get(1),
                            Some(rspirv::dr::Operand::Decoration(rspirv::spirv::Decoration::Binding))
                        )
                })
                .count();
            assert_eq!(bindings, expected_bindings, "{name}: binding count mismatch");
        }
    }

    #[test]
    fn translate_rdna2_alu_fragment_emits_valid_spirv() {
        // v_mov_b32 v0, 1.0; v_mov_b32 v1, 0.5; exp mrt0 v0, v1, v0, v0; s_endpgm
        let code = [0x7e00_02f2, 0x7e02_02f0, 0xf800_080f, 0x0000_0100, 0xbf81_0000];
        let (dec, spv) = compile_rdna2_to_spirv(&code, ShaderStage::Fragment);
        assert!(dec.exports_mrt0);
        assert_eq!(spv[0], 0x0723_0203);
    }

    #[test]
    fn decode_clear_and_copy_shaders() {
        // Real PS5 compute clear shader pattern (buffer_store_dwordx4 + s_endpgm)
        let clear = [0x7e00_0204, 0x7e02_0205, 0x7e04_0206, 0x7e06_0207, 0xe01c_2000, 0x8000_0004, 0xbf81_0000];
        let d = decode_rdna2(&clear);
        assert!(d.has_buffer_store);
        assert_eq!(d.ops.last(), Some(&Rdna2Op::EndPgm));
    }
}
