#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::Command;

const AUTHOR_LINE: &str = "Overbrowsered by @bmisiak";
const NO_BROWSER_SEEN_YET: &str = "none detected yet";

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "windows")]
use windows as platform;

#[derive(Clone)]
struct Browser {
    id: String,
    display_name: String,
}

fn main() {
    let links: Vec<String> = std::env::args().skip(1).collect();
    if links.is_empty() {
        return platform::watch_for_browsers_and_serve_menu();
    }
    let Some(argv) = platform::remembered_browser().and_then(|id| platform::launch_argv(&id)) else {
        eprintln!("Overbrowsered has yet to see you use a browser.");
        return;
    };
    let _ = Command::new(&argv[0]).args(&argv[1..]).args(links).spawn();
}

fn most_recently_used_browser_line(browser: Option<&Browser>) -> String {
    format!(
        "Most recently used browser: {}",
        browser.map_or(NO_BROWSER_SEEN_YET, |browser| &browser.display_name)
    )
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{AUTHOR_LINE, Browser, most_recently_used_browser_line};
    use atspi::events::window::ActivateEvent;
    use atspi::{AccessibilityConnection, Event, WindowEvents};
    use freedesktop_desktop_entry::{
        DesktopEntry, default_paths, desktop_entries, get_languages_from_env,
    };
    use futures_lite::StreamExt;
    use ksni::menu::{MenuItem, StandardItem};
    use ksni::{Tray, TrayMethods};
    use std::path::{Path, PathBuf};
    use zbus::fdo::DBusProxy;
    use zbus::names::BusName;

    const TRAY_ICON_ARGB: &[u8] = include_bytes!("../icons/tray-22.argb");
    const DESKTOP_FILE: &str = "overbrowsered.desktop";

    struct Overbrowsered {
        browser: Option<Browser>,
    }

    impl Tray for Overbrowsered {
        fn id(&self) -> String {
            "overbrowsered".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![ksni::Icon {
                width: 22,
                height: 22,
                data: TRAY_ICON_ARGB.to_vec(),
            }]
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let disabled = |label: String| {
                StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into()
            };
            vec![
                disabled(AUTHOR_LINE.into()),
                MenuItem::Separator,
                disabled(most_recently_used_browser_line(self.browser.as_ref())),
                MenuItem::Separator,
                StandardItem {
                    label: "Quit".into(),
                    activate: Box::new(|_| std::process::exit(0)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    pub fn watch_for_browsers_and_serve_menu() {
        futures_lite::future::block_on(serve());
    }

    async fn serve() {
        register_as_link_handler();
        let browsers = installed_browsers();
        let remembered = remembered_browser()
            .and_then(|id| browsers.iter().find(|(_, browser)| browser.id == id))
            .map(|(_, browser)| browser.clone());
        let tray = Overbrowsered {
            browser: remembered,
        }
        .assume_sni_available(true)
        .spawn()
        .await
        .expect("the session bus is reachable");

        if watch_for_browser_activations(&browsers, &tray).await.is_none() {
            eprintln!("Overbrowsered cannot detect browsers: the accessibility bus is unavailable.");
        }
        std::future::pending::<()>().await
    }

    async fn watch_for_browser_activations(
        browsers: &[(String, Browser)],
        tray: &ksni::Handle<Overbrowsered>,
    ) -> Option<()> {
        let accessibility = AccessibilityConnection::new().await.ok()?;
        accessibility.register_event::<ActivateEvent>().await.ok()?;
        let bus = DBusProxy::new(accessibility.connection()).await.ok()?;

        let mut activations = std::pin::pin!(accessibility.event_stream());
        while let Some(event) = activations.next().await {
            let Ok(Event::Window(WindowEvents::Activate(activation))) = event else {
                continue;
            };
            let Some(name) = activation.item.name() else {
                continue;
            };
            let Ok(process) = bus
                .get_connection_unix_process_id(BusName::Unique(name.to_owned()))
                .await
            else {
                continue;
            };
            let executable_name = executable_name(process);
            let flatpak_app_id = flatpak_app_id(process);
            let Some((_, browser)) = browsers.iter().find(|(known_as, _)| {
                Some(known_as) == flatpak_app_id.as_ref() || Some(known_as) == executable_name.as_ref()
            }) else {
                continue;
            };
            let changed = tray
                .update(|tray| {
                    let changed = tray.browser.as_ref().map(|b| &b.id) != Some(&browser.id);
                    tray.browser = Some(browser.clone());
                    changed
                })
                .await;
            if changed == Some(true) {
                remember(&browser.id);
            }
        }
        Some(())
    }

    fn file_name(path: &Path) -> Option<&str> {
        path.file_name()?.to_str()
    }

    fn executable_name(process: u32) -> Option<String> {
        let path = std::fs::read_link(format!("/proc/{process}/exe")).ok()?;
        Some(file_name(&path)?.to_owned())
    }

    fn flatpak_app_id(process: u32) -> Option<String> {
        let info = std::fs::read_to_string(format!("/proc/{process}/root/.flatpak-info")).ok()?;
        Some(info.lines().find_map(|line| line.strip_prefix("name="))?.to_owned())
    }

    fn installed_browsers() -> Vec<(String, Browser)> {
        let locales = get_languages_from_env();
        desktop_entries(&locales)
            .iter()
            .filter(|entry| entry.appid != "overbrowsered")
            .filter_map(|entry| {
                if !entry.mime_type()?.contains(&"x-scheme-handler/http") {
                    return None;
                }
                let argv = exec_argv(entry.exec()?);
                let browser = Browser {
                    id: entry.appid.clone(),
                    display_name: entry
                        .name(&locales)
                        .map_or_else(|| entry.appid.clone(), |name| name.into_owned()),
                };
                let known_as = match entry.flatpak() {
                    Some(app_id) => app_id.to_owned(),
                    None => file_name(Path::new(argv.first()?))?.to_owned(),
                };
                Some((known_as, browser))
            })
            .collect()
    }

    pub fn launch_argv(appid: &str) -> Option<Vec<String>> {
        let locales = get_languages_from_env();
        let entry = default_paths()
            .find_map(|dir| DesktopEntry::from_path(dir.join(appid), Some(&locales)).ok())?;
        Some(exec_argv(entry.exec()?))
    }

    fn exec_argv(exec: &str) -> Vec<String> {
        exec.split_whitespace()
            .filter(|token| !token.starts_with('%') && !token.starts_with("@@"))
            .map(|token| token.trim_matches('"').to_owned())
            .collect()
    }

    fn register_as_link_handler() {
        let (Ok(executable), Some(home)) = (std::env::current_exe(), std::env::var("HOME").ok())
        else {
            return;
        };
        let directory = PathBuf::from(
            std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share")),
        )
        .join("applications");
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Overbrowsered\nComment=Opens links in your most recently used browser\nExec={} %u\nIcon=overbrowsered\nTerminal=false\nStartupNotify=false\nCategories=Network;WebBrowser;\nMimeType=x-scheme-handler/http;x-scheme-handler/https;\n",
            executable.display()
        );
        let path = directory.join(DESKTOP_FILE);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(entry.as_str()) {
            let _ = std::fs::create_dir_all(&directory);
            let _ = std::fs::write(path, entry);
            let _ = std::process::Command::new("update-desktop-database")
                .arg(&directory)
                .spawn();
        }
    }

    fn config_directory() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            format!("{}/.config", std::env::var("HOME").unwrap_or_default())
        });
        PathBuf::from(base).join("overbrowsered")
    }

    pub fn remembered_browser() -> Option<String> {
        let id = std::fs::read_to_string(config_directory().join("browser")).ok()?;
        Some(id.trim().to_owned())
    }

    fn remember(appid: &str) {
        let directory = config_directory();
        let temporary = directory.join("browser.tmp");
        let _ = std::fs::create_dir_all(&directory);
        if std::fs::write(&temporary, appid).is_ok() {
            let _ = std::fs::rename(temporary, directory.join("browser"));
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{AUTHOR_LINE, Browser, most_recently_used_browser_line};
    use std::cell::RefCell;
    use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIconBuilder};
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

    const TRAY_ICON_RGBA: &[u8] = include_bytes!("../icons/tray-44.rgba");
    const REMEMBERED_BROWSER_KEY: &str = "Software\\Overbrowsered";
    const OUR_PROGRAM_ID: &str = "Overbrowsered.Url";

    thread_local! {
        static BROWSER_LINE: MenuItem = MenuItem::new("", false, None);
        static INSTALLED_BROWSERS: Vec<(String, Browser)> = installed_browsers();
        static REMEMBERED_BROWSER: RefCell<Option<Browser>> = RefCell::new(
            remembered_browser().and_then(|id| {
                INSTALLED_BROWSERS.with(|browsers| {
                    browsers
                        .iter()
                        .find(|(_, browser)| browser.id == id)
                        .map(|(_, browser)| browser.clone())
                })
            }),
        );
    }

    pub fn watch_for_browsers_and_serve_menu() {
        register_as_link_handler();
        INSTALLED_BROWSERS.with(|_| ());
        REMEMBERED_BROWSER.with(|_| ());

        REMEMBERED_BROWSER.with_borrow(|browser| show_in_menu(browser.as_ref()));
        let menu = Menu::new();
        BROWSER_LINE.with(|browser_line| {
            let _ = menu.append_items(&[
                &MenuItem::new(AUTHOR_LINE, false, None),
                &PredefinedMenuItem::separator(),
                browser_line,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(Some("Quit")),
            ]);
        });

        let _tray = TrayIconBuilder::new()
            .with_icon(
                Icon::from_rgba(TRAY_ICON_RGBA.to_vec(), 44, 44).expect("the embedded icon is rgba"),
            )
            .with_tooltip("Overbrowsered")
            .with_menu(Box::new(menu))
            .build()
            .expect("the tray icon can be created");

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
    }

    fn show_in_menu(browser: Option<&Browser>) {
        BROWSER_LINE.with(|line| line.set_text(most_recently_used_browser_line(browser)));
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
        let Some(executable) = (unsafe { executable_path_of_window(window) }) else {
            return;
        };
        let executable = executable.to_lowercase();
        let Some(browser) = INSTALLED_BROWSERS.with(|browsers| {
            browsers
                .iter()
                .find(|(candidate, _)| *candidate == executable)
                .map(|(_, browser)| browser.clone())
        }) else {
            return;
        };
        REMEMBERED_BROWSER.with_borrow_mut(|remembered| {
            if remembered.as_ref().map(|b| &b.id) == Some(&browser.id) {
                return;
            }
            remember(&browser.id);
            show_in_menu(Some(&browser));
            *remembered = Some(browser);
        });
    }

    unsafe fn executable_path_of_window(window: HWND) -> Option<String> {
        let mut process = 0;
        unsafe { GetWindowThreadProcessId(window, &mut process) };
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process) };
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let queried =
            unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) };
        unsafe { CloseHandle(handle) };
        (queried != 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
    }

    fn installed_browsers() -> Vec<(String, Browser)> {
        let mut browsers = Vec::new();
        for predefined in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let root = RegKey::predef(predefined);
            let Ok(registered) = root.open_subkey("SOFTWARE\\RegisteredApplications") else {
                continue;
            };
            for (registered_name, _) in registered.enum_values().flatten() {
                let Some(capabilities) = registered
                    .get_value::<String, _>(&registered_name)
                    .ok()
                    .and_then(|path| root.open_subkey(path).ok())
                else {
                    continue;
                };
                let Some(program_id) = capabilities
                    .open_subkey("URLAssociations")
                    .ok()
                    .and_then(|associations| associations.get_value::<String, _>("http").ok())
                else {
                    continue;
                };
                if program_id == OUR_PROGRAM_ID {
                    continue;
                }
                let Some(argv) = launch_argv(&program_id) else {
                    continue;
                };
                browsers.push((
                    argv[0].to_lowercase(),
                    Browser {
                        id: program_id,
                        display_name: capabilities
                            .get_value::<String, _>("ApplicationName")
                            .ok()
                            .filter(|name| !name.starts_with('@'))
                            .unwrap_or(registered_name),
                    },
                ));
            }
        }
        browsers
    }

    pub fn launch_argv(program_id: &str) -> Option<Vec<String>> {
        let command = RegKey::predef(HKEY_CLASSES_ROOT)
            .open_subkey(format!("{program_id}\\shell\\open\\command"))
            .ok()?
            .get_value::<String, _>("")
            .ok()?;
        let command = command.trim();
        let executable = match command.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next()?,
            None => command.split_whitespace().next()?,
        };
        Some(vec![executable.to_owned()])
    }

    fn register_as_link_handler() {
        let Ok(executable) = std::env::current_exe() else {
            return;
        };
        let user = RegKey::predef(HKEY_CURRENT_USER);
        let register = || -> std::io::Result<()> {
            user.create_subkey(format!("Software\\Classes\\{OUR_PROGRAM_ID}"))?
                .0
                .set_value("", &"Overbrowsered URL Handler".to_owned())?;
            user.create_subkey(format!("Software\\Classes\\{OUR_PROGRAM_ID}\\shell\\open\\command"))?
                .0
                .set_value("", &format!("\"{}\" \"%1\"", executable.display()))?;
            let capabilities = user
                .create_subkey(format!("{REMEMBERED_BROWSER_KEY}\\Capabilities"))?
                .0;
            capabilities.set_value("ApplicationName", &"Overbrowsered".to_owned())?;
            capabilities.set_value(
                "ApplicationDescription",
                &"Opens links in your most recently used browser".to_owned(),
            )?;
            let associations = user
                .create_subkey(format!("{REMEMBERED_BROWSER_KEY}\\Capabilities\\URLAssociations"))?
                .0;
            associations.set_value("http", &OUR_PROGRAM_ID.to_owned())?;
            associations.set_value("https", &OUR_PROGRAM_ID.to_owned())?;
            user.create_subkey("Software\\RegisteredApplications")?
                .0
                .set_value("Overbrowsered", &format!("{REMEMBERED_BROWSER_KEY}\\Capabilities"))?;
            Ok(())
        };
        let _ = register();
    }

    pub fn remembered_browser() -> Option<String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(REMEMBERED_BROWSER_KEY)
            .ok()?
            .get_value("")
            .ok()
    }

    fn remember(program_id: &str) {
        if let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(REMEMBERED_BROWSER_KEY)
        {
            let _ = key.set_value("", &program_id.to_owned());
        }
    }
}
