//! PS4 package (.pkg) headers: enough to identify a package and tell whether its file
//! system can be read. Game files live in a PFS image that is encrypted in retail packages
//! and in packages made with the usual packaging tools; Orbis does not contain the keys
//! needed to decrypt it, so it asks for the game folder instead.

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}
fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
}

pub struct PkgInfo {
    pub content_id: String,
    pub content_type: u32,
    pub pfs_encrypted: bool,
}

pub fn is_pkg(head: &[u8]) -> bool {
    head.len() >= 4 && head[..4] == [0x7F, b'C', b'N', b'T']
}

pub fn read(path: &str) -> Result<PkgInfo, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = vec![0u8; 0x1000];
    f.read_exact(&mut h).map_err(|e| e.to_string())?;
    if !is_pkg(&h) {
        return Err("not a PS4 package".into());
    }
    let content_id = String::from_utf8_lossy(&h[0x40..0x64]).trim_end_matches('\0').to_string();
    let content_type = be32(&h, 0x74);
    let pfs_offset = be64(&h, 0x410);
    let mut pfs = [0u8; 0x20];
    f.seek(SeekFrom::Start(pfs_offset)).map_err(|e| e.to_string())?;
    f.read_exact(&mut pfs).map_err(|e| e.to_string())?;
    let mode = u16::from_le_bytes([pfs[0x1C], pfs[0x1D]]);
    Ok(PkgInfo { content_id, content_type, pfs_encrypted: mode & 0x4 != 0 })
}
