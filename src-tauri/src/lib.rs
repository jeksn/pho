use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;
use tauri::{AppHandle, Emitter};

const RAW_EXTS: &[&str] = &[
    // Canon
    "crw", "cr2", "cr3",
    // Nikon
    "nef", "nrw",
    // Sony / Konica Minolta
    "arw", "srf", "sr2", "mrw",
    // Fujifilm
    "raf",
    // Olympus / OM System
    "orf",
    // Panasonic / Leica
    "rw2", "rwl",
    // Pentax / Ricoh
    "pef",
    // Samsung
    "srw",
    // Sigma
    "x3f",
    // Hasselblad
    "3fr", "fff",
    // Phase One
    "iiq",
    // Leaf
    "mos",
    // Kodak
    "dcr", "kdc", "k25",
    // Epson
    "erf",
    // Mamiya
    "mef",
    // GoPro
    "gpr",
    // Adobe DNG (also DJI drones, phones shooting ProRAW)
    "dng",
    // generic
    "raw",
];
const IMG_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "heic", "heif", "png", "tif", "tiff", "webp", "gif", "bmp", "avif",
    // thumbnail sidecar written next to videos by many cameras (content is jpeg)
    "thm",
];
const VIDEO_EXTS: &[&str] = &[
    "mov", "mp4", "m4v", "mts", "m2ts", "3gp", "mpg", "mpeg", "avi", "wmv", "mkv", "webm",
];
const AUDIO_EXTS: &[&str] = &["wav", "mp3", "m4a", "aac", "aiff", "aif", "flac"];
const STATE_FILE: &str = ".pho.json";

#[derive(Serialize, Clone)]
struct VolumeInfo {
    name: String,
    path: String,
    has_dcim: bool,
    file_count: u32,
}

#[derive(Serialize, Clone)]
struct SessionInfo {
    name: String,
    path: String,
    total: usize,
    pending: usize,
    kept: usize,
    discarded: usize,
}

#[derive(Serialize, Clone)]
struct FileEntry {
    path: String,
    name: String,
    ext: String,
    kind: String,
}

#[derive(Serialize, Clone)]
struct Shot {
    id: String,
    name: String,
    dir: String,
    preview: String,
    files: Vec<FileEntry>,
    state: String,
}

#[derive(Serialize, Clone)]
struct Stats {
    total: usize,
    pending: usize,
    kept: usize,
    discarded: usize,
}

#[derive(Serialize, Clone)]
struct ImportSummary {
    dest: String,
    total: usize,
    copied: usize,
}

