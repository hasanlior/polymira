//! Save data (libSceSaveData): each save directory is a folder on the PC, mounted for the
//! game as /savedata0, /savedata1, …
//!
//! Folder: %APPDATA%\POLYMIRA\orbis\savedata\<game folder>\<save directory name>

use std::ffi::c_char;
use std::path::PathBuf;
use std::sync::Mutex;

const ERROR_PARAMETER: i32 = 0x809F_0000u32 as i32;
const ERROR_MOUNT_BUSY: i32 = 0x809F_0003u32 as i32;
const ERROR_EXISTS: i32 = 0x809F_0007u32 as i32;
const ERROR_NOT_FOUND: i32 = 0x809F_0008u32 as i32;
const ERROR_MOUNT_FULL: i32 = 0x809F_000Cu32 as i32;

const MODE_CREATE: u32 = 1 << 2;
const MODE_CREATE2: u32 = 1 << 5;

/// (mount point, folder, directory name)
static MOUNTS: Mutex<Vec<(String, PathBuf, String)>> = Mutex::new(Vec::new());

fn text(p: *const c_char, max: usize) -> String {
    if p.is_null() {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, max) };
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(max);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect()
}

fn root() -> PathBuf {
    let data = crate::kernel::data_root();
    let game = crate::kernel::GAME_DIR.lock().map(|g| g.clone()).unwrap_or_default();
    let title = game.trim_end_matches('/').rsplit('/').next().unwrap_or("game").to_string();
    PathBuf::from(format!("{data}/savedata/{}", safe(&title)))
}

/// Host folder for a console path under a mounted save point (used by the file system).
pub fn resolve(path: &str) -> Option<PathBuf> {
    let mounts = MOUNTS.lock().ok()?;
    for (mp, dir, _) in mounts.iter() {
        if let Some(rest) = path.strip_prefix(mp.as_str()) {
            if rest.is_empty() || rest.starts_with('/') {
                return Some(dir.join(rest.trim_start_matches('/')));
            }
        }
    }
    None
}

pub extern "sysv64" fn ok() -> i32 {
    0
}

/// sceSaveDataMount3(mount, result): mount = { user, dirName*, blocks, systemBlocks, mode, …, resource }
/// result = { char mountPoint[16]; …; u32 mountStatus at 0x1C }
pub extern "sysv64" fn mount3(mount: *const u8, result: *mut u8) -> i32 {
    if mount.is_null() || result.is_null() {
        return ERROR_PARAMETER;
    }
    let (dir_ptr, mode) = unsafe { (*(mount.add(8) as *const usize), *(mount.add(0x20) as *const u32)) };
    let dir = text(dir_ptr as *const c_char, 32);
    mount_dir(&dir, mode, result)
}

fn mount_dir(dir: &str, mode: u32, result: *mut u8) -> i32 {
    if dir.is_empty() {
        return ERROR_PARAMETER;
    }
    let path = root().join(safe(dir));
    let existed = path.is_dir();
    if !existed && mode & (MODE_CREATE | MODE_CREATE2) == 0 {
        eprintln!("[orbis] save data {dir}: none yet");
        return ERROR_NOT_FOUND;
    }
    if existed && mode & MODE_CREATE != 0 && mode & MODE_CREATE2 == 0 {
        return ERROR_EXISTS;
    }
    if std::fs::create_dir_all(&path).is_err() {
        return ERROR_PARAMETER;
    }
    let mut mounts = MOUNTS.lock().unwrap();
    if mounts.iter().any(|(_, _, d)| d == dir) {
        return ERROR_MOUNT_BUSY;
    }
    let Some(slot) = (0..16).find(|i| !mounts.iter().any(|(mp, _, _)| *mp == format!("/savedata{i}"))) else { return ERROR_MOUNT_FULL };
    let mp = format!("/savedata{slot}");
    mounts.push((mp.clone(), path.clone(), dir.to_string()));
    unsafe {
        std::ptr::write_bytes(result, 0, 0x40);
        std::ptr::copy_nonoverlapping(mp.as_ptr(), result, mp.len());
        *(result.add(0x1C) as *mut u32) = if existed { 0 } else { 1 };
    }
    eprintln!("[orbis] save data {dir} mounted at {mp} ({})", path.display());
    0
}

/// Finds the mount-point string among the arguments (PS4 and PS5 variants differ).
fn mount_point_arg(args: &[usize]) -> Option<String> {
    for &a in args {
        if a > 0x10000 {
            let s = text(a as *const c_char, 16);
            if s.starts_with("/savedata") {
                return Some(s);
            }
        }
    }
    None
}

/// sceSaveDataUmount(mountPoint*) / sceSaveDataUmount2(mode, mountPoint*)
pub extern "sysv64" fn umount(a: usize, b: usize) -> i32 {
    if let Some(mp) = mount_point_arg(&[a, b]) {
        MOUNTS.lock().unwrap().retain(|(m, _, _)| *m != mp);
    }
    0
}

