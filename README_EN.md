<div align="center">

<img src="assets/deskfence-icon.svg" width="88" alt="DeskFence icon">

# DeskFence

**Give your Windows desktop back — auto-categorized, native-looking, unobtrusive desktop icon fences**

[中文](README.md) | English

[![Release](https://img.shields.io/github/v/release/zghehehe/DeskFence)](https://github.com/zghehehe/DeskFence/releases/latest)
[![Stars](https://img.shields.io/github/stars/zghehehe/DeskFence?style=flat&logo=github&label=Stars)](https://github.com/zghehehe/DeskFence/stargazers)
[![License](https://img.shields.io/badge/license-GPL--3.0%20%2B%20Commercial-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-blue)
![Rust](https://img.shields.io/badge/Rust-%F0%9F%A6%80-orange)

[**⬇ Download the latest version**](https://github.com/zghehehe/DeskFence/releases/latest/download/deskfence.exe) · [All releases](https://github.com/zghehehe/DeskFence/releases) · [Report an issue](https://github.com/zghehehe/DeskFence/issues)

<img src="docs/demo.svg" width="760" alt="Launch DeskFence and desktop icons sort themselves into fences (demo animation)">

*Single file · No installation · Statically linked — no runtime libraries needed*

</div>

---

A lightweight Windows desktop icon organizer built around fences. It sorts your
desktop icons into "fences" by category, while icons, labels, and context menus
all come straight from native Windows Shell resources, so it looks and feels
just like the native desktop. Implemented in Rust + Win32 + Direct2D.

## Features

- **Two categorization modes**:
  - Auto sort: Apps / Folders / Documents / Pictures / Media / Code / Archives /
    Other are filed into fences automatically, and new files land in place on
    their own
  - Custom (drag to assign): files only go into the fence you drop them into —
    categorization is entirely up to you
  - Category management panel (tray → Auto sort ▸): rename, delete, or add
    categories; each category has editable extension rules, and all file
    assignments are recomputed instantly on save
  - Manual pinning and drag-in/drag-out are supported
- **Per-fence sorting**: Most used (by frequency) / Recently modified / Name /
  Manual (drag to arrange)
- **Native look and feel**: icons come from the system image list (including
  .lnk shortcut arrows); labels are rendered with the same font as Explorer
  plus ClearType. Precise mode is pixel-identical to the native desktop, with
  a Transparent mode as a fallback for special environments
- **Three alignment & drag modes**: Auto align (fixed spacing) / Snap to icon
  grid / Free move (magnetic snapping to neighbors). When dragging fences or
  icons, an insertion line shows the landing spot in real time while everything
  else stays put; items only reflow when you release, and Esc cancels
- **Three desktop states** (switch from the tray menu; persists across restarts):
  - Normal: fences take over the desktop
  - Zen: fences and icons are both hidden — wallpaper only
  - Native: bring back Explorer's native icons
- **Borders on demand**: thin borders are shown by default so groups are easy to
  see; turn them off and fences stay completely clean — the border, title, and
  handles only appear on hover (one-click toggle in the tray)
- **Fence operations**: New / Rename / Collapse / Lock / Delete; scroll with the
  mouse wheel when content overflows, box-select to move items in batches, and
  rename icons in place (same as Explorer)
- **Chinese & English UI**: switch instantly via tray → 语言 / Language
  (System default / 中文 / English), no restart required; the UI wording
  matches the native menus exactly
- **First-run guide (one-time)**: on first launch, a welcome window explains
  that your desktop has been organized and how to restore it with one click;
  "Show fence borders" and "Start with Windows" are pre-checked (uncheck them
  in the window if you don't want them); once closed, it never appears again
- **Multi-monitor + high DPI**: adapts instantly to per-monitor DPI changes and
  follows the system's Ctrl+scroll icon size; start with Windows (tray toggle);
  the full desktop is presented about 0.3 seconds after the process launches
- **Layouts are reversible**: undo the last layout change or restore the default
  layout with one click; self-heals when Explorer restarts
- **Double-click to recover**: double-clicking the exe at any time gives you a
  healthy DeskFence — the old instance gracefully exits (restoring icons,
  saving state) and hands over to the new one; if the old instance is frozen
  and never receives the quit message, it is force-killed after 2 seconds as a
  fallback. Upgrading to a new version works the same way — just double-click

## System Requirements

- Windows 10 / 11 (x64)
- No administrator rights needed — autostart and registry access stay in the
  current user's hive (HKCU)
- No VC++ runtime required — a crt-static, statically linked single file that
  depends only on built-in system libraries
- About 1.2 MB single-file exe; resident memory roughly 60–90 MB (including wallpaper/icon caches, varies with desktop content), plus ~10 MB of data cache under %APPDATA% (safe to delete, rebuilt automatically)

## Installation

### Option 1: Direct download (recommended)

1. Download `deskfence.exe` from [Releases](https://github.com/zghehehe/DeskFence/releases/latest)
2. Double-click to run. If SmartScreen warns on first run: click "More info → Run anyway"
3. The app lives in the system tray (bottom-right icon); right-click the tray icon to manage it

Uninstall = quit from the tray, then delete the exe and the `%APPDATA%\DeskFence`
folder. If you enabled "Start with Windows", you can also remove the leftover
entry under Task Manager's "Startup apps" (harmless if left behind).

### Option 2: Build from source

Prerequisites: [Rust](https://rustup.rs) stable (MSVC toolchain; requires the
[Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/)).

```bash
git clone https://github.com/zghehehe/DeskFence.git
cd DeskFence
cargo build --release
# Output: target/release/deskfence.exe
```

The app icon and manifest resources are pre-compiled and bundled
(`resources/deskfence.res`), so the build needs no extra tools such as windres.

## Usage at a Glance

| Action | How |
|---|---|
| New fence / show & hide fences / alignment / render mode | Right-click an empty desktop area → DeskFence submenu |
| Category management (rename / delete / add / extension rules) | Right-click the tray icon → Auto sort ▸ |
| Fence sorting (most used / time / name / manual) | Fence title drop-down ▾ → Sort by |
| Zen desktop / restore native desktop / undo / restore default layout / Start with Windows | Right-click the tray icon |
| Move / resize / collapse / lock a fence | Drag the title bar, drag a corner handle, or use the fence right-click menu |
| Cancel a drag | Press Esc while dragging |
| **Self-rescue**: frozen app / misbehaving desktop | Double-click `deskfence.exe` again (the old instance yields automatically; force-killed after 2 s if frozen) |
| Restore native desktop icons only (no UI) | `deskfence.exe --restore-desktop` (also clears the old instance first) |

Data location: `%APPDATA%\DeskFence` (settings, wallpaper and icon caches) —
delete it to reset. The app never goes online and collects no data; it is fully
open source, so audit it yourself.

## Command-line Options

| Option | Description |
|---|---|
| `--restore-desktop` | Restore the native desktop icons and exit (no UI); any running old instance is cleared first — handy for self-rescue when the desktop misbehaves |

Starting with no arguments runs the app normally (the old instance is replaced
first, then the desktop is taken over).

## FAQ

- **Can I customize the categories?** Tray → Auto sort ▸ opens the category
  management panel, where you can rename, delete, or add categories and edit
  the extension rules for each one. You can also switch to "Custom (drag to
  assign)" mode, in which files only go into the fence you drag them into.
- **SmartScreen / antivirus warnings?** The app is not signed with a paid
  code-signing certificate. It is fully open source — review the code and build
  from source yourself. Antivirus tools occasionally flag statically linked
  binaries; whitelisting the exe fixes it.
- **Desktop out of control?** Double-click `deskfence.exe` again — the old
  instance exits gracefully (or is force-killed after 2 seconds if frozen), and
  the restarted instance is guaranteed to be healthy. To restore icons without
  any UI, run `deskfence.exe --restore-desktop` (which also clears the old
  instance first).

## License

Dual-licensed under **GPL-3.0 + Commercial** (see [LICENSE](LICENSE)):

- **Open-source use**: personal use, learning, modification, and redistribution
  under [GNU GPL-3.0](https://www.gnu.org/licenses/gpl-3.0.html) — commercial
  use included, but derivatives must also be open-sourced under GPL-3.0
- **Commercial licensing**: for closed-source integration, OEM customization,
  and other scenarios where the GPL obligations cannot be met, contact the
  author via [GitHub Issues](https://github.com/zghehehe/DeskFence/issues) to
  arrange a separate license
