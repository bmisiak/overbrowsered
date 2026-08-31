#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

const AUTHOR_LINE: &str = "Overbrowsered by @ibmisiak";
const APP_DESCRIPTION: &str = "Opens links in your most recently used browser";
const SET_DEFAULT_PROMPT: &str =
    "⚠️ For Overbrowsered to work, click here to set it as the default \"browser\".";
const NO_BROWSER_ADVICE: &str = "Overbrowsered could not find a browser to open this link.\n\n\
     Focus any browser window once so it can learn which one you use, \
     then try the link again.";

fn most_recent_browser_line(display_name: Option<&str>) -> String {
    format!("Most recently used browser: {}", display_name.unwrap_or("none detected yet"))
}

fn default_handler_line(we_are_default: bool, handler_name: Option<&str>) -> String {
    if we_are_default {
        "Default http handler: me 👌".to_owned()
    } else {
        format!("Default http handler: {} ☹️", handler_name.unwrap_or("not me"))
    }
}

fn main() {
    let links: Vec<String> = std::env::args().skip(1).collect();
    let outcome = if links.is_empty() { platform::run() } else { platform::open(&links) };
    if let Err(error) = outcome {
        platform::report(&error);
    }
}
