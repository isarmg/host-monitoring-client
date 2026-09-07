#![cfg_attr(windows, windows_subsystem = "windows")]

#[path = "../windows/tray/mod.rs"]
mod tray;

fn main() {
    tray::entry();
}
