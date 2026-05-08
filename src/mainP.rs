use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

// ── ANSI helpers ──────────────────────────────────────────────────────────────
const RESET:   &str = "\x1b[0m";
const BOLD:    &str = "\x1b[1m";
const DIM:     &str = "\x1b[2m";
const CYAN:    &str = "\x1b[36m";
const YELLOW:  &str = "\x1b[33m";
const GREEN:   &str = "\x1b[32m";
const RED:     &str = "\x1b[31m";
const REVERSE: &str = "\x1b[7m";

macro_rules! bold   { ($s:expr) => { format!("{BOLD}{}{RESET}",    $s) }; }
macro_rules! dim    { ($s:expr) => { format!("{DIM}{}{RESET}",     $s) }; }
macro_rules! cyan   { ($s:expr) => { format!("{CYAN}{}{RESET}",    $s) }; }
macro_rules! yellow { ($s:expr) => { format!("{YELLOW}{}{RESET}",  $s) }; }
macro_rules! green  { ($s:expr) => { format!("{GREEN}{}{RESET}",   $s) }; }
macro_rules! red    { ($s:expr) => { format!("{RED}{}{RESET}",     $s) }; }
macro_rules! rev    { ($s:expr) => { format!("{REVERSE}{}{RESET}", $s) }; }

// ── CLI ───────────────────────────────────────────────────────────────────────
#[derive(Parser)]
#[command(name = "mac_cleaner", about = "Find and remove the largest files/folders")]
struct Cli {
    /// Root path to scan
    #[arg(default_value = "/System/Volumes/Data")]
    path: PathBuf,

    /// Top N items to show
    #[arg(short, long, default_value_t = 20)]
    top: usize,

    /// Scan depth (1 = direct children, 2 = one level deeper, 0 = unlimited)
    #[arg(short, long, default_value_t = 2)]
    depth: usize,
}

// ── Raw terminal ──────────────────────────────────────────────────────────────
struct RawMode {
    orig: libc::termios,
}

impl RawMode {
    fn enable() -> Self {
        unsafe {
            let mut orig: libc::termios = std::mem::zeroed();
            libc::tcgetattr(libc::STDIN_FILENO, &mut orig);
            let mut raw = orig;
            raw.c_lflag &= !(libc::ECHO | libc::ICANON);
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw);
            Self { orig }
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.orig);
        }
    }
}

fn term_size() -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 4 {
            (ws.ws_col as usize, ws.ws_row as usize)
        } else {
            (80, 24)
        }
    }
}

// ── Key input ─────────────────────────────────────────────────────────────────
enum Key {
    Up,
    Down,
    Enter,
    Space,
    Backspace,
    Char(char),
    Esc,
}

fn read_byte() -> u8 {
    let mut b = [0u8; 1];
    io::stdin().read_exact(&mut b).ok();
    b[0]
}

fn read_byte_timeout(ms: u64) -> Option<u8> {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        libc::tcgetattr(libc::STDIN_FILENO, &mut t);
        let saved = t;
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = ((ms / 100).max(1).min(255)) as libc::cc_t;
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t);
        let mut b = [0u8; 1];
        let n = io::stdin().read(&mut b).unwrap_or(0);
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved);
        if n > 0 { Some(b[0]) } else { None }
    }
}

fn read_key() -> Key {
    match read_byte() {
        b'\r' | b'\n' => Key::Enter,
        b' '          => Key::Space,
        0x7f | 0x08   => Key::Backspace,
        0x1b => match read_byte_timeout(100) {
            Some(b'[') => match read_byte_timeout(100) {
                Some(b'A') => Key::Up,
                Some(b'B') => Key::Down,
                _          => Key::Esc,
            },
            _ => Key::Esc,
        },
        c if c >= 0x20 => Key::Char(c as char),
        _ => Key::Esc,
    }
}

// ── Misc helpers ──────────────────────────────────────────────────────────────
fn human(bytes: u64) -> String {
    const U: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < U.len() { v /= 1024.0; u += 1; }
    if u == 0 { format!("{} B", bytes) } else { format!("{:.1} {}", v, U[u]) }
}

fn open_in_finder(path: &Path) {
    Command::new("open").arg("-R").arg(path).spawn().ok();
}

