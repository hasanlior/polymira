//! PS5 (AMD GFX10 / RDNA2) texture untiling for 8-bpp, 16-bpp, and 32-bpp surfaces.
//!
//! Converts linear and GFX10 64KB standard swizzle (`SW_64KB_S` / `SW_64KB_D` / `SW_64KB_R`)
//! textures into linear RGBA8 rows suitable for Vulkan `vkCmdCopyBufferToImage`.

#[inline]
fn map_dst_sel(ch: [u8; 4], dst_sel: u32) -> u32 {
    if dst_sel == 0 {
        return u32::from_le_bytes(ch);
    }
    let pick = |s: u32| -> u8 {
        match s & 7 {
            0 => 0,
            1 => 255,
            4 => ch[0],
            5 => ch[1],
            6 => ch[2],
            7 => ch[3],
            _ => 0,
        }
    };
    u32::from_le_bytes([
        pick(dst_sel),
        pick(dst_sel >> 3),
        pick(dst_sel >> 6),
        pick(dst_sel >> 9),
    ])
}

/// Untiles a GFX10 texture surface into linear `width * height` RGBA8 `u32` pixels.
///
/// `swiz_pack`:
/// - bits `0..5`: `swizzle_mode` (`0` = linear, `1..=31` = GFX10 64KB tiled)
/// - bits `8..17`: T# unified `format` (`1 | 2 | 9` = 8-bpp R8, `16 | 17 | 25` = 16-bpp RG8, default = 32-bpp RGBA8)
/// - bits `20..32`: T# `DST_SEL_XYZW` (12 bits from `T#[3] & 0xFFF`)
pub fn untile_32bpp(src: &[u8], width: u32, height: u32, swiz_pack: u32) -> Vec<u32> {
    let swizzle_mode = swiz_pack & 0x1F;
    let fmt = (swiz_pack >> 8) & 0x1FF;
    let dst_sel = (swiz_pack >> 20) & 0xFFF;
    match fmt {
        1..=14 => untile_8bpp(src, width, height, swizzle_mode, dst_sel),
        15..=31 => untile_16bpp(src, width, height, swizzle_mode, dst_sel),
        _ => untile_32bpp_raw(src, width, height, swizzle_mode, dst_sel),
    }
}

/// Untiles an 8-bpp (1 byte/pixel, e.g. `R8_UNORM` font atlas) GFX10 surface into linear RGBA8 `u32` pixels.
pub fn untile_8bpp(src: &[u8], width: u32, height: u32, swizzle_mode: u32, _dst_sel: u32) -> Vec<u32> {
    let w = width as usize;
    let h = height as usize;
    let mut dst = vec![0u32; w * h];
    if w == 0 || h == 0 || src.is_empty() {
        return dst;
    }
    // Replicate the single 8-bit sample into (R, G, B, A) so both `.r`-sampling and `.a`-sampling
    // font/mask shaders work regardless of T# DST_SEL_XYZW.
    let expand = |v: u8| -> u32 {
        u32::from_le_bytes([v, v, v, v])
    };
    if swizzle_mode == 0 {
        let count = src.len().min(w * h);
        for i in 0..count {
            dst[i] = expand(src[i]);
        }
        return dst;
    }
    // GFX10 64KB standard swizzle (SW_64KB_S, mode 9) for 8-bpp (1 byte/pixel):
    // Micro-tile: 16x16 pixels (256 bytes):
    //   xBytesMask = 0xAAAF -> x0=0, x1=1, x2=2, x3=3, x4=9, x5=11, x6=13, x7=15
    //   yMask      = 0x5550 -> y0=4, y1=5, y2=6, y3=7, y4=8, y5=10, y6=12, y7=14
    let tiles_x = (w + 255) / 256;
    for y in 0..h {
        let ty = y >> 8;
        let ly = y & 255;
        for x in 0..w {
            let tx = x >> 8;
            let lx = x & 255;
            let in_tile = (lx & 15)
                | ((ly & 15) << 4)
                | (((ly >> 4) & 1) << 8)
                | (((lx >> 4) & 1) << 9)
                | (((ly >> 5) & 1) << 10)
                | (((lx >> 5) & 1) << 11)
                | (((ly >> 6) & 1) << 12)
                | (((lx >> 6) & 1) << 13)
                | (((ly >> 7) & 1) << 14)
                | (((lx >> 7) & 1) << 15);
            let tile_idx = ty * tiles_x + tx;
            let byte_off = (tile_idx * 65536) + in_tile;
            if byte_off < src.len() {
                dst[y * w + x] = expand(src[byte_off]);
            }
        }
    }
    dst
}

