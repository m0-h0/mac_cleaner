# mac_cleaner

A small macOS CLI tool to find and remove the largest files/folders under a path

## Build

```bash
make build
```

## Example

```bash
make run /System/Volumes/Data
make run ~/Downloads
```

## Controls

| Key | Action |
|-----|--------|
| `↑` / `↓` | Move cursor |
| `Space` | Expand / collapse folder |
| `s` | Toggle-select item under cursor |
| `1,2,…` | Type numbers (comma-separated) + `Enter` for multi-select |
| `PgUp` / `<` | Previous page |
| `PgDn` / `>` | Next page |
| `Enter` | Open action menu (Finder / Delete) |
| `q` / `Esc` | Quit |

## Binaries

| Binary | Source | Description |
|--------|--------|-------------|
| `mC` | `src/main.rs` | Simple numbered-list version |
| `mCp` | `src/mainP.rs` | Interactive UI (arrow keys, paging, multi-select) |
