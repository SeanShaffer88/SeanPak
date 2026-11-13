@echo off
setlocal enabledelayedexpansion

echo ==============================================================
echo [SeanPak] Zero-Dependency Windows High-Throughput Packager Build
echo ==============================================================

where cargo >nul 2>&1
if %errorlevel% neq 0 (
    echo [ERROR] cargo is not found in PATH.
    echo Please install Rust from https://rustup.rs/
    pause
    exit /b 1
)

echo.
echo [*] Compiling SeanPak Executable (Release, Static CRT, LTO)...
cargo build --release --target x86_64-pc-windows-msvc

if %errorlevel% equ 0 (
    echo.
    echo [SUCCESS] SeanPak compiled successfully!
    echo Binary: target\x86_64-pc-windows-msvc\release\seanpak.exe
) else (
    echo.
    echo [ERROR] Compilation failed. Review diagnostics above.
)
