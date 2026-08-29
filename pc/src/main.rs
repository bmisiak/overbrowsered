use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::mpsc::Receiver;
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const CONFIGURATION_NAME: &str = "overbrowsered";
const AUTHOR_LINE: &str = "Overbrowsered by @bmisiak";
const NO_BROWSER_SEEN_YET: &str = "none detected yet";

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Browser {
    display_name: String,
    launch_argv: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedState {
    most_recently_used_browser: Option<Browser>,
}

fn main() {
    let links: Vec<String> = std::env::args().skip(1).collect();
    if links.is_empty() {
        show_menu_while_watching_for_browsers();
    } else {
        open_links_in_most_recently_used_browser(&links);
    }
}

fn load_most_recently_used_browser() -> Option<Browser> {
    confy::load::<PersistedState>(CONFIGURATION_NAME, None)
        .ok()?
        .most_recently_used_browser
}

fn persist_most_recently_used_browser(browser: &Browser) {
    let _ = confy::store(
        CONFIGURATION_NAME,
        None,
        PersistedState {
            most_recently_used_browser: Some(browser.clone()),
        },
    );
}

fn open_links_in_most_recently_used_browser(links: &[String]) {
    let Some(browser) = load_most_recently_used_browser() else {
        eprintln!("Overbrowsered has yet to see you use a browser.");
        return;
    };
    let _ = Command::new(&browser.launch_argv[0])
        .args(&browser.launch_argv[1..])
        .args(links)
        .spawn();
}

fn most_recently_used_browser_line(browser: Option<&Browser>) -> String {
    format!(
        "Most recently used browser: {}",
        browser.map_or(NO_BROWSER_SEEN_YET, |browser| &browser.display_name)
    )
}

fn embedded_menu_bar_icon() -> Icon {
    let pixels = image::load_from_memory(include_bytes!(
        "../../Overbrowsered/Assets.xcassets/StatusBarButtonImage.imageset/overbrowsered44.png"
    ))
    .expect("the embedded icon decodes")
    .into_rgba8();
    let (width, height) = pixels.dimensions();
    Icon::from_rgba(pixels.into_raw(), width, height).expect("the embedded icon is valid rgba")
}

fn build_tray(most_recently_used_browser_item: &MenuItem) -> TrayIcon {
    let menu = Menu::new();
    let _ = menu.append_items(&[
        &MenuItem::new(AUTHOR_LINE, false, None),
        &PredefinedMenuItem::separator(),
        most_recently_used_browser_item,
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(Some("Quit")),
    ]);
    TrayIconBuilder::new()
        .with_icon(embedded_menu_bar_icon())
        .with_tooltip("Overbrowsered")
        .with_menu(Box::new(menu))
        .build()
        .expect("the tray icon can be created")
}

fn remember_newly_activated_browsers(
    activations: &Receiver<Browser>,
    menu_item: &MenuItem,
    remembered: &mut Option<Browser>,
) {
    for browser in activations.try_iter() {
        if remembered.as_ref() == Some(&browser) {
            continue;
        }
        menu_item.set_text(most_recently_used_browser_line(Some(&browser)));
        persist_most_recently_used_browser(&browser);
        *remembered = Some(browser);
    }
}

#[cfg(target_os = "linux")]
fn show_menu_while_watching_for_browsers() {
    let (report_activation, activations) = std::sync::mpsc::channel();
    linux::watch_for_activated_browsers(report_activation);

    gtk::init().expect("gtk starts");
    let mut remembered = load_most_recently_used_browser();
    let menu_item = MenuItem::new(most_recently_used_browser_line(remembered.as_ref()), false, None);
    let _tray = build_tray(&menu_item);

    gtk::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        remember_newly_activated_browsers(&activations, &menu_item, &mut remembered);
        gtk::glib::ControlFlow::Continue
    });
    gtk::main();
}

