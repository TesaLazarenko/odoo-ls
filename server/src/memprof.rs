//! Opt-in memory profiling of `--parse` runs, used to measure the memory work of this
//! fork. Enabled by `OLS_MEMPROF=<csv path>`; inert otherwise.
//!
//! Writes one CSV line (time, kind, label, jemalloc allocated/active/resident/mapped/
//! retained, VmRSS) every 200 ms and at every build phase change (`phase`), plus `mark`
//! lines from `CliBackend::memprof_attribution` at the end of the run, which frees the
//! state part by part to attribute the retained memory.
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static PHASE: Mutex<&'static str> = Mutex::new("start");
static OUT: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stats() -> [usize; 5] {
    use tikv_jemalloc_ctl::{epoch, stats};
    let _ = epoch::advance();
    [
        stats::allocated::read().unwrap_or(0),
        stats::active::read().unwrap_or(0),
        stats::resident::read().unwrap_or(0),
        stats::mapped::read().unwrap_or(0),
        stats::retained::read().unwrap_or(0),
    ]
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn stats() -> [usize; 5] { [0; 5] }

fn rss_kb() -> usize {
    std::fs::read_to_string("/proc/self/status").ok().and_then(|s| {
        s.lines().find(|l| l.starts_with("VmRSS:"))
            .and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok())
    }).unwrap_or(0)
}

pub fn allocated() -> usize { stats()[0] }

fn write_line(kind: &str, label: &str) {
    let Some(out) = OUT.get() else { return };
    let t = START.get().map(|s| s.elapsed().as_millis()).unwrap_or(0);
    let [a, ac, r, m, re] = stats();
    let mut f = out.lock().unwrap();
    let _ = writeln!(f, "{t},{kind},{label},{a},{ac},{r},{m},{re},{}", rss_kb());
}

pub fn init() {
    let Ok(path) = std::env::var("OLS_MEMPROF") else { return };
    let mut f = std::fs::File::create(path).expect("memprof file");
    let _ = writeln!(f, "t_ms,kind,label,allocated,active,resident,mapped,retained,rss_kb");
    let _ = OUT.set(Mutex::new(f));
    let _ = START.set(Instant::now());
    std::thread::spawn(|| loop {
        let p = *PHASE.lock().unwrap();
        write_line("sample", p);
        std::thread::sleep(Duration::from_millis(200));
    });
}

pub fn phase(p: &'static str) {
    *PHASE.lock().unwrap() = p;
    write_line("phase", p);
}

pub fn mark(label: &str) {
    write_line("mark", label);
}

/// Return the free pages of all jemalloc arenas to the OS now, instead of waiting for
/// the decay. Called once a large build is over: its transient memory (ASTs, pre-parse
/// payloads, build slack) was freed from several threads' arenas, and an idle server
/// does not allocate enough to drive the decay of all of them.
pub fn release_free_memory() {
    #[cfg(all(not(debug_assertions), any(target_os = "linux", target_os = "macos")))]
    {
        // "arena.<MALLCTL_ARENAS_ALL = 4096>.purge" is a void control: jemalloc rejects it
        // (EPERM) unless both the old and the new value pointers are null.
        // SAFETY: NUL-terminated name, no input or output buffers.
        unsafe {
            tikv_jemalloc_sys::mallctl(c"arena.4096.purge".as_ptr(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0);
        }
    }
    phase("released");
}
