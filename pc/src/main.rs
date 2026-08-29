#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::Command;

const AUTHOR_LINE: &str = "Overbrowsered by @bmisiak";
const NO_BROWSER_SEEN_YET: &str = "none detected yet";

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "windows")]
use windows as platform;

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

#[cfg(target_os = "linux")]
mod linux {
    use super::{AUTHOR_LINE, NO_BROWSER_SEEN_YET};
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

    #[derive(Clone, PartialEq)]
    enum RecognisedBy {
        Executable(String),
        FlatpakApp(String),
    }

    #[derive(Clone)]
    struct Browser {
        appid: String,
        display_name: String,
        recognised_by: RecognisedBy,
    }

    impl Browser {
        fn is_running_as(&self, executable: Option<&str>, flatpak_app: Option<&str>) -> bool {
            match &self.recognised_by {
                RecognisedBy::Executable(name) => Some(name.as_str()) == executable,
                RecognisedBy::FlatpakApp(app) => Some(app.as_str()) == flatpak_app,
            }
        }
    }

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
            let unclickable = |label: String| {
                StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into()
            };
            vec![
                unclickable(AUTHOR_LINE.into()),
                MenuItem::Separator,
                unclickable(format!(
                    "Most recently used browser: {}",
                    self.browser
                        .as_ref()
                        .map_or(NO_BROWSER_SEEN_YET, |browser| &browser.display_name)
                )),
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
        let installed = installed_browsers();
        let tray = Overbrowsered {
            browser: remembered_browser()
                .and_then(|appid| installed.iter().find(|browser| browser.appid == appid))
                .cloned(),
        }
        .assume_sni_available(true)
        .spawn()
        .await
        .expect("the session bus is reachable");

        if watch_for_activations(&installed, &tray).await.is_none() {
            eprintln!("Overbrowsered cannot detect browsers: the accessibility bus is unavailable.");
        }
        std::future::pending::<()>().await
    }

