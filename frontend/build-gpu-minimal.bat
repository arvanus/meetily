@echo off
REM Meetily GPU-Accelerated Build Script (Minimal) for Windows
REM
REM Builds ONLY the standalone executable, skipping the installer/bundle phase.
REM This delegates to build-gpu.bat with TAURI_NO_BUNDLE=1, which passes
REM `--no-bundle` to `tauri build`. Skipping the bundle phase avoids generating
REM the NSIS/MSI installers, the updater artifacts, AND the slow code-signing
REM step (SignTool) that otherwise hangs for a couple of minutes.
REM
REM Output: src-tauri\target\release\meetily.exe

setlocal
set "TAURI_NO_BUNDLE=1"
call "%~dp0build-gpu.bat" %*
exit /b %errorlevel%
