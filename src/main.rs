use clap::Parser;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

// ── ANSI helpers ──────────────────────────────────────────────────────────────
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";

macro_rules! bold    { ($s:expr) => { format!("{BOLD}{}{RESET}", $s)   }; }
macro_rules! dim     { ($s:expr) => { format!("{DIM}{}{RESET}", $s)    }; }
macro_rules! cyan    { ($s:expr) => { format!("{CYAN}{}{RESET}", $s)   }; }
macro_rules! yellow  { ($s:expr) => { format!("{YELLOW}{}{RESET}", $s) }; }
macro_rules! green   { ($s:expr) => { format!("{GREEN}{}{RESET}", $s)  }; }
macro_rules! red     { ($s:expr) => { format!("{RED}{}{RESET}", $s)    }; }

// ── CLI ───────────────────────────────────────────────────────────────────────
#[derive(Parser)]
#[command(
    name = "mac_cleaner",
    about = "Find and remove the largest files/folders under a path"
)]
struct Cli {
    /// Root path to scan
    #[arg(default_value = "/System/Volumes/Data")]
    path: PathBuf,

    /// How many top items to list
    #[arg(short, long, default_value_t = 20)]
    top: usize,

    /// Scan depth for grouping (1 = direct children, 2 = one level deeper, 0 = unlimited)
    #[arg(short, long, default_value_t = 2)]
    depth: usize,
}

// ── Item ──────────────────────────────────────────────────────────────────────
struct Item {
    path: PathBuf,
    size: u64,
    is_dir: bool,
}

// ── Helpers ───────────────────────────────────────────────────────────────────
fn human(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut val = bytes as f64;
    let mut unit = 0;
    while val >= 1024.0 && unit + 1 < UNITS.len() {
        val /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", val, UNITS[unit])
    }
}

fn prompt(msg: &str) -> String {
    print!("{msg}");
    io::stdout().flush().unwrap();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).unwrap();
    buf.trim().to_string()
}

fn open_in_finder(path: &Path) {
    Command::new("open").arg("-R").arg(path).spawn().ok();
}

fn delete_item(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

// ── Scanner ───────────────────────────────────────────────────────────────────
fn scan(root: &Path, depth: usize) -> Vec<Item> {
    let max_depth = if depth == 0 { usize::MAX } else { depth };
    let mut sizes: HashMap<PathBuf, u64> = HashMap::new();
    let mut count: u64 = 0;

    // Walk the tree
    let _ = visit(root, root, 0, max_depth, &mut sizes, &mut count);

    eprint!("\r\x1b[2K"); // clear spinner line

    let mut items: Vec<Item> = sizes
        .into_iter()
        .map(|(path, size)| {
            let is_dir = path.is_dir();
            Item { path, size, is_dir }
        })
        .collect();

    items.sort_by(|a, b| b.size.cmp(&a.size));
    items
}

fn visit(
    root: &Path,
    dir: &Path,
    current_depth: usize,
    max_depth: usize,
    sizes: &mut HashMap<PathBuf, u64>,
    count: &mut u64,
) -> u64 {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    let mut dir_total: u64 = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        *count += 1;
        if *count % 500 == 0 {
            eprint!("\r{}  Scanned {} entries…\x1b[K", cyan!("⟳"), count);
        }

        if meta.file_type().is_symlink() {
            continue;
        }

        if meta.is_file() {
            let sz = meta.len();
            dir_total += sz;
            // Attribute to the direct child of root
            if let Some(child) = top_child(root, &path) {
                *sizes.entry(child).or_insert(0) += sz;
            }
        } else if meta.is_dir() {
            let sub_total = if current_depth + 1 < max_depth {
                visit(root, &path, current_depth + 1, max_depth, sizes, count)
            } else {
                // At max depth: still sum but don't recurse for grouping
                sum_dir(&path, count)
            };

            dir_total += sub_total;

            // At depth boundary, register this subdir as a leaf entry
            if current_depth + 1 == max_depth {
                if let Some(child) = top_child(root, &path) {
                    *sizes.entry(child).or_insert(0) += sub_total;
                } else {
                    // path IS a direct child of root
                    *sizes.entry(path.clone()).or_insert(0) += sub_total;
                }
            }
        }
    }

    dir_total
}