fn delete_item(path: &Path) -> io::Result<()> {
    if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

/// Read typed characters until Enter, with backspace support.
fn collect_chars() -> String {
    let mut s = String::new();
    loop {
        match read_key() {
            Key::Enter => return s,
            Key::Backspace if !s.is_empty() => {
                s.pop();
                print!("\x1b[1D \x1b[1D");
                io::stdout().flush().unwrap();
            }
            Key::Char(c) => {
                s.push(c);
                print!("{c}");
                io::stdout().flush().unwrap();
            }
            Key::Esc => return String::new(),
            _ => {}
        }
    }
}

// ── Scanner ───────────────────────────────────────────────────────────────────
struct ScanItem {
    path: PathBuf,
    size: u64,
    is_dir: bool,
}

fn scan(root: &Path, depth: usize) -> Vec<ScanItem> {
    let max_depth = if depth == 0 { usize::MAX } else { depth };
    let mut sizes: HashMap<PathBuf, u64> = HashMap::new();
    let mut count = 0u64;
    visit(root, root, 0, max_depth, &mut sizes, &mut count);
    eprint!("\r\x1b[2K");
    let mut items: Vec<ScanItem> = sizes
        .into_iter()
        .map(|(path, size)| ScanItem { is_dir: path.is_dir(), path, size })
        .collect();
    items.sort_by(|a, b| b.size.cmp(&a.size));
    items
}

fn visit(
    root: &Path, dir: &Path,
    cur: usize, max: usize,
    sizes: &mut HashMap<PathBuf, u64>,
    n: &mut u64,
) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    let mut total = 0u64;
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        *n += 1;
        if *n % 500 == 0 {
            eprint!("\r{}  {} entries scanned\x1b[K", cyan!("⟳"), n);
        }
        if meta.file_type().is_symlink() { continue; }
        if meta.is_file() {
            let sz = meta.len();
            total += sz;
            if let Some(child) = top_child(root, &path) {
                *sizes.entry(child).or_insert(0) += sz;
            }
        } else if meta.is_dir() {
            let sub = if cur + 1 < max {
                visit(root, &path, cur + 1, max, sizes, n)
            } else {
                sum_dir(&path, n)
            };
            total += sub;
            if cur + 1 == max {
                let key = top_child(root, &path).unwrap_or_else(|| path.clone());
                *sizes.entry(key).or_insert(0) += sub;
            }
        }
    }
    total
}

fn sum_dir(dir: &Path, n: &mut u64) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten().map(|e| {
        *n += 1;
        let Ok(m) = e.metadata() else { return 0 };
        if m.is_file() { m.len() }
        else if m.is_dir() { sum_dir(&e.path(), n) }
        else { 0 }
    }).sum()
}

fn top_child(root: &Path, path: &Path) -> Option<PathBuf> {
    path.strip_prefix(root).ok()
        .and_then(|r| r.components().next())
        .map(|c| root.join(c))
}

fn scan_children(dir: &Path) -> Vec<ScanItem> {
    let Ok(rd) = fs::read_dir(dir) else { return vec![] };
    let mut items: Vec<ScanItem> = rd.flatten().filter_map(|e| {
        let path = e.path();
        let meta = e.metadata().ok()?;
        if meta.file_type().is_symlink() { return None; }
        let size = if meta.is_file() { meta.len() } else { dir_size_recursive(&path) };
        Some(ScanItem { is_dir: meta.is_dir(), path, size })
    }).collect();
    items.sort_by(|a, b| b.size.cmp(&a.size));
    items
}

fn dir_size_recursive(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten().map(|e| {
        let Ok(m) = e.metadata() else { return 0 };
        if m.is_file() { m.len() }
        else if m.is_dir() { dir_size_recursive(&e.path()) }
        else { 0 }
    }).sum()
}

// ── Display entry ─────────────────────────────────────────────────────────────
struct Entry {
    path: PathBuf,
    size: u64,
    is_dir: bool,
    depth: usize,
    expanded: bool,
    num: usize, // 1-based top-level label; 0 = expanded child
}

// ── App state ─────────────────────────────────────────────────────────────────
struct App {
    root: PathBuf,
    entries: Vec<Entry>,
    cursor: usize,
    selected: HashSet<PathBuf>,
    scroll: usize,
    input: Option<String>, // Some(_) = comma-list input mode
}

impl App {
    fn new(root: PathBuf, top: Vec<ScanItem>) -> Self {
        let entries = top.into_iter().enumerate().map(|(i, it)| Entry {
            path: it.path, size: it.size, is_dir: it.is_dir,
            depth: 0, expanded: false, num: i + 1,
        }).collect();
        App { root, entries, cursor: 0, selected: HashSet::new(), scroll: 0, input: None }
    }

    fn vis_rows(&self, h: usize) -> usize { h.saturating_sub(5) }

    fn ensure_cursor_visible(&mut self, h: usize) {
        let vis = self.vis_rows(h);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if vis > 0 && self.cursor >= self.scroll + vis {
            self.scroll = self.cursor + 1 - vis;
        }
    }

    fn renumber(&mut self) {
        let mut n = 1;
        for e in &mut self.entries {
            if e.depth == 0 { e.num = n; n += 1; } else { e.num = 0; }
        }
    }