/// Untiles a 16-bpp (2 bytes/pixel, e.g. `R8G8_UNORM`) GFX10 surface into linear RGBA8 `u32` pixels.
pub fn untile_16bpp(src: &[u8], width: u32, height: u32, swizzle_mode: u32, dst_sel: u32) -> Vec<u32> {
    let w = width as usize;
    let h = height as usize;
    let mut dst = vec![0u32; w * h];
    if w == 0 || h == 0 || src.len() < 2 {
        return dst;
    }
    let expand = |r: u8, g: u8| -> u32 {
        if dst_sel == 0 {
            u32::from_le_bytes([r, g, 0, 255])
        } else {
            map_dst_sel([r, g, 0, 255], dst_sel)
        }
    };
    if swizzle_mode == 0 {
        let count = (src.len() / 2).min(w * h);
        for i in 0..count {
            dst[i] = expand(src[i * 2], src[i * 2 + 1]);
        }
        return dst;
    }
    // GFX10 64KB standard swizzle (SW_64KB_S, mode 9) for 16-bpp (2 bytes/pixel):
    // Macro-tile: 256x128 pixels (65536 bytes), micro-tile: 16x8 pixels (256 bytes).
    let tiles_x = (w + 255) / 256;
    for y in 0..h {
        let ty = y >> 7;
        let ly = y & 127;
        for x in 0..w {
            let tx = x >> 8;
            let lx = x & 255;
            let in_tile = (lx & 7)
                | ((ly & 3) << 3)
                | (((lx >> 3) & 1) << 5)
                | (((ly >> 2) & 1) << 6)
                | (((ly >> 3) & 1) << 7)
                | (((lx >> 4) & 1) << 8)
                | (((ly >> 4) & 1) << 9)
                | (((lx >> 5) & 1) << 10)
                | (((ly >> 5) & 1) << 11)
                | (((lx >> 6) & 1) << 12)
                | (((ly >> 6) & 1) << 13)
                | (((lx >> 7) & 1) << 14);
            let tile_idx = ty * tiles_x + tx;
            let byte_off = (tile_idx * 65536) + (in_tile * 2);
            if byte_off + 2 <= src.len() {
                dst[y * w + x] = expand(src[byte_off], src[byte_off + 1]);
            }
        }
    }
    dst
}

fn untile_32bpp_raw(src: &[u8], width: u32, height: u32, swizzle_mode: u32, dst_sel: u32) -> Vec<u32> {
    let w = width as usize;
    let h = height as usize;
    let mut dst = vec![0u32; w * h];
    if w == 0 || h == 0 || src.len() < 4 {
        return dst;
    }
    let map_px = |b: [u8; 4]| -> u32 {
        if dst_sel == 0 || dst_sel == 0xFAC {
            u32::from_le_bytes(b)
        } else {
            map_dst_sel(b, dst_sel)
        }
    };

    if swizzle_mode == 0 {
        let count = (src.len() / 4).min(w * h);
        for i in 0..count {
            let b = i * 4;
            dst[i] = map_px([src[b], src[b + 1], src[b + 2], src[b + 3]]);
        }
        return dst;
    }

    // GFX10.3 / RDNA2 SW_64KB_R_X (mode 27, 32-bpp, 128x128 = 64 KiB, PS5 Oberon 16-pipe):
    //   256B micro-tile (8x8): x0, x1, y0, y1, x2, y2
    //   64KB macro-tile with 16-pipe _X XOR: bits [6..9] = (x3, y3, x4, y4) ^ (y6, x6, y5, x5)
    //   upper bits [10..13] = (x5, y5, x6, y6).
    if swizzle_mode == 27 || swizzle_mode == 25 || swizzle_mode == 26 {
        let tiles_x = (w + 127) / 128;
        let is_rx = swizzle_mode == 27;
        for y in 0..h {
            let ty = y >> 7;
            let ly = y & 127;
            for x in 0..w {
                let tx = x >> 7;
                let lx = x & 127;
                let x5 = (lx >> 5) & 1;
                let y5 = (ly >> 5) & 1;
                let x6 = (lx >> 6) & 1;
                let y6 = (ly >> 6) & 1;
                let mut b6 = (lx >> 3) & 1;
                let mut b7 = (ly >> 3) & 1;
                let mut b8 = (lx >> 4) & 1;
                let mut b9 = (ly >> 4) & 1;
                if is_rx {
                    b6 ^= y6;
                    b7 ^= x6;
                    b8 ^= y5;
                    b9 ^= x5;
                }
                let in_tile = (lx & 3)
                    | ((ly & 3) << 2)
                    | (((lx >> 2) & 1) << 4)
                    | (((ly >> 2) & 1) << 5)
                    | (b6 << 6)
                    | (b7 << 7)
                    | (b8 << 8)
                    | (b9 << 9)
                    | (x5 << 10)
                    | (y5 << 11)
                    | (x6 << 12)
                    | (y6 << 13);
                let tile_idx = ty * tiles_x + tx;
                let byte_off = (tile_idx * 65536) + (in_tile * 4);
                if byte_off + 4 <= src.len() {
                    dst[y * w + x] = map_px([
                        src[byte_off],
                        src[byte_off + 1],
                        src[byte_off + 2],
                        src[byte_off + 3],
                    ]);
                }
            }
        }
        return dst;
    }

    // GFX10 64KB standard swizzle (SW_64KB_S, mode 9) for 32-bpp:
    // Macro-tile: 128x128 pixels (65536 bytes).
    // Byte offset masks (D3D12 / GFX10 SW_64KB_S 32bpp):
    //   xBytesMask = 0xAA8F -> pixel bits: x0=0, x1=1, x2=5, x3=7, x4=9, x5=11, x6=13
    //   yMask      = 0x5570 -> pixel bits: y0=2, y1=3, y2=4, y3=6, y4=8, y5=10, y6=12
    let tiles_x = (w + 127) / 128;
    for y in 0..h {
        let ty = y >> 7;
        let ly = y & 127;
        for x in 0..w {
            let tx = x >> 7;
            let lx = x & 127;
            let in_tile = (lx & 3)
                | ((ly & 7) << 2)
                | (((lx >> 2) & 1) << 5)
                | (((ly >> 3) & 1) << 6)
                | (((lx >> 3) & 1) << 7)
                | (((ly >> 4) & 1) << 8)
                | (((lx >> 4) & 1) << 9)
                | (((ly >> 5) & 1) << 10)
                | (((lx >> 5) & 1) << 11)
                | (((ly >> 6) & 1) << 12)
                | (((lx >> 6) & 1) << 13);
            let tile_idx = ty * tiles_x + tx;
            let byte_off = (tile_idx * 65536) + (in_tile * 4);
            if byte_off + 4 <= src.len() {
                dst[y * w + x] = map_px([
                    src[byte_off],
                    src[byte_off + 1],
                    src[byte_off + 2],
                    src[byte_off + 3],
                ]);
            }
        }
    }
    dst
}