#[cfg(target_os = "windows")]
fn show_menu_while_watching_for_browsers() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, SetTimer, TranslateMessage,
    };

    let (report_activation, activations) = std::sync::mpsc::channel();
    windows::watch_for_activated_browsers(report_activation);

    let mut remembered = load_most_recently_used_browser();
    let menu_item = MenuItem::new(most_recently_used_browser_line(remembered.as_ref()), false, None);
    let _tray = build_tray(&menu_item);

    unsafe {
        SetTimer(std::ptr::null_mut(), 0, 500, None);
        let mut message = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) != 0 {
            remember_newly_activated_browsers(&activations, &menu_item, &mut remembered);
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Browser;
    use atspi::AccessibilityConnection;
    use atspi::events::window::ActivateEvent;
    use atspi::{Event, WindowEvents};
    use freedesktop_desktop_entry::{DesktopEntry, desktop_entries, get_languages_from_env};
    use futures_lite::StreamExt;
    use std::path::Path;
    use std::sync::mpsc::Sender;
    use zbus::fdo::DBusProxy;
    use zbus::names::BusName;

    pub fn watch_for_activated_browsers(report_activation: Sender<Browser>) {
        std::thread::spawn(move || {
            futures_lite::future::block_on(report_activated_browsers(report_activation))
        });
    }

    async fn report_activated_browsers(report_activation: Sender<Browser>) {
        let browsers = installed_browsers_by_executable_name();
        let accessibility = AccessibilityConnection::new()
            .await
            .expect("the accessibility bus is reachable");
        accessibility
            .register_event::<ActivateEvent>()
            .await
            .expect("window activation events can be subscribed to");
        let bus = DBusProxy::new(accessibility.connection())
            .await
            .expect("the session bus is reachable");

        let mut activations = std::pin::pin!(accessibility.event_stream());
        while let Some(event) = activations.next().await {
            let Ok(Event::Window(WindowEvents::Activate(activation))) = event else {
                continue;
            };
            let Some(unique_name) = activation.item.name() else {
                continue;
            };
            let Ok(process_id) = bus
                .get_connection_unix_process_id(BusName::Unique(unique_name.to_owned()))
                .await
            else {
                continue;
            };
            let Ok(executable) = std::fs::read_link(format!("/proc/{process_id}/exe")) else {
                continue;
            };
            let Some(executable_name) = executable.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some((_, browser)) = browsers
                .iter()
                .find(|(candidate, _)| candidate == executable_name)
            {
                let _ = report_activation.send(browser.clone());
            }
        }
    }

    fn installed_browsers_by_executable_name() -> Vec<(String, Browser)> {
        let locales = get_languages_from_env();
        desktop_entries(&locales)
            .iter()
            .filter(|entry| handles_web_links(entry))
            .filter_map(|entry| {
                let launch_argv = launch_argv_from_exec(entry.exec()?);
                let executable_name = Path::new(launch_argv.first()?)
                    .file_name()?
                    .to_str()?
                    .to_owned();
                Some((
                    executable_name,
                    Browser {
                        display_name: entry
                            .name(&locales)
                            .map_or_else(|| entry.appid.clone(), |name| name.into_owned()),
                        launch_argv,
                    },
                ))
            })
            .collect()
    }

    fn handles_web_links(entry: &DesktopEntry) -> bool {
        entry.mime_type().is_some_and(|mime_types| {
            mime_types.contains(&"x-scheme-handler/http")
                || mime_types.contains(&"x-scheme-handler/https")
        })
    }

    fn launch_argv_from_exec(exec: &str) -> Vec<String> {
        exec.split_whitespace()
            .filter(|token| !token.starts_with('%'))
            .map(|token| token.trim_matches('"').to_owned())
            .collect()
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::Browser;
    use std::sync::OnceLock;
    use std::sync::mpsc::Sender;
    use windows_sys::Win32::Foundation::{CloseHandle, HWND};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetMessageW, GetWindowThreadProcessId,
        TranslateMessage, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    };
    use winreg::RegKey;
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    static BROWSERS_BY_EXECUTABLE_PATH: OnceLock<Vec<(String, Browser)>> = OnceLock::new();
    static ACTIVATION_REPORTER: OnceLock<Sender<Browser>> = OnceLock::new();

    pub fn watch_for_activated_browsers(report_activation: Sender<Browser>) {
        std::thread::spawn(move || {
            let _ = BROWSERS_BY_EXECUTABLE_PATH.set(installed_browsers_by_executable_path());
            let _ = ACTIVATION_REPORTER.set(report_activation);
            unsafe {
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    std::ptr::null_mut(),
                    Some(on_foreground_window_changed),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );
                let mut message = std::mem::zeroed();
                while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        });
    }

    unsafe extern "system" fn on_foreground_window_changed(
        _hook: HWINEVENTHOOK,
        _event: u32,
        window: HWND,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        let Some(executable_path) = unsafe { executable_path_of_window(window) } else {
            return;
        };
        let Some(browsers) = BROWSERS_BY_EXECUTABLE_PATH.get() else {
            return;
        };
        let Some((_, browser)) = browsers
            .iter()
            .find(|(candidate, _)| *candidate == executable_path.to_lowercase())
        else {
            return;
        };
        if let Some(reporter) = ACTIVATION_REPORTER.get() {
            let _ = reporter.send(browser.clone());
        }
    }

    unsafe fn executable_path_of_window(window: HWND) -> Option<String> {
        let mut process_id = 0;
        unsafe { GetWindowThreadProcessId(window, &mut process_id) };
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let queried = unsafe {
            QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length)
        };
        unsafe { CloseHandle(process) };
        (queried != 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
    }

    fn installed_browsers_by_executable_path() -> Vec<(String, Browser)> {
        let mut browsers = Vec::new();
        for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let root = RegKey::predef(root);
            let Ok(registered_applications) = root.open_subkey("SOFTWARE\\RegisteredApplications")
            else {
                continue;
            };
            for application_name in registered_applications.enum_values().flatten().map(|(name, _)| name) {
                let Ok(capabilities_path) =
                    registered_applications.get_value::<String, _>(&application_name)
                else {
                    continue;
                };
                let Some(executable_path) = web_link_handler_executable(&root, &capabilities_path)
                else {
                    continue;
                };
                browsers.push((
                    executable_path.to_lowercase(),
                    Browser {
                        display_name: application_name,
                        launch_argv: vec![executable_path],
                    },
                ));
            }
        }
        browsers
    }

    fn web_link_handler_executable(root: &RegKey, capabilities_path: &str) -> Option<String> {
        let program_id = root
            .open_subkey(format!("{capabilities_path}\\URLAssociations"))
            .ok()?
            .get_value::<String, _>("http")
            .ok()?;
        let command = RegKey::predef(HKEY_CLASSES_ROOT)
            .open_subkey(format!("{program_id}\\shell\\open\\command"))
            .ok()?
            .get_value::<String, _>("")
            .ok()?;
        executable_from_shell_command(&command)
    }

    fn executable_from_shell_command(command: &str) -> Option<String> {
        let command = command.trim();
        if let Some(quoted) = command.strip_prefix('"') {
            return quoted.split('"').next().map(str::to_owned);
        }
        command.split_whitespace().next().map(str::to_owned)
    }
}
