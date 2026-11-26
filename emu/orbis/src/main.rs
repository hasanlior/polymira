//! POLYMIRA Orbis — experimental PS4 executable analyzer and loader.
//!
//!   orbis analyze <eboot.bin> [--json]   what the executable needs from the system
//!   orbis run <eboot.bin>                load and start it (early: system calls are mostly stubs)
//!   orbis selftest                       build and run a PS4-format test program
//!   orbis sample <path>                  write that test program as a SELF file (eboot.bin)
//!
//! Orbis runs only files you provide. It contains no Sony code, keys or firmware.

mod format;
mod heal;
mod hle;
mod hle_rt;
mod known;
mod kernel;
mod rdna;
mod equeue;
mod gpu;
mod agc;
mod audio;
mod shader;
mod tiling;
mod vk;
mod video;
mod savedata;
mod system;
mod thread;
mod loader;
mod nid;
mod pkg;
mod selftest;
mod tls;

use std::collections::BTreeMap;
use std::process::ExitCode;

use format::{Executable, PT_SCE_PROCPARAM, PT_TLS};

#[derive(serde::Serialize)]
struct LibraryReport {
    name: String,
    functions: usize,
    implemented: usize,
}

#[derive(serde::Serialize)]
struct Report {
    file: String,
    container: &'static str,
    kind: &'static str,
    entry: String,
    segments: usize,
    uses_tls: bool,
    has_procparam: bool,
    modules: Vec<String>,
    libraries: Vec<LibraryReport>,
    imports: usize,
    implemented: usize,
    /// Of those, functions the game's own modules (sce_module/*.prx) provide.
    from_game_modules: usize,
    /// Share of imported system functions Orbis implements (0-100).
    readiness: u32,
    missing_examples: Vec<String>,
}

