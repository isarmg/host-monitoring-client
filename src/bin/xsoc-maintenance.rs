#![cfg_attr(windows, windows_subsystem = "windows")]

#[path = "../windows/maintenance/mod.rs"]
mod maintenance;

fn main() {
    maintenance::entry();
}