    fn toggle_expand(&mut self, i: usize) {
        if !self.entries[i].is_dir { return; }
        let depth = self.entries[i].depth;
        if self.entries[i].expanded {
            // Collapse: remove following entries deeper than this one
            let end = self.entries[i + 1..]
                .iter()
                .position(|e| e.depth <= depth)
                .map(|p| i + 1 + p)
                .unwrap_or(self.entries.len());
            self.entries.drain(i + 1..end);
            self.entries[i].expanded = false;
        } else {
            // Expand: insert children after i
            let children = scan_children(&self.entries[i].path.clone());
            for (j, c) in children.into_iter().enumerate() {
                self.entries.insert(i + 1 + j, Entry {
                    path: c.path, size: c.size, is_dir: c.is_dir,
                    depth: depth + 1, expanded: false, num: 0,
                });
            }
            self.entries[i].expanded = true;
        }
        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
    }

    fn apply_input_select(&mut self) {
        let buf = self.input.take().unwrap_or_default();
        self.selected.clear();
        for token in buf.split(',') {
            if let Ok(n) = token.trim().parse::<usize>() {
                if let Some(e) = self.entries.iter().find(|e| e.num == n) {
                    self.selected.insert(e.path.clone());
                }
            }
        }
    }

    fn action_targets(&self) -> Vec<(PathBuf, u64)> {
        if self.selected.is_empty() {
            let e = &self.entries[self.cursor];
            vec![(e.path.clone(), e.size)]
        } else {
            self.entries.iter()
                .filter(|e| self.selected.contains(&e.path))
                .map(|e| (e.path.clone(), e.size))
                .collect()
        }
    }

    fn remove_paths(&mut self, paths: &[PathBuf]) {
        self.entries.retain(|e| {
            !paths.iter().any(|p| e.path == *p || e.path.starts_with(p))
        });
        self.selected.clear();
        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
        self.renumber();
    }

    fn render(&self, w: usize, h: usize) {
        let vis = self.vis_rows(h);
        let mut out = String::with_capacity(8192);
        out.push_str("\x1b[H\x1b[2J");

        // Header
        out.push_str(&format!(
            "{} Largest items under {}\n\n",
            green!("◉"), bold!(self.root.display())
        ));

        // Entry list
        let end = (self.scroll + vis).min(self.entries.len());
        for i in self.scroll..end {
            let e = &self.entries[i];
            let is_cursor = i == self.cursor;
            let is_sel    = self.selected.contains(&e.path);

            let sel    = if is_sel { yellow!("✓") } else { " ".to_string() };
            let num    = if e.num > 0 { format!("{:>3}", e.num) } else { "   ".to_string() };
            let indent = "  ".repeat(e.depth);
            let icon   = if e.is_dir { if e.expanded { "📂" } else { "📁" } } else { "📄" };
            let sz     = format!("{:>10}", human(e.size));
            let raw_name = e.path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| e.path.display().to_string());

            let left = 1 + 1 + 3 + 2 + e.depth * 2 + 4 + 2 + sz.len() + 2;
            let max_name = w.saturating_sub(left).max(8);
            let name = if raw_name.chars().count() > max_name {
                format!("{}…", raw_name.chars().take(max_name - 1).collect::<String>())
            } else {
                raw_name
            };

            let line   = format!("{sel} {num}  {indent}{icon}  {sz}  {name}");
            let padded = format!("{:<width$}", line, width = w.min(300));
            out.push_str(&format!("{}\n", if is_cursor { rev!(padded) } else { padded }));
        }

        for _ in end..self.scroll + vis {
            out.push('\n');
        }

        // Footer
        out.push_str(&format!("{}\n", dim!("─".repeat(w.min(100)))));
        if let Some(ref buf) = self.input {
            out.push_str(&format!(
                "  {} {}█  {}\n",
                bold!("Select:"), yellow!(buf),
                dim!("comma-separated numbers · Enter=confirm · Esc=cancel")
            ));
        } else {
            let sel_info = if !self.selected.is_empty() {
                format!("{}  ", yellow!(format!("{} ✓ selected", self.selected.len())))
            } else {
                String::new()
            };
            out.push_str(&format!(
                "  {}{}  {}  {}  {}  {}  {}\n",
                sel_info,
                dim!("↑↓ move"),
                dim!("Space expand"),
                dim!("s select cursor"),
                dim!("1,2,… multi-select"),
                dim!("Enter action"),
                dim!("q quit"),
            ));
        }

        print!("{}", out);
        io::stdout().flush().unwrap();
    }
}