#[derive(Serialize, Deserialize, Clone)]
struct ShotEntry {
    state: String,
    preview: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct StateFile {
    shots: HashMap<String, ShotEntry>,
}

// ---------- helpers ----------

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn imports_root() -> PathBuf {
    home_dir().join("Pictures").join("Imports")
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn kind_of(ext: &str) -> Option<&'static str> {
    if RAW_EXTS.contains(&ext) {
        Some("raw")
    } else if IMG_EXTS.contains(&ext) {
        Some("image")
    } else if VIDEO_EXTS.contains(&ext) {
        Some("video")
    } else if AUDIO_EXTS.contains(&ext) {
        Some("audio")
    } else {
        None
    }
}

fn is_junk_name(name: &str) -> bool {
    name.starts_with('.') || name.starts_with("._")
}

fn walk_files(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(root) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if is_junk_name(&name) {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                walk_files(&p, out);
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
}

fn mtime_secs(p: &Path) -> u64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
        .unwrap_or(0)
}

fn natural_key(s: &str) -> Vec<String> {
    fn push_chunk(out: &mut Vec<String>, cur: &str, digit: bool) {
        if cur.is_empty() {
            return;
        }
        if digit {
            let n: u128 = cur.parse().unwrap_or(0);
            out.push(format!("1{n:032}"));
        } else {
            out.push(format!("0{}", cur.to_lowercase()));
        }
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_digit = false;
    for ch in s.chars() {
        let d = ch.is_ascii_digit();
        if d != cur_digit && !cur.is_empty() {
            push_chunk(&mut out, &cur, cur_digit);
            cur.clear();
        }
        cur_digit = d;
        cur.push(ch);
    }
    push_chunk(&mut out, &cur, cur_digit);
    out
}

fn path_key(path: &str) -> String {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    format!("{:x}", h.finish())
}

fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

// ---------- shot grouping ----------

fn choose_preview(files: &[FileEntry]) -> String {
    files
        .iter()
        .find(|f| f.kind == "image" && (f.ext == "jpg" || f.ext == "jpeg"))
        .or_else(|| files.iter().find(|f| f.kind == "image"))
        .or_else(|| files.iter().find(|f| f.kind == "raw"))
        .or_else(|| files.iter().next())
        .map(|f| f.path.clone())
        .unwrap_or_default()
}

fn stem_of(name: &str, ext: &str) -> String {
    let suffix = format!(".{ext}");
    if name.len() > suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(&suffix) {
        name[..name.len() - suffix.len()].to_string()
    } else {
        name.to_string()
    }
}

fn group_shots(folder: &Path) -> Vec<Shot> {
    let mut files = Vec::new();
    walk_files(folder, &mut files);
    let mut map: HashMap<(String, String), Vec<FileEntry>> = HashMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    for f in files {
        let ext = ext_of(&f);
        let Some(kind) = kind_of(&ext) else { continue };
        let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
        let stem = stem_of(&name, &ext);
        let rel_dir = f
            .parent()
            .and_then(|p| p.strip_prefix(folder).ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let key = (rel_dir.clone(), stem.to_lowercase());
        if !map.contains_key(&key) {
            order.push(key.clone());
        }
        map.entry(key).or_default().push(FileEntry {
            path: f.to_string_lossy().to_string(),
            name,
            ext,
            kind: kind.to_string(),
        });
    }
    let mut shots: Vec<Shot> = order
        .into_iter()
        .map(|(dir, stem_lower)| {
            let mut files = map.remove(&(dir.clone(), stem_lower.clone())).unwrap_or_default();
            files.sort_by(|a, b| a.ext.cmp(&b.ext));
            let name = files
                .first()
                .map(|f| stem_of(&f.name, &f.ext))
                .unwrap_or_else(|| stem_lower.clone());
            Shot {
                id: if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") },
                preview: choose_preview(&files),
                name,
                dir,
                files,
                state: "pending".to_string(),
            }
        })
        .collect();
    shots.sort_by(|a, b| {
        natural_key(&a.name)
            .cmp(&natural_key(&b.name))
            .then_with(|| a.dir.cmp(&b.dir))
    });
    shots
}

// ---------- session state ----------

fn state_path(folder: &Path) -> PathBuf {
    folder.join(STATE_FILE)
}

fn load_states(folder: &Path) -> StateFile {
    let mut sf: StateFile = fs::read_to_string(state_path(folder))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // sessions written by older versions used "culled" for what is now "discarded"
    for e in sf.shots.values_mut() {
        if e.state == "culled" {
            e.state = "discarded".to_string();
        }
    }
    sf
}

fn save_states(folder: &Path, states: &StateFile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(states).map_err(|e| e.to_string())?;
    fs::write(state_path(folder), json).map_err(|e| e.to_string())
}

fn compute_session(folder: &Path) -> (Vec<Shot>, Stats) {
    let mut shots = group_shots(folder);
    let states = load_states(folder);
    let disk_ids: HashSet<String> = shots.iter().map(|s| s.id.clone()).collect();
    for s in shots.iter_mut() {
        if let Some(e) = states.shots.get(&s.id) {
            s.state = e.state.clone();
            if let Some(p) = &e.preview {
                if !p.is_empty() {
                    s.preview = p.clone();
                }
            }
        }
        if s.preview.is_empty() {
            s.preview = choose_preview(&s.files);
        }
    }
    // shots that were discarded (files removed from disk) stay visible from the state file
    for (id, e) in &states.shots {
        if !disk_ids.contains(id) {
            let (dir, name) = match id.rsplit_once('/') {
                Some((d, n)) => (d.to_string(), n.to_string()),
                None => (String::new(), id.clone()),
            };
            shots.push(Shot {
                id: id.clone(),
                name,
                dir,
                preview: e.preview.clone().unwrap_or_default(),
                files: Vec::new(),
                state: e.state.clone(),
            });
        }
    }
    shots.sort_by(|a, b| {
        natural_key(&a.name)
            .cmp(&natural_key(&b.name))
            .then_with(|| a.dir.cmp(&b.dir))
    });
    let stats = Stats {
        total: shots.len(),
        pending: shots.iter().filter(|s| s.state == "pending").count(),
        kept: shots.iter().filter(|s| s.state == "kept").count(),
        discarded: shots.iter().filter(|s| s.state == "discarded").count(),
    };
    (shots, stats)
}

fn set_state(folder: &Path, id: &str, state: &str) -> Result<Stats, String> {
    let mut states = load_states(folder);
    let (shots, _) = compute_session(folder);
    let preview = shots
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.preview.clone());
    states.shots.insert(
        id.to_string(),
        ShotEntry {
            state: state.to_string(),
            preview,
        },
    );
    save_states(folder, &states)?;
    let (_, stats) = compute_session(folder);
    Ok(stats)
}

fn trash_file(p: &Path) -> bool {
    let esc = p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("tell application \"Finder\" to delete POSIX file \"{esc}\"");
    let ok = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        true
    } else {
        fs::remove_file(p).is_ok()
    }
}

fn discard_set(folder: &Path, id: &str, use_trash: bool) -> Result<Stats, String> {
    let (shots, _) = compute_session(folder);
    let shot = shots
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("shot not found: {id}"))?;
    for f in &shot.files {
        let p = Path::new(&f.path);
        if p.is_file() {
            if use_trash {
                trash_file(p);
            } else {
                let _ = fs::remove_file(p);
            }
        }
    }
    set_state(folder, id, "discarded")
}

