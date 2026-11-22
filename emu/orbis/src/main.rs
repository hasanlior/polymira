//! POLYMIRA Orbis — experimental PS4 executable analyzer and loader.
//!
//!   orbis analyze <eboot.bin> [--json]   what the executable needs from the system
//!   orbis run <eboot.bin>                load and start it (early: system calls are mostly stubs)
//!   orbis selftest                       build and run a PS4-format test program
//!   orbis sample <path>                  write that test program as a SELF file (eboot.bin)
//!
//! Orbis runs only files you provide. It contains no Sony code, keys or firmware.

mod format;
mod hle;
mod known;
mod loader;
mod nid;
mod selftest;

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
    /// Share of imported system functions Orbis implements (0-100).
    readiness: u32,
    missing_examples: Vec<String>,
}

fn analyze(path: &str) -> Result<Report, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let exe = Executable::parse(&bytes).map_err(|e| e.to_string())?;
    let imports = exe.imports();
    let table = hle::table();
    let names = known::names();
    let mut libs: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut missing = vec![];
    for i in &imports {
        let e = libs.entry(i.library.clone()).or_default();
        e.0 += 1;
        if table.contains_key(&i.nid) {
            e.1 += 1;
        } else if missing.len() < 12 {
            missing.push(names.get(&i.nid).map(|s| s.to_string()).unwrap_or_else(|| i.nid.clone()));
        }
    }
    let implemented = imports.iter().filter(|i| table.contains_key(&i.nid)).count();
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
            let bytes = std::fs::read(&args[1]).map_err(|e| e.to_string())?;
            let exe = Executable::parse(&bytes).map_err(|e| e.to_string())?;
            let loaded = loader::load(&exe)?;
            eprintln!("[orbis] {} functions bound, {} stubbed; starting", loaded.bound, loaded.stubbed);
            let code = loader::run(&loaded);
            eprintln!("[orbis] exited with {code}");
            Ok(())
        })(),
        Some("selftest") => selftest::run(),
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
