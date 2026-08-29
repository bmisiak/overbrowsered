#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

const AUTHOR_LINE: &str = "Overbrowsered by @bmisiak";
const NO_BROWSER_SEEN_YET: &str = "none detected yet";

fn main() {
    let links: Vec<String> = std::env::args().skip(1).collect();
    let outcome = if links.is_empty() {
        platform::run()
    } else {
        platform::open(&links)
    };
    if let Err(error) = outcome {
        platform::report(&error);
    }
}
