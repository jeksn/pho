# pho

A small, local desktop utility for importing, sorting and reviewing photos
from a camera card. Built with Tauri v2 (Rust backend, vanilla JS frontend)
on macOS.

## What it does

1. **Import** — detects cameras under `/Volumes` (a volume counts as a
   camera if it has a `DCIM` folder; other drives can be pinned in Settings).
   Everything on the card is copied into a dated folder in the library
   (default `~/Pictures/Imports/YYYY-MM-DD/`, auto-suffixed on repeat imports).
   Files are hashed (SHA-256), so re-importing a card that wasn't wiped only
   copies new files — duplicates are skipped.
2. **Sort** — shots are grouped by filename stem (RAW + JPEG + video/audio
   sidecars with the same name are one shot) and sorted in shooting order.
3. **Review** — go through shots one by one: `←`/`→` navigate, `K` keeps,
   `X` discards (the whole pair is moved to the Trash). A thumbnail strip
   shows all shots with their state. Sessions persist (`.pho.json`), so you
   can resume later; the import screen has a calendar and preview cards for
   previous imports.

Previews use the JPEG sidecar when present (downscaled via `sips`), with a
`qlmanage` fallback for RAW/video — cached in `~/Library/Caches/pho`.

## Requirements

- macOS, Node.js + pnpm, Rust toolchain

## Run

```sh
pnpm install
pnpm tauri dev      # dev mode (Vite on port 1420)
```

Build a binary:

```sh
pnpm tauri build --debug     # src-tauri/target/debug/pho
```

## Settings

In the app's Settings drawer: library location (move the imports folder and
update the path — sessions and duplicate detection follow), manual import
path, and drive pinning. Settings are stored in
`~/Library/Application Support/pho/settings.json`.

## Layout

```
├── index.html / src/        # frontend (vanilla JS, no framework)
└── src-tauri/src/lib.rs     # all backend logic + unit tests (cargo test)
```