/// Byte offset of element (x, y) in array slice `slice` of a `width` x `height` surface with
/// `bpe` bytes per element: linear (mode 0) or GFX10 64KB standard swizzle, using the same
/// bit layouts as the untilers above. None for layouts it does not know.
pub fn element_offset(x: usize, y: usize, slice: usize, width: usize, height: usize, bpe: usize, swizzle_mode: u32) -> Option<usize> {
    if swizzle_mode == 0 {
        return Some(((slice * height + y) * width + x) * bpe);
    }
    // (x bits, y bits) in element-index order inside one 64 KiB block, and block size.
    let (xb, yb, bw, bh): (&[u32], &[u32], usize, usize) = match bpe {
        1 => (&[0, 1, 2, 3, 9, 11, 13, 15], &[4, 5, 6, 7, 8, 10, 12, 14], 256, 256),
        2 => (&[0, 1, 2, 5, 8, 10, 12, 14], &[3, 4, 6, 7, 9, 11, 13], 256, 128),
        4 => (&[0, 1, 5, 7, 9, 11, 13], &[2, 3, 4, 6, 8, 10, 12], 128, 128),
        _ => return None,
    };
    let (lx, ly) = (x % bw, y % bh);
    let mut e = 0usize;
    for (k, &b) in xb.iter().enumerate() {
        e |= ((lx >> k) & 1) << b;
    }
    for (k, &b) in yb.iter().enumerate() {
        e |= ((ly >> k) & 1) << b;
    }
    let tiles_x = (width + bw - 1) / bw;
    let tiles_y = (height + bh - 1) / bh;
    let block = slice * tiles_x * tiles_y + (y / bh) * tiles_x + x / bw;
    Some(block * 65536 + e * bpe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_untile_roundtrip() {
        let pixels: Vec<u32> = (0..16).map(|i| 0xFF00_0000 | i).collect();
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
        assert_eq!(untile_32bpp(&bytes, 4, 4, 0), pixels);
    }

    #[test]
    fn swizzle_64kb_preserves_pixel_count() {
        let mut src = vec![0u8; 65536];
        src[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let out = untile_32bpp(&src, 128, 128, 9);
        assert_eq!(out[0], 0xDEAD_BEEF);
        assert_eq!(out.len(), 128 * 128);
    }

    #[test]
    fn swizzle_64kb_rx_is_bijection() {
        let mut src = vec![0u8; 65536];
        for i in 0..16384u32 {
            let b = (i as usize) * 4;
            src[b..b + 4].copy_from_slice(&(i + 1).to_le_bytes());
        }
        let out = untile_32bpp(&src, 128, 128, 27);
        assert_eq!(out.len(), 16384);
        let mut seen = vec![false; 16384];
        for &v in &out {
            assert!(v >= 1 && v <= 16384);
            assert!(!seen[(v - 1) as usize]);
            seen[(v - 1) as usize] = true;
        }
    }
}