// ---------- import ----------

fn unique_dest(root: &Path, date: &str) -> PathBuf {
    let mut cand = root.join(date);
    let mut i = 2;
    while cand.exists() {
        cand = root.join(format!("{date}-{i}"));
        i += 1;
    }
    cand
}

fn do_import(
    src: &Path,
    dest_root: &Path,
    date: &str,
    progress: &dyn Fn(usize, usize, &str),
) -> Result<ImportSummary, String> {
    if !src.is_dir() {
        return Err(format!("not a directory: {}", src.display()));
    }
    let mut files = Vec::new();
    walk_files(src, &mut files);
    let total = files.len();
    if total == 0 {
        return Err("no files found on source".to_string());
    }
    let dest = unique_dest(dest_root, date);
    fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let mut copied = 0usize;
    for (i, f) in files.iter().enumerate() {
        let rel = f.strip_prefix(src).map_err(|e| e.to_string())?;
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(f, &target).map_err(|e| format!("copy failed for {}: {e}", f.display()))?;
        copied += 1;
        progress(
            i + 1,
            total,
            &f.file_name().unwrap_or_default().to_string_lossy(),
        );
    }
    // seed state file so discarded shots remain visible after their files are removed
    let shots = group_shots(&dest);
    let mut states = StateFile::default();
    for s in &shots {
        states.shots.insert(
            s.id.clone(),
            ShotEntry {
                state: "pending".to_string(),
                preview: Some(s.preview.clone()),
            },
        );
    }
    save_states(&dest, &states)?;
    Ok(ImportSummary {
        dest: dest.to_string_lossy().to_string(),
        total,
        copied,
    })
}

// ---------- preview ----------