fn sum_dir(dir: &Path, count: &mut u64) -> u64 {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        *count += 1;
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            total += sum_dir(&entry.path(), count);
        }
    }
    total
}

/// Returns the direct child of `root` that is an ancestor of `path`, or None if path==root.
fn top_child(root: &Path, path: &Path) -> Option<PathBuf> {
    if let Ok(rel) = path.strip_prefix(root) {
        let mut comps = rel.components();
        if let Some(first) = comps.next() {
            return Some(root.join(first));
        }
    }
    None
}

// ── Main loop ─────────────────────────────────────────────────────────────────
fn main() {
    let cli = Cli::parse();

    if !cli.path.exists() {
        eprintln!("{} Path not found: {}", red!("✗"), cli.path.display());
        std::process::exit(1);
    }

    println!(
        "{} Scanning {} (depth {})…",
        cyan!("⟳"),
        bold!(cli.path.display()),
        if cli.depth == 0 { "unlimited".to_string() } else { cli.depth.to_string() }
    );

    let mut items = scan(&cli.path, cli.depth);

    if items.is_empty() {
        println!("No items found.");
        return;
    }

    items.truncate(cli.top);

    loop {
        println!(
            "\n{} Top {} items under {}\n",
            green!("◉"),
            items.len(),
            bold!(cli.path.display())
        );

        for (i, item) in items.iter().enumerate() {
            let icon = if item.is_dir { "📁" } else { "📄" };
            println!(
                "  {}  {}  {:>12}  {}",
                bold!(format!("{:>2}", i + 1)),
                icon,
                yellow!(human(item.size)),
                item.path.display()
            );
        }

        println!("\n  {}  Quit", dim!(" 0"));
        println!();

        let input = prompt(&format!("{}  Select item number: ", bold!("▶")));

        let idx: usize = match input.parse::<usize>() {
            Ok(0) => break,
            Ok(n) if n <= items.len() => n - 1,
            _ => {
                println!("{}", dim!("  (invalid choice, try again)"));
                continue;
            }
        };

        let item = &items[idx];
        println!(
            "\n  {} {}\n  Size: {}\n",
            if item.is_dir { "📁" } else { "📄" },
            bold!(item.path.display()),
            yellow!(human(item.size))
        );

        println!("  {}  Reveal in Finder", bold!("r"));
        println!("  {}  Delete", bold!("d"));
        println!("  {}  Back", bold!("b"));
        println!();

        let action = prompt(&format!("{}  Action [r/d/b]: ", bold!("▶")));

        match action.to_lowercase().as_str() {
            "r" => {
                open_in_finder(&item.path);
                println!("  {}", green!("Opened in Finder."));
            }
            "d" => {
                println!(
                    "\n  {} About to delete:\n  {}\n  Size: {}\n",
                    yellow!("⚠"),
                    bold!(item.path.display()),
                    yellow!(human(item.size))
                );
                let confirm = prompt(&format!(
                    "  {} Type {} to confirm, anything else to cancel: ",
                    bold!("▶"),
                    bold!("DELETE")
                ));
                if confirm == "DELETE" {
                    match delete_item(&item.path) {
                        Ok(_) => {
                            println!("  {}", green!("✓ Deleted."));
                            items.remove(idx);
                            if items.is_empty() {
                                println!("No more items.");
                                break;
                            }
                        }
                        Err(e) => eprintln!("  {} {}", red!("✗ Error:"), e),
                    }
                } else {
                    println!("  {}", dim!("Cancelled."));
                }
            }
            _ => {}
        }
    }

    println!("\n{}", dim!("Bye!"));
}