    async fn watch_for_activations(
        installed: &[Browser],
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
            let executable = executable_name(process);
            let flatpak_app = flatpak_app_id(process);
            let Some(browser) = installed.iter().find(|browser| {
                browser.is_running_as(executable.as_deref(), flatpak_app.as_deref())
            }) else {
                continue;
            };
            let newly_activated = tray
                .update(|tray| {
                    let newly_activated = tray.browser.as_ref().map(|b| &b.appid) != Some(&browser.appid);
                    tray.browser = Some(browser.clone());
                    newly_activated
                })
                .await;
            if newly_activated == Some(true) {
                remember(&browser.appid);
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

    fn installed_browsers() -> Vec<Browser> {
        let locales = get_languages_from_env();
        desktop_entries(&locales)
            .iter()
            .filter(|entry| entry.appid != "overbrowsered")
            .filter_map(|entry| {
                if !entry.mime_type()?.contains(&"x-scheme-handler/http") {
                    return None;
                }
                Some(Browser {
                    appid: entry.appid.clone(),
                    display_name: entry
                        .name(&locales)
                        .map_or_else(|| entry.appid.clone(), |name| name.into_owned()),
                    recognised_by: match entry.flatpak() {
                        Some(app) => RecognisedBy::FlatpakApp(app.to_owned()),
                        None => RecognisedBy::Executable(
                            file_name(Path::new(exec_argv(entry.exec()?).first()?))?.to_owned(),
                        ),
                    },
                })
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
        let appid = std::fs::read_to_string(config_directory().join("browser")).ok()?;
        Some(appid.trim().to_owned())
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
    use super::{AUTHOR_LINE, NO_BROWSER_SEEN_YET};
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows_sys::Win32::UI::Shell::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    use winreg::RegKey;
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    const TRAY_ICON: &[u8] = include_bytes!("../icons/tray-44.icon");
    const OUR_PROGRAM_ID: &str = "Overbrowsered.Url";
    const REMEMBERED_BROWSER_KEY: &str = "Software\\Overbrowsered";
    const WM_TRAY_ICON: u32 = WM_APP + 1;
    const WM_BROWSER_ACTIVATED: u32 = WM_APP + 2;
    const QUIT_MENU_ITEM: usize = 1;

    static OVERBROWSERED_WINDOW: AtomicIsize = AtomicIsize::new(0);

    thread_local! {
        static OVERBROWSERED: Overbrowsered = Overbrowsered::new();
    }

    #[derive(Clone)]
    struct Browser {
        program_id: String,
        display_name: String,
        executable_path: String,
    }

    struct Overbrowsered {
        installed: Vec<Browser>,
        remembered: RefCell<Option<Browser>>,
    }

    impl Overbrowsered {
        fn new() -> Self {
            let installed = installed_browsers();
            Self {
                remembered: RefCell::new(
                    remembered_browser()
                        .and_then(|id| installed.iter().find(|b| b.program_id == id))
                        .cloned(),
                ),
                installed,
            }
        }

        fn foreground_changed(&self, window: HWND) {
            let Some(executable) = (unsafe { executable_path_of_window(window) }) else {
                return;
            };
            let executable = executable.to_lowercase();
            let Some(browser) = self
                .installed
                .iter()
                .find(|browser| browser.executable_path == executable)
            else {
                return;
            };
            let mut remembered = self.remembered.borrow_mut();
            if remembered.as_ref().map(|b| &b.program_id) == Some(&browser.program_id) {
                return;
            }
            remember(&browser.program_id);
            *remembered = Some(browser.clone());
        }

        fn show_menu(&self, window: HWND) {
            let browser_line = format!(
                "Most recently used browser: {}",
                self.remembered
                    .borrow()
                    .as_ref()
                    .map_or(NO_BROWSER_SEEN_YET, |browser| &browser.display_name)
            );
            unsafe {
                let menu = CreatePopupMenu();
                AppendMenuW(menu, MF_STRING | MF_DISABLED, 0, utf16(AUTHOR_LINE).as_ptr());
                AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                AppendMenuW(menu, MF_STRING | MF_DISABLED, 0, utf16(&browser_line).as_ptr());
                AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                AppendMenuW(menu, MF_STRING, QUIT_MENU_ITEM, utf16("Quit").as_ptr());

                let mut cursor = POINT { x: 0, y: 0 };
                GetCursorPos(&mut cursor);
                SetForegroundWindow(window);
                let chosen = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD,
                    cursor.x,
                    cursor.y,
                    0,
                    window,
                    std::ptr::null(),
                );
                DestroyMenu(menu);
                if chosen as usize == QUIT_MENU_ITEM {
                    PostQuitMessage(0);
                }
            }
        }
    }

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    pub fn watch_for_browsers_and_serve_menu() {
        register_as_link_handler();
        OVERBROWSERED.with(|_| ());

        unsafe {
            let window = create_window();
            OVERBROWSERED_WINDOW.store(window as isize, Ordering::Relaxed);
            let mut icon = tray_icon(window);
            Shell_NotifyIconW(NIM_ADD, &icon);

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
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            Shell_NotifyIconW(NIM_DELETE, &mut icon);
        }
    }

    unsafe fn create_window() -> HWND {
        let class_name = utf16("Overbrowsered");
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        unsafe { RegisterClassW(&class) };
        unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        }
    }

    unsafe fn tray_icon(window: HWND) -> NOTIFYICONDATAW {
        let mut icon: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        icon.hWnd = window;
        icon.uID = 1;
        icon.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        icon.uCallbackMessage = WM_TRAY_ICON;
        icon.hIcon = unsafe {
            CreateIconFromResourceEx(
                TRAY_ICON.as_ptr(),
                TRAY_ICON.len() as u32,
                1,
                0x0003_0000,
                0,
                0,
                LR_DEFAULTCOLOR,
            )
        };
        for (slot, character) in icon.szTip.iter_mut().zip(utf16("Overbrowsered")) {
            *slot = character;
        }
        icon
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_BROWSER_ACTIVATED => {
                OVERBROWSERED.with(|overbrowsered| overbrowsered.foreground_changed(wparam as HWND));
                0
            }
            WM_TRAY_ICON if matches!(lparam as u32, WM_LBUTTONUP | WM_RBUTTONUP) => {
                OVERBROWSERED.with(|overbrowsered| overbrowsered.show_menu(window));
                0
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                0
            }
            _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
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
        let ours = OVERBROWSERED_WINDOW.load(Ordering::Relaxed) as HWND;
        if !ours.is_null() {
            unsafe { PostMessageW(ours, WM_BROWSER_ACTIVATED, window as WPARAM, 0) };
        }
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

    fn installed_browsers() -> Vec<Browser> {
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
                let Some(executable_path) = command_executable(&program_id) else {
                    continue;
                };
                browsers.push(Browser {
                    display_name: capabilities
                        .get_value::<String, _>("ApplicationName")
                        .ok()
                        .filter(|name| !name.starts_with('@'))
                        .unwrap_or(registered_name),
                    executable_path: executable_path.to_lowercase(),
                    program_id,
                });
            }
        }
        browsers
    }

    fn command_executable(program_id: &str) -> Option<String> {
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
        Some(executable.to_owned())
    }

    pub fn launch_argv(program_id: &str) -> Option<Vec<String>> {
        Some(vec![command_executable(program_id)?])
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
            user.create_subkey(format!(
                "Software\\Classes\\{OUR_PROGRAM_ID}\\shell\\open\\command"
            ))?
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
                .create_subkey(format!(
                    "{REMEMBERED_BROWSER_KEY}\\Capabilities\\URLAssociations"
                ))?
                .0;
            associations.set_value("http", &OUR_PROGRAM_ID.to_owned())?;
            associations.set_value("https", &OUR_PROGRAM_ID.to_owned())?;
            user.create_subkey("Software\\RegisteredApplications")?
                .0
                .set_value(
                    "Overbrowsered",
                    &format!("{REMEMBERED_BROWSER_KEY}\\Capabilities"),
                )?;
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