fn ql_thumbnail(p: &Path, size: u32, cache_dir: &Path, key: &str) -> Option<PathBuf> {
    let png = cache_dir.join(format!("{key}_ql.png"));
    if !png.is_file() || mtime_secs(&png) < mtime_secs(p) {
        let _ = Command::new("qlmanage")
            .arg("-t")
            .arg("-s")
            .arg(size.to_string())
            .arg(p)
            .arg("-o")
            .arg(cache_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let produced = cache_dir.join(format!(
            "{}.png",
            p.file_name().unwrap_or_default().to_string_lossy()
        ));
        if produced.is_file() && produced != png {
            let _ = fs::rename(&produced, &png);
        }
    }
    if png.is_file() {
        Some(png)
    } else {
        None
    }
}

fn sips_downscale(src: &Path, size: u32, out: &Path) -> bool {
    Command::new("sips")
        .arg("-Z")
        .arg(size.to_string())
        .arg("-s")
        .arg("format")
        .arg("jpeg")
        .arg(src)
        .arg("--out")
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success() && out.is_file())
        .unwrap_or(false)
}

fn preview_base64(path: &str, size: u32) -> Result<String, String> {
    let p = Path::new(path);
    if !p.is_file() {
        return Err(format!("no such file: {path}"));
    }
    let cache_dir = home_dir().join("Library").join("Caches").join("pho");
    fs::create_dir_all(&cache_dir).map_err(|e| e.to_string())?;
    let key = path_key(path);
    let out = cache_dir.join(format!("{key}_{size}.jpg"));
    if out.is_file() && mtime_secs(&out) >= mtime_secs(p) {
        let bytes = fs::read(&out).map_err(|e| e.to_string())?;
        return Ok(base64_encode(&bytes));
    }
    let kind = kind_of(&ext_of(p));
    let made = match kind {
        // sips cannot read raw/video/audio — go through QuickLook
        Some("raw") | Some("video") | Some("audio") | None => ql_thumbnail(p, size, &cache_dir, &key)
            .map(|png| sips_downscale(&png, size, &out))
            .unwrap_or(false),
        // sips handles common image formats directly; fall back to QuickLook for the rest
        Some(_) => {
            sips_downscale(p, size, &out)
                || ql_thumbnail(p, size, &cache_dir, &key)
                    .map(|png| sips_downscale(&png, size, &out))
                    .unwrap_or(false)
        }
    };
    if !made {
        return Err(format!("could not generate preview for {path}"));
    }
    let bytes = fs::read(&out).map_err(|e| e.to_string())?;
    Ok(base64_encode(&bytes))
}

// ---------- commands ----------

