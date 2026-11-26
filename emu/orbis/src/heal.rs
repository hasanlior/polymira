//! Self-healing for system functions Orbis does not implement.
//!
//! A missing function used to answer 0 to everything. Now each one gets a policy:
//!   * a first guess from its name (dialog status → "finished", network work → "offline" error,
//!     create/open of a network object → a handle, everything else → 0), and
//!   * learning across runs: when the game crashes, the last missing function the crashing thread
//!     called moves to the next policy, saved in `profiles/<title>.heal.json`. The next start
//!     uses it, so a game that dies on a wrong answer tries a different one by itself.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    /// Return 0 (success, empty result).
    Zero,
    /// Return a negative SCE error: "not available" (offline, no service).
    Error,
    /// Return a new positive handle / id.
    Handle,
    /// Return a fixed value (e.g. dialog status FINISHED = 3).
    Value(u64),
    /// Return a pointer to zeroed memory (functions that hand out an address, like errno).
    Buffer,
}

const SCE_ERROR_UNAVAILABLE: u64 = 0x8000_0001;

impl Policy {
    fn name(self) -> String {
        match self {
            Policy::Zero => "zero".into(),
            Policy::Error => "error".into(),
            Policy::Handle => "handle".into(),
            Policy::Value(v) => format!("value:{v}"),
            Policy::Buffer => "buffer".into(),
        }
    }
    fn parse(s: &str) -> Option<Policy> {
        Some(match s {
            "zero" => Policy::Zero,
            "error" => Policy::Error,
            "handle" => Policy::Handle,
            "buffer" => Policy::Buffer,
            _ => Policy::Value(s.strip_prefix("value:")?.parse().ok()?),
        })
    }
    /// The policy tried after this one made the game crash.
    fn next(self) -> Policy {
        match self {
            Policy::Zero => Policy::Error,
            Policy::Error => Policy::Handle,
            Policy::Handle => Policy::Buffer,
            Policy::Buffer | Policy::Value(_) => Policy::Zero,
        }
    }
}

/// First guess from the function's name ("sceNetResolverCreate (C4UgDHHPvdw)").
pub fn guess(label: &str) -> Policy {
    let name = label.split(' ').next().unwrap_or(label);
    if name.len() == 11 && !name.starts_with("sce") {
        return Policy::Zero; // only the NID is known
    }
    let n = name.to_ascii_lowercase();
    if n.ends_with("loc") || n.ends_with("ptr") || n.ends_with("address") && !n.contains("mac") {
        return Policy::Buffer; // returns a pointer the caller reads through
    }
    let network = ["scenet", "scehttp", "scessl", "scenpwebapi", "scenpcommerce", "scenpauth", "scenpsignin", "scesignin", "scenpentitlement", "scenpuniversal", "scewebbrowser", "scenpgameintent"]
        .iter()
        .any(|p| n.starts_with(p));
    let lifecycle = ["initialize", "init", "terminate", "term", "finalize", "register", "unregister", "delete", "destroy", "abort", "close"]
        .iter()
        .any(|w| n.ends_with(w) || n.contains(&format!("{w}internal")));
    if n.contains("dialog") && (n.contains("getstatus") || n.contains("updatestatus")) {
        return Policy::Value(3); // SCE_COMMON_DIALOG_STATUS_FINISHED: never wait for a dialog
    }
    if network {
        if lifecycle {
            return Policy::Zero;
        }
        if n.contains("create") || n.ends_with("open") {
            return Policy::Handle;
        }
        return Policy::Error; // offline: requests, sign-in and store calls fail cleanly
    }
    Policy::Zero
}

struct State {
    file: String,
    learned: HashMap<String, Policy>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    let file = format!("{}/profiles/{}.heal.json", crate::kernel::data_root(), crate::kernel::title_id_safe());
    let learned = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
        .map(|m| m.into_iter().filter_map(|(k, v)| Policy::parse(&v).map(|p| (k, p))).collect())
        .unwrap_or_default();
    Mutex::new(State { file, learned })
});

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Key of a missing function in the heal file: its name, or the NID.
fn key(label: &str) -> &str {
    label.split(' ').next().unwrap_or(label)
}

pub fn policy(label: &str) -> Policy {
    let st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    st.learned.get(key(label)).copied().unwrap_or_else(|| guess(label))
}

/// The value a missing function returns under `p`.
pub fn value(p: Policy) -> u64 {
    match p {
        Policy::Zero => 0,
        Policy::Error => SCE_ERROR_UNAVAILABLE,
        Policy::Handle => NEXT_HANDLE.fetch_add(1, Ordering::Relaxed),
        Policy::Value(v) => v,
        // A fresh zeroed block; the loader keeps one per function (see `fixed`).
        Policy::Buffer => Box::leak(vec![0u64; 8192].into_boxed_slice()).as_mut_ptr() as u64,
    }
}

/// Called from the crash report: the last missing function the crashing thread used gets the
/// next policy for the next run. Must not allocate much or take long.
/// `fault` is the address the crash touched: a low address right after a missing function
/// answered means the game used the answer as a pointer, so it gets real memory next time.
pub fn learn_from_crash(last_stub: Option<&str>, fault: Option<u64>) -> Option<String> {
    let label = last_stub?;
    let mut st = STATE.try_lock().ok()?;
    let k = key(label).to_string();
    let cur = st.learned.get(&k).copied().unwrap_or_else(|| guess(label));
    let pointer_like = fault.is_some_and(|a| a < 0x1_0000_0000);
    let next = if pointer_like && cur != Policy::Buffer { Policy::Buffer } else { cur.next() };
    st.learned.insert(k.clone(), next);
    let map: HashMap<&String, String> = st.learned.iter().map(|(k, v)| (k, v.name())).collect();
    if let Some(dir) = std::path::Path::new(&st.file).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&st.file, serde_json::to_string_pretty(&map).unwrap_or_default());
    Some(format!("{k}: {} -> {}", cur.name(), next.name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses() {
        assert_eq!(guess("sceNetResolverCreate (C4UgDHHPvdw)"), Policy::Handle);
        assert_eq!(guess("sceHttp2Init (3JCe3lCbQ8A)"), Policy::Zero);
        assert_eq!(guess("sceHttp2SendRequest (xxxxxxxxxxx)"), Policy::Error);
        assert_eq!(guess("sceSigninDialogUpdateStatus (xxxxxxxxxxx)"), Policy::Value(3));
        assert_eq!(guess("YFC3dBBipj8"), Policy::Zero);
        assert_eq!(guess("sceImeUpdate (-4GCfYdNF1s)"), Policy::Zero);
    }

    #[test]
    fn rotation() {
        assert_eq!(Policy::Zero.next(), Policy::Error);
        assert_eq!(Policy::Error.next(), Policy::Handle);
        assert_eq!(Policy::Handle.next(), Policy::Buffer);
        assert_eq!(Policy::Value(3).next(), Policy::Zero);
        assert_eq!(guess("sceNetErrnoLoc (HQOwnfMGipQ)"), Policy::Buffer);
    }
}
