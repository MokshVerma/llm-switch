#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

fn main() {
    if let Err(e) = llm_switch::gui::run() {
        llm_switch::gui::fatal(&format!("{e:#}"));
    }
}