#[tauri::command]
fn list_volumes() -> Result<Vec<VolumeInfo>, String> {
    let mut out = Vec::new();
    let rd = fs::read_dir("/Volumes").map_err(|e| e.to_string())?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with("Macintosh HD") {
            continue;
        }
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let has_dcim = p.join("DCIM").is_dir();
        let mut count = 0u32;
        count_media(&p, &mut count, 9999);
        out.push(VolumeInfo {
            name,
            path: p.to_string_lossy().to_string(),
            has_dcim,
            file_count: count,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn count_media(dir: &Path, count: &mut u32, cap: u32) {
    if *count >= cap {
        return;
    }
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if is_junk_name(&name) {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                count_media(&p, count, cap);
            } else if kind_of(&ext_of(&p)).is_some() {
                *count += 1;
            }
            if *count >= cap {
                return;
            }
        }
    }
}

#[tauri::command]
async fn import_media(app: AppHandle, src: String) -> Result<ImportSummary, String> {
    let dest_root = imports_root();
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let src2 = src.clone();
    tauri::async_runtime::spawn_blocking(move || {
        do_import(Path::new(&src2), &dest_root, &date, &|done, total, name| {
            let _ = app.emit(
                "import-progress",
                serde_json::json!({ "done": done, "total": total, "file": name }),
            );
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn list_sessions() -> Result<Vec<SessionInfo>, String> {
    let root = imports_root();
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(&root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let (_, stats) = compute_session(&p);
                out.push(SessionInfo {
                    name: e.file_name().to_string_lossy().to_string(),
                    path: p.to_string_lossy().to_string(),
                    total: stats.total,
                    pending: stats.pending,
                    kept: stats.kept,
                    discarded: stats.discarded,
                });
            }
        }
    }
    out.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(out)
}

// pick up to `limit` preview paths, evenly spaced across the session's shots
fn pick_session_previews(folder: &Path, limit: u32) -> Vec<String> {
    let (shots, _) = compute_session(folder);
    let with_preview: Vec<&Shot> = shots.iter().filter(|s| !s.preview.is_empty()).collect();
    let n = with_preview.len();
    if n == 0 {
        return Vec::new();
    }
    let take = (limit as usize).max(1).min(n);
    (0..take)
        .map(|i| with_preview[i * n / take].preview.clone())
        .collect()
}

fn session_preview_b64(folder: &Path, limit: u32) -> Vec<String> {
    pick_session_previews(folder, limit)
        .iter()
        .filter_map(|p| preview_base64(p, 240).ok())
        .collect()
}

#[tauri::command]
async fn session_previews(folder: String, limit: u32) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || Ok(session_preview_b64(Path::new(&folder), limit)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn list_shots(folder: String) -> Result<Vec<Shot>, String> {
    let (shots, _) = compute_session(Path::new(&folder));
    Ok(shots)
}

#[tauri::command]
fn session_stats(folder: String) -> Result<Stats, String> {
    let (_, stats) = compute_session(Path::new(&folder));
    Ok(stats)
}

#[tauri::command]
fn set_shot_state(folder: String, id: String, state: String) -> Result<Stats, String> {
    set_state(Path::new(&folder), &id, &state)
}

#[tauri::command]
fn discard_shot(folder: String, id: String) -> Result<Stats, String> {
    discard_set(Path::new(&folder), &id, true)
}

#[tauri::command]
fn library_path() -> String {
    imports_root().to_string_lossy().to_string()
}

#[tauri::command]
async fn preview(path: String, size: u32) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || preview_base64(&path, size))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    let esc = path.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "tell application \"Finder\" to reveal POSIX file \"{esc}\"\ntell application \"Finder\" to activate"
    );
    Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            list_volumes,
            import_media,
            list_sessions,
            session_previews,
            list_shots,
            session_stats,
            set_shot_state,
            discard_shot,
            library_path,
            preview,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("error while running pho");
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmpdir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("pho-test-{}-{n}", std::process::id()));
        let p = p.with_file_name(format!("{tag}-{}-{n}", p.file_name().unwrap().to_string_lossy()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn natural_sort_orders_numbers() {
        let mut v = vec!["IMG_10", "IMG_2", "IMG_1", "IMG_20", "A1"];
        v.sort_by_key(|s| natural_key(s));
        assert_eq!(v, vec!["A1", "IMG_1", "IMG_2", "IMG_10", "IMG_20"]);
    }

    #[test]
    fn groups_raw_jpeg_pairs_by_stem() {
        let root = tmpdir("group");
        write(&root.join("DCIM/100CANON/IMG_0001.ARW"), "raw");
        write(&root.join("DCIM/100CANON/IMG_0001.JPG"), "jpg");
        write(&root.join("DCIM/100CANON/IMG_0002.JPG"), "jpg");
        write(&root.join("DCIM/100CANON/IMG_0010.CR3"), "raw");
        write(&root.join("DCIM/100CANON/notes.txt"), "hi");
        write(&root.join("DCIM/100CANON/.hidden.jpg"), "x");

        let shots = group_shots(&root);
        assert_eq!(shots.len(), 3);
        assert_eq!(shots[0].name, "IMG_0001");
        assert_eq!(shots[0].files.len(), 2);
        let exts: Vec<&str> = shots[0].files.iter().map(|f| f.ext.as_str()).collect();
        assert!(exts.contains(&"arw") && exts.contains(&"jpg"));
        assert!(shots[0].preview.ends_with("IMG_0001.JPG"));
        assert_eq!(shots[1].name, "IMG_0002");
        assert_eq!(shots[1].files.len(), 1);
        assert_eq!(shots[2].name, "IMG_0010");
        assert!(shots[2].preview.ends_with("IMG_0010.CR3"));
    }

    #[test]
    fn import_copies_skips_junk_and_seeds_state() {
        let cam = tmpdir("cam");
        write(&cam.join("DCIM/100CANON/IMG_0001.ARW"), "raw");
        write(&cam.join("DCIM/100CANON/IMG_0001.JPG"), "jpg");
        write(&cam.join("DCIM/100CANON/IMG_0002.JPG"), "jpg");
        write(&cam.join(".Trashes/junk.jpg"), "junk");
        write(&cam.join("._IMG_0001.JPG"), "appledouble");
        let dest_root = tmpdir("dest");

        let sum = do_import(&cam, &dest_root, "2026-02-14", &|_d, _t, _n| {}).unwrap();
        assert_eq!(sum.copied, 3);
        assert!(sum.dest.ends_with("2026-02-14"));
        assert!(Path::new(&sum.dest).join("DCIM/100CANON/IMG_0001.ARW").is_file());
        assert!(!Path::new(&sum.dest).join(".Trashes/junk.jpg").exists());
        assert!(!Path::new(&sum.dest).join("._IMG_0001.JPG").exists());

        let states = load_states(Path::new(&sum.dest));
        assert_eq!(states.shots.len(), 2);
        assert_eq!(states.shots["DCIM/100CANON/IMG_0001"].state, "pending");

        // second import same day gets a suffix
        let sum2 = do_import(&cam, &dest_root, "2026-02-14", &|_d, _t, _n| {}).unwrap();
        assert!(sum2.dest.ends_with("2026-02-14-2"));
    }

    #[test]
    fn discard_removes_both_pair_files_and_marks_state() {
        let root = tmpdir("discard");
        write(&root.join("IMG_0001.ARW"), "raw");
        write(&root.join("IMG_0001.JPG"), "jpg");
        write(&root.join("IMG_0002.JPG"), "jpg");
        save_states(
            &root,
            &StateFile {
                shots: HashMap::from([(
                    "IMG_0001".to_string(),
                    ShotEntry { state: "pending".to_string(), preview: None },
                )]),
            },
        )
        .unwrap();

        let stats = discard_set(&root, "IMG_0001", false).unwrap();
        assert!(!root.join("IMG_0001.ARW").exists());
        assert!(!root.join("IMG_0001.JPG").exists());
        assert!(root.join("IMG_0002.JPG").exists());
        assert_eq!(stats.discarded, 1);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.total, 2);

        // discarded shot still listed (from state file) with files gone
        let (shots, _) = compute_session(&root);
        let s = shots.iter().find(|s| s.id == "IMG_0001").unwrap();
        assert_eq!(s.state, "discarded");
        assert!(s.files.is_empty());
    }

    #[test]
    fn legacy_culled_state_is_read_as_discarded() {
        let root = tmpdir("legacy");
        write(&root.join("IMG_0001.JPG"), "jpg");
        let mut shots = HashMap::new();
        shots.insert(
            "IMG_0001".to_string(),
            ShotEntry { state: "culled".to_string(), preview: None },
        );
        save_states(&root, &StateFile { shots }).unwrap();
        let (shots, stats) = compute_session(&root);
        assert_eq!(shots[0].state, "discarded");
        assert_eq!(stats.discarded, 1);
    }

    #[test]
    fn preview_pipeline_produces_jpeg_for_image_and_raw() {
        let root = tmpdir("prev");
        let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/icon.png");
        let jpg = root.join("shot.jpg");
        let raw = root.join("shot.arw");
        fs::copy(&icon, &jpg).unwrap();
        fs::copy(&icon, &raw).unwrap(); // qlmanage sniffs content, works for the test

        let b64 = preview_base64(jpg.to_str().unwrap(), 300).unwrap();
        assert!(b64.starts_with("/9j/"), "expected jpeg magic in base64");

        let b64_raw = preview_base64(raw.to_str().unwrap(), 300).unwrap();
        assert!(b64_raw.starts_with("/9j/"));

        // second call hits the cache and still works
        let b64b = preview_base64(jpg.to_str().unwrap(), 300).unwrap();
        assert_eq!(b64, b64b);
    }

    #[test]
    fn full_session_flow() {
        // fake camera card
        let cam = tmpdir("e2e-cam");
        let d1 = cam.join("DCIM/100CANON");
        write(&d1.join("IMG_0001.ARW"), "raw1");
        write(&d1.join("IMG_0001.JPG"), "jpg1");
        write(&d1.join("IMG_0002.JPG"), "jpg2");
        write(&d1.join("IMG_0010.ARW"), "raw10");
        write(&d1.join("IMG_0010.JPG"), "jpg10");
        let d2 = cam.join("DCIM/101CANON");
        write(&d2.join("IMG_0011.CR3"), "raw11");
        write(&d2.join("IMG_0011.JPG"), "jpg11");
        write(&cam.join("MISC/readme.txt"), "not media");

        let dest_root = tmpdir("e2e-dest");
        let sum = do_import(&cam, &dest_root, "2026-02-14", &|_d, _t, _n| {}).unwrap();
        assert_eq!(sum.copied, 8); // all files, including readme.txt
        let dest = PathBuf::from(&sum.dest);

        // sorted in shooting order
        let (shots, stats) = compute_session(&dest);
        let names: Vec<&str> = shots.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["IMG_0001", "IMG_0002", "IMG_0010", "IMG_0011"]);
        assert_eq!(stats.total, 4);
        assert_eq!(stats.pending, 4);

        // keep one, discard a raw+jpeg pair
        let stats = set_state(&dest, "DCIM/100CANON/IMG_0001", "kept").unwrap();
        assert_eq!(stats.kept, 1);
        let stats = discard_set(&dest, "DCIM/100CANON/IMG_0010", false).unwrap();
        assert_eq!(stats.discarded, 1);
        assert!(!dest.join("DCIM/100CANON/IMG_0010.ARW").exists());
        assert!(!dest.join("DCIM/100CANON/IMG_0010.JPG").exists());

        // resume: discarded shot still listed with empty files, preview retained
        let (shots, stats) = compute_session(&dest);
        assert_eq!(stats.total, 4);
        let discarded = shots.iter().find(|s| s.name == "IMG_0010").unwrap();
        assert_eq!(discarded.state, "discarded");
        assert!(discarded.files.is_empty());
        assert!(discarded.preview.ends_with("IMG_0010.JPG"));
    }

    #[test]
    fn classifies_common_camera_formats() {
        let cases: &[(&str, &str)] = &[
            // raw (checked case-insensitively via ext_of in practice)
            ("orf", "raw"), ("crw", "raw"), ("cr2", "raw"), ("cr3", "raw"),
            ("nef", "raw"), ("nrw", "raw"), ("arw", "raw"), ("srf", "raw"), ("sr2", "raw"),
            ("mrw", "raw"), ("raf", "raw"), ("rw2", "raw"), ("rwl", "raw"),
            ("pef", "raw"), ("srw", "raw"), ("x3f", "raw"), ("3fr", "raw"), ("fff", "raw"),
            ("iiq", "raw"), ("mos", "raw"), ("dcr", "raw"), ("kdc", "raw"), ("k25", "raw"),
            ("erf", "raw"), ("mef", "raw"), ("gpr", "raw"), ("dng", "raw"), ("raw", "raw"),
            // image
            ("jpg", "image"), ("jpeg", "image"), ("jpe", "image"), ("heic", "image"),
            ("heif", "image"), ("png", "image"), ("tif", "image"), ("tiff", "image"),
            ("webp", "image"), ("gif", "image"), ("bmp", "image"), ("avif", "image"), ("thm", "image"),
            // video / audio
            ("mov", "video"), ("mp4", "video"), ("m4v", "video"), ("mts", "video"),
            ("m2ts", "video"), ("3gp", "video"), ("mpg", "video"), ("mpeg", "video"),
            ("avi", "video"), ("wmv", "video"), ("mkv", "video"), ("webm", "video"),
            ("wav", "audio"), ("mp3", "audio"), ("m4a", "audio"), ("aac", "audio"),
            ("aiff", "audio"), ("aif", "audio"), ("flac", "audio"),
            // not media
            ("txt", ""), ("exe", ""), ("json", ""), ("", ""),
        ];
        for (ext, want) in cases {
            assert_eq!(kind_of(ext).unwrap_or(""), *want, "ext {ext}");
        }
        // uppercase extensions classify the same
        for ext in ["ORF", "JPG", "HEIC", "MOV", "CR3", "DNG"] {
            assert!(kind_of(&ext.to_lowercase()).is_some(), "ext {ext}");
        }
    }

    #[test]
    fn groups_orf_pair_and_video_sidecars() {
        let root = tmpdir("orf");
        // Olympus raw+jpeg pair
        write(&root.join("P1010001.ORF"), "raw");
        write(&root.join("P1010001.JPG"), "jpg");
        // video with thm + wav sidecars shares the stem
        write(&root.join("P1010002.MOV"), "vid");
        write(&root.join("P1010002.THM"), "thm");
        write(&root.join("P1010002.WAV"), "wav");

        let shots = group_shots(&root);
        assert_eq!(shots.len(), 2);
        assert_eq!(shots[0].name, "P1010001");
        assert_eq!(shots[0].files.len(), 2);
        assert!(shots[0].preview.ends_with("P1010001.JPG"));
        assert_eq!(shots[1].name, "P1010002");
        assert_eq!(shots[1].files.len(), 3);
        let kinds: Vec<&str> = shots[1].files.iter().map(|f| f.kind.as_str()).collect();
        assert!(kinds.contains(&"video") && kinds.contains(&"image") && kinds.contains(&"audio"));
    }

    #[test]
    fn session_previews_are_capped_and_evenly_spaced() {
        let root = tmpdir("sessprev");
        let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/icon.png");
        for i in 1..=10 {
            fs::copy(&icon, root.join(format!("IMG_{i:04}.JPG"))).unwrap();
        }
        let picks = pick_session_previews(&root, 4);
        let names: Vec<String> = picks
            .iter()
            .map(|p| Path::new(p).file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["IMG_0001.JPG", "IMG_0003.JPG", "IMG_0006.JPG", "IMG_0008.JPG"]);

        // limit above shot count returns all shots
        assert_eq!(pick_session_previews(&root, 99).len(), 10);
        // and the base64 variant produces jpeg data
        let b64 = session_preview_b64(&root, 3);
        assert_eq!(b64.len(), 3);
        assert!(b64.iter().all(|b| b.starts_with("/9j/")));
    }

    #[test]
    fn base64_roundtrip() {
        let data: Vec<u8> = (0u32..1000).map(|i| (i % 251) as u8).collect();
        let enc = base64_encode(&data);
        // decode
        let mut rev = Vec::new();
        let mut buf = 0u32;
        let mut nbits = 0;
        for c in enc.chars().filter(|c| *c != '=') {
            let v = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
                .find(c)
                .unwrap() as u32;
            buf = (buf << 6) | v;
            nbits += 6;
            if nbits >= 8 {
                nbits -= 8;
                rev.push((buf >> nbits) as u8);
            }
        }
        assert_eq!(rev, data);
    }
}