/// sceSaveDataGetMountInfo(mountPoint*, info*): { blocks, freeBlocks, … }
/// 1 block = 32 KiB; report 2 GiB (65536 blocks = SCE_SAVE_DATA_BLOCKS_MAX2) with ~1.87 GiB free.
pub extern "sysv64" fn mount_info(_mp: usize, info: *mut u64) -> i32 {
    if !info.is_null() {
        unsafe {
            std::ptr::write_bytes(info as *mut u8, 0, 0x30);
            *info = 65536;     // blocks (2 GiB)
            *info.add(1) = 60000; // freeBlocks (~1.87 GiB)
        }
    }
    0
}

/// sceSaveDataDirNameSearch(cond, result)
/// cond = { user, titleId*, dirName* (pattern, % = any), sortKey, sortOrder }
/// result = { u32 hitNum; dirNames* @8; u32 dirNamesNum @0x10; u32 setNum @0x14; params* @0x18; infos* @0x20 }
pub extern "sysv64" fn dir_name_search(cond: *const u8, result: *mut u8) -> i32 {
    if cond.is_null() || result.is_null() {
        return ERROR_PARAMETER;
    }
    let pattern = text(unsafe { *(cond.add(0x10) as *const usize) } as *const c_char, 32);
    let mut names: Vec<String> = std::fs::read_dir(root())
        .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.retain(|n| matches(&pattern, n));
    names.sort();
    unsafe {
        let dir_names = *(result.add(8) as *const usize) as *mut u8;
        let cap = *(result.add(0x10) as *const u32) as usize;
        let params = *(result.add(0x18) as *const usize) as *mut u8;
        let infos = *(result.add(0x20) as *const usize) as *mut u8;
        let set = names.len().min(cap);
        *(result as *mut u32) = names.len() as u32;
        *(result.add(0x14) as *mut u32) = set as u32;
        for (i, n) in names.iter().take(set).enumerate() {
            if !dir_names.is_null() {
                let d = dir_names.add(i * 32);
                std::ptr::write_bytes(d, 0, 32);
                std::ptr::copy_nonoverlapping(n.as_ptr(), d, n.len().min(31));
            }
            if !params.is_null() {
                std::ptr::write_bytes(params.add(i * 0x530), 0, 0x530);
            }
            if !infos.is_null() {
                std::ptr::write_bytes(infos.add(i * 0x30), 0, 0x30);
                // SceSaveDataSearchInfo: { u64 blocks @0x00, u64 freeBlocks @0x08 }
                // Report realistic per-save size (128 blocks = 4 MiB) + ample free space (32768 blocks = 1 GiB)
                // so games summing used save blocks do not overflow their save quota.
                *(infos.add(i * 0x30) as *mut u64) = 128;
                *(infos.add(i * 0x30 + 8) as *mut u64) = 32768;
            }
        }
    }
    0
}

fn matches(pattern: &str, name: &str) -> bool {
    if pattern.is_empty() || pattern == "%" {
        return true;
    }
    match pattern.split_once('%') {
        None => pattern == name,
        Some((pre, post)) => name.starts_with(pre) && name.ends_with(post) && name.len() >= pre.len() + post.len(),
    }
}

/// sceSaveDataDelete(del): { user, titleId* @8, dirName* @0x10, … }
pub extern "sysv64" fn delete(del: *const u8) -> i32 {
    if del.is_null() {
        return ERROR_PARAMETER;
    }
    let dir = text(unsafe { *(del.add(0x10) as *const usize) } as *const c_char, 32);
    if dir.is_empty() {
        return ERROR_PARAMETER;
    }
    let path = root().join(safe(&dir));
    if !path.is_dir() {
        return ERROR_NOT_FOUND;
    }
    let _ = std::fs::remove_dir_all(path);
    0
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("sceSaveDataInitialize3", ok as *const () as usize),
        ("sceSaveDataMount", mount3 as *const () as usize),
        ("sceSaveDataMount2", mount3 as *const () as usize),
        ("#ZP4e7rlzOUk", mount3 as *const () as usize), // sceSaveDataMount3
        ("sceSaveDataUmount", umount as *const () as usize),
        ("#uW4vfTwMQVo", umount as *const () as usize), // sceSaveDataUmount2
        ("sceSaveDataGetMountInfo", mount_info as *const () as usize),
        ("#65VH0Qaaz6s", mount_info as *const () as usize), // sceSaveDataGetMountInfo
        ("sceSaveDataDirNameSearch", dir_name_search as *const () as usize),
        ("#X4MYzukPc3g", dir_name_search as *const () as usize), // sceSaveDataDirNameSearchPs4
        ("sceSaveDataDelete", delete as *const () as usize),
        ("#sDCBrmc61XU", ok as *const () as usize), // sceSaveDataPrepare
        ("#ie7qhZ4X0Cc", ok as *const () as usize), // sceSaveDataCommit
        ("#gjRZNnw0JPE", ok as *const () as usize), // sceSaveDataCreateTransactionResource
        ("#lJUQuaKqoKY", ok as *const () as usize), // sceSaveDataDeleteTransactionResource
        ("#85zul--eGXs", ok as *const () as usize), // sceSaveDataSetParam
        ("sceSaveDataSaveIcon", ok as *const () as usize),
    ]
}
