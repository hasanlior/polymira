//! PS5 (AMD GFX10 / RDNA2) texture untiling for 32-bpp surfaces.
//!
//! Converts linear and GFX10 64KB standard swizzle (`SW_64KB_S` / `SW_64KB_D` / `SW_64KB_R`)
//! textures into linear RGBA8/BGRA8 rows suitable for Vulkan `vkCmdCopyBufferToImage`.

/// Untiles a 32-bpp (4 bytes/pixel) GFX10 surface into linear `width * height` u32 pixels.
///
/// `swizzle_mode`:
/// - `0` => linear
/// - `1..=31` => GFX10 micro-tile (8×8 pixels = 256 B) + 64KB macro-tile (128×128 pixels)
pub fn untile_32bpp(src: &[u8], width: u32, height: u32, swizzle_mode: u32) -> Vec<u32> {
    let w = width as usize;
    let h = height as usize;
    let mut dst = vec![0u32; w * h];
    if w == 0 || h == 0 || src.len() < 4 {
        return dst;
    }

    if swizzle_mode == 0 {
        let count = (src.len() / 4).min(w * h);
        for i in 0..count {
            let b = i * 4;
            dst[i] = u32::from_le_bytes([src[b], src[b + 1], src[b + 2], src[b + 3]]);
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
                dst[y * w + x] = u32::from_le_bytes([
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
}