// ── Action submenu ────────────────────────────────────────────────────────────
/// Returns true if the user wants to quit entirely.
fn show_action(app: &mut App, w: usize, h: usize) -> bool {
    let targets = app.action_targets();
    if targets.is_empty() { return false; }

    let total: u64 = targets.iter().map(|(_, s)| s).sum();
    let desc = if targets.len() == 1 {
        targets[0].0.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| targets[0].0.display().to_string())
    } else {
        format!("{} items  ({})", targets.len(), human(total))
    };

    loop {
        app.render(w, h);
        print!("\x1b[{}H\x1b[2K", h.saturating_sub(1));
        print!(
            "  {} {}   {}  {}  {} ",
            dim!("▸"), yellow!(&desc),
            bold!("[r] Finder"),
            bold!("[d] Delete"),
            bold!("[b] Back"),
        );
        io::stdout().flush().unwrap();

        match read_key() {
            Key::Char('r') | Key::Char('R') => {
                for (p, _) in &targets { open_in_finder(p); }
                app.selected.clear();
                return false;
            }
            Key::Char('d') | Key::Char('D') => {
                print!("\x1b[{}H\x1b[2K", h.saturating_sub(1));
                print!("  {} Type {} to confirm: ", yellow!("⚠"), bold!("DELETE"));
                io::stdout().flush().unwrap();
                let confirm = collect_chars();
                if confirm == "DELETE" {
                    let paths: Vec<PathBuf> = targets.iter().map(|(p, _)| p.clone()).collect();
                    let mut errs: Vec<String> = vec![];
                    for p in &paths {
                        if let Err(e) = delete_item(p) { errs.push(format!("{}: {e}", p.display())); }
                    }
                    if !errs.is_empty() {
                        print!("\x1b[{}H\x1b[2K  {} {}", h.saturating_sub(1), red!("Error:"), errs.join("; "));
                        io::stdout().flush().unwrap();
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                    app.remove_paths(&paths);
                } else if !confirm.is_empty() {
                    print!("\x1b[{}H\x1b[2K  {}", h.saturating_sub(1), dim!("Cancelled."));
                    io::stdout().flush().unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(700));
                }
                return false;
            }
            Key::Char('b') | Key::Char('B') | Key::Esc => return false,
            Key::Char('q') => return true,
            _ => {}
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────
fn main() {
    let cli = Cli::parse();

    if !cli.path.exists() {
        eprintln!("{} Path not found: {}", red!("✗"), cli.path.display());
        std::process::exit(1);
    }

    eprintln!(
        "{} Scanning {} (depth {})…",
        cyan!("⟳"), cli.path.display(),
        if cli.depth == 0 { "unlimited".to_string() } else { cli.depth.to_string() }
    );

    let mut items = scan(&cli.path, cli.depth);
    if items.is_empty() { eprintln!("No items found."); return; }
    items.truncate(cli.top);

    print!("\x1b[?25l"); // hide cursor
    let _raw = RawMode::enable();
    let mut app = App::new(cli.path, items);

    'main: loop {
        let (w, h) = term_size();
        app.ensure_cursor_visible(h);
        app.render(w, h);

        let key = read_key();

        // ── Input mode (comma-list multi-select) ──────────────────────────────
        if app.input.is_some() {
            match key {
                Key::Enter     => app.apply_input_select(),
                Key::Esc       => app.input = None,
                Key::Backspace => { app.input.as_mut().unwrap().pop(); }
                Key::Char(c) if c.is_ascii_digit() || c == ',' || c == ' ' => {
                    app.input.as_mut().unwrap().push(c);
                }
                _ => {}
            }
            continue;
        }

        // ── Browse mode ───────────────────────────────────────────────────────
        match key {
            Key::Up => {
                if app.cursor > 0 { app.cursor -= 1; }
            }
            Key::Down => {
                if app.cursor + 1 < app.entries.len() { app.cursor += 1; }
            }
            Key::Space => {
                let i = app.cursor;
                if app.entries[i].is_dir && !app.entries[i].expanded {
                    let (_, hh) = term_size();
                    print!("\x1b[{}H\x1b[2K  {} Scanning folder…", hh.saturating_sub(1), cyan!("⟳"));
                    io::stdout().flush().unwrap();
                }
                app.toggle_expand(i);
            }
            Key::Char('s') | Key::Char('S') => {
                let path = app.entries[app.cursor].path.clone();
                if app.selected.contains(&path) {
                    app.selected.remove(&path);
                } else {
                    app.selected.insert(path);
                }
            }
            // Typing a digit starts comma-list input mode
            Key::Char(c) if c.is_ascii_digit() => {
                app.input = Some(c.to_string());
            }
            Key::Enter => {
                if show_action(&mut app, w, h) { break 'main; }
            }
            Key::Char('q') | Key::Esc => break 'main,
            _ => {}
        }
    }

    print!("\x1b[H\x1b[2J\x1b[?25h"); // clear + restore cursor
    println!("{}", dim!("Bye!"));
}
