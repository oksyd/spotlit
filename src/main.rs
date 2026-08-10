#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    spotlit::run_from_env()
}
