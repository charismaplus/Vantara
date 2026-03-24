# Vantara

[한국어 README](./README.md)

> This project was created through vibe coding by a planner, not by a professional programmer.

Vantara is a project-first desktop terminal workspace for AI-assisted development.

Instead of treating terminals as a flat pile of tabs, Vantara structures work around projects, sessions, tabs, and panes so that multiple codebases stay organized while using CLI tools like Claude Code and Codex.

## Overview

- Project sidebar with per-project session trees
- Session workspaces with top tabs and split panes
- Built-in launchers for `Claude Code`, `Claude Unsafe`, `Codex`, `Codex Full Auto`, and `Terminal`
- tmux shim integration for AI CLI split/window workflows
- Clipboard, drag-and-drop, and status-panel UX for developer workflows

## Workspace Model

- `Project`
  Top-level codebase container
- `Session`
  Persistent work thread inside a project
- `Tab`
  Top tab inside a session
- `Pane`
  Split terminal region inside a tab

## Tech Stack

- Tauri 2
- React 19
- Vite
- TypeScript
- Rust
- SQLite
- `portable-pty`
- `xterm.js`

The current terminal renderer is `xterm.js`.

## Platform

- Windows-first
- Node.js required
- Rust toolchain required

Optional CLI tools:

- `claude`
- `codex`

Git Bash is recommended on Windows for more reliable tmux-style child pane flows.

## Quick Start

Install dependencies:

```powershell
npm install
```

Run the web UI only:

```powershell
npm run dev
```

Run the desktop app in development:

```powershell
npm run tauri:dev
```

Build the frontend:

```powershell
npm run build
```

Build the desktop app:

```powershell
npm run tauri:build
```

## Repository Layout

- `apps/ui`
  React-based application UI
- `apps/desktop/src-tauri`
  Tauri + Rust native runtime
- `packages/contracts`
  Shared frontend/backend contracts
- `devhub_src`
  Historical reference material

## Status

Vantara is already usable as a Windows desktop workspace terminal, but it is still under active development.

Current focus areas:

- project/session ergonomics
- pane and tab UX
- tmux shim compatibility
- PTY reliability
- rendering and performance improvements

## Notes

- Internal planning and experimental docs are intentionally excluded from Git commits in this repository.
- The active product code lives under `apps/` and `packages/`.