fn analyze(path: &str) -> Result<Report, String> {
    // A package: say what it is and why its files cannot be read.
    if let Ok(mut f) = std::fs::File::open(path) {
        use std::io::Read;
        let mut head = [0u8; 4];
        if f.read_exact(&mut head).is_ok() && pkg::is_pkg(&head) {
            let info = pkg::read(path)?;
            return Err(format!(
                "pkg:{}:{}",
                info.content_id,
                if info.pfs_encrypted { "encrypted" } else { "plain" }
            ));
        }
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let exe = Executable::parse(&bytes).map_err(|e| e.to_string())?;
    let imports = exe.imports();
    let table = hle::table();
    let names = known::names();
    // Functions exported by the modules the game ships (libc.prx, middleware): the loader links
    // these, so they are available even though Orbis does not implement them itself.
    let mut game_exports: std::collections::HashSet<String> = Default::default();
    for m in loader::game_modules(std::path::Path::new(path)) {
        if let Ok(b) = std::fs::read(&m) {
            if let Ok(e) = Executable::parse(&b) {
                game_exports.extend(e.exports().into_iter().map(|(nid, _)| nid));
            }
        }
    }
    let ready = |nid: &String| table.contains_key(nid) || game_exports.contains(nid);
    let mut libs: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut missing = vec![];
    for i in &imports {
        let e = libs.entry(i.library.clone()).or_default();
        e.0 += 1;
        if ready(&i.nid) {
            e.1 += 1;
        } else if missing.len() < 12 {
            missing.push(names.get(&i.nid).map(|s| s.to_string()).unwrap_or_else(|| i.nid.clone()));
        }
    }
    let implemented = imports.iter().filter(|i| ready(&i.nid)).count();
    let from_game_modules = imports.iter().filter(|i| !table.contains_key(&i.nid) && game_exports.contains(&i.nid)).count();
    Ok(Report {
        file: path.to_string(),
        container: exe.container,
        kind: match exe.elf_type {
            0xFE10 => "executable",
            0xFE18 => "dynamic executable",
            0xFE0C => "system module",
            _ => "other",
        },
        entry: format!("{:#x}", exe.entry),
        segments: exe.phdrs.len(),
        uses_tls: exe.segment(PT_TLS).is_some(),
        has_procparam: exe.segment(PT_SCE_PROCPARAM).is_some(),
        modules: exe.modules.iter().map(|(_, n)| n.clone()).collect(),
        libraries: libs.into_iter().map(|(name, (functions, implemented))| LibraryReport { name, functions, implemented }).collect(),
        imports: imports.len(),
        implemented,
        from_game_modules,
        readiness: if imports.is_empty() { 0 } else { (implemented * 100 / imports.len()) as u32 },
        missing_examples: missing,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("analyze") if args.len() >= 2 => analyze(&args[1]).map(|r| {
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string(&r).unwrap());
            } else {
                println!("{} ({} {}), entry {}", r.file, r.container, r.kind, r.entry);
                println!("modules: {}", r.modules.join(", "));
                for l in &r.libraries {
                    println!("  {:<28} {:>4} functions, {:>4} implemented", l.name, l.functions, l.implemented);
                }
                println!("readiness: {}% ({}/{})", r.readiness, r.implemented, r.imports);
            }
        }),
        Some("run") if args.len() >= 2 => (|| {
            // The game folder (save data name, /app0, modules) comes from this path: make it absolute.
            let mut args = args.clone();
            if let Ok(abs) = std::path::absolute(&args[1]) {
                args[1] = abs.to_string_lossy().into_owned();
            }
            let bytes = std::fs::read(&args[1]).map_err(|e| e.to_string())?;
            let exe = Executable::parse(&bytes).map_err(|e| e.to_string())?;
            let mut kinds = std::collections::BTreeMap::new();
            for (_, kind, _, _) in exe.relocations() {
                *kinds.entry(kind).or_insert(0usize) += 1;
            }
            eprintln!("[orbis] {} ({}), {} relocations by type: {:?}", args[1], exe.container, kinds.values().sum::<usize>(), kinds);
            let path = std::path::Path::new(&args[1]);
            let game_dir = path.parent().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            *kernel::GAME_DIR.lock().unwrap() = game_dir.clone();
            video::show_os_splash(&game_dir);
            loader::install_crash_report();
            let loaded = loader::load_with_modules(exe, Some(path))?;
            eprintln!("[orbis] loaded at {:p} with {} game modules; {} imports bound, {} stubbed; starting at entry {:#x}", loaded.base, loaded.modules.len(), loaded.bound, loaded.stubbed, loaded.entry - loaded.base as u64);
            let code = loader::run(&loaded);
            eprintln!("[orbis] exited with {code}");
            Ok(())
        })(),
        Some("selftest") => selftest::run(),
        // rdna-dis <shader.bin>: one line per instruction (offset, dwords, name) for checking the decoder.
        Some("rdna-dis") if args.len() >= 2 => (|| {
            let bytes = std::fs::read(&args[1]).map_err(|e| e.to_string())?;
            let code: Vec<u32> = bytes.chunks(4).map(|c| { let mut b = [0u8; 4]; b[..c.len()].copy_from_slice(c); u32::from_le_bytes(b) }).collect();
            for i in rdna::decode_shader(&code) {
                println!("{:06x} {} {}", i.pc, i.words.len(), i.name);
            }
            Ok(())
        })(),
        // ps-spv <dir>: translates every dumped pixel shader (*_k1.bin) and validates the SPIR-V.
        Some("ps-spv") if args.len() >= 2 => (|| {
            let mut ok = 0;
            let mut fails: std::collections::BTreeMap<String, usize> = Default::default();
            let mut names: Vec<_> = std::fs::read_dir(&args[1]).map_err(|e| e.to_string())?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with("_k1.bin")).collect();
            names.sort();
            for p in names {
                let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
                let code: Vec<u32> = bytes.chunks(4).map(|c| { let mut b = [0u8; 4]; b[..c.len()].copy_from_slice(c); u32::from_le_bytes(b) }).collect();
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                match rdna::ps::translate(&code, 0x302) {
                    Err(e) => { println!("{name}: unsupported {e}"); *fails.entry(e).or_default() += 1; }
                    Ok(t) => {
                        let opts = naga::front::spv::Options::default();
                        match naga::front::spv::parse_u8_slice(bytemuck_words(&t.spirv), &opts) {
                            Err(e) => { println!("{name}: parse {e:?}"); if std::env::var("DIS").is_ok() { use rspirv::binary::Disassemble; println!("{}", rspirv::dr::load_words(&t.spirv).unwrap().disassemble()); } *fails.entry("parse".into()).or_default() += 1; }
                            Ok(m) => {
                                let mut v = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
                                match v.validate(&m) {
                                    Err(e) => { println!("{name}: invalid {:?}", e.into_inner()); *fails.entry("invalid".into()).or_default() += 1; }
                                    Ok(_) => { ok += 1; println!("{name}: ok slots={} images={}", t.slots, t.images); }
                                }
                            }
                        }
                        if let Some(out) = args.get(2) { let _ = std::fs::write(format!("{out}/{name}.spv"), bytemuck_words(&t.spirv)); }
                    }
                }
            }
            println!("ok {ok}; {fails:?}");
            Ok(())
        })(),
        Some("sample") if args.len() >= 2 => std::fs::write(&args[1], selftest::wrap_self(&selftest::build_elf())).map_err(|e| e.to_string()),
        _ => Err("usage: orbis analyze <eboot.bin> [--json] | orbis run <eboot.bin> | orbis selftest".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::json!({ "error": e }));
            } else {
                eprintln!("orbis: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn bytemuck_words(w: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 4) }
}
