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
        let _ = register_as_link_handler();
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
                let _ = remember(&browser.appid);
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

    fn register_as_link_handler() -> std::io::Result<()> {
        let (Ok(executable), Some(home)) = (std::env::current_exe(), std::env::var("HOME").ok())
        else {
            return Ok(());
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
        if std::fs::read_to_string(&path).ok().as_deref() == Some(entry.as_str()) {
            return Ok(());
        }
        std::fs::create_dir_all(&directory)?;
        std::fs::write(path, entry)?;
        std::process::Command::new("update-desktop-database")
            .arg(&directory)
            .spawn()?;
        Ok(())
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

    fn remember(appid: &str) -> std::io::Result<()> {
        let directory = config_directory();
        let temporary = directory.join("browser.tmp");
        std::fs::create_dir_all(&directory)?;
        std::fs::write(&temporary, appid)?;
        std::fs::rename(temporary, directory.join("browser"))
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{AUTHOR_LINE, NO_BROWSER_SEEN_YET};
    use std::cell::RefCell;
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconFromResourceEx, DefWindowProcW, EVENT_SYSTEM_FOREGROUND, LR_DEFAULTCOLOR,
        WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    };
    use winsafe::{self as w, co};

    const TRAY_ICON: &[u8] = include_bytes!("../icons/tray-44.icon");
    const OUR_PROGRAM_ID: &str = "Overbrowsered.Url";
    const REMEMBERED_BROWSER_KEY: &str = "Software\\Overbrowsered";
    const WM_TRAY_ICON: u32 = 0x8000 + 1;
    const QUIT_MENU_ITEM: u16 = 1;

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

        fn foreground_changed(&self, window: &w::HWND) -> Option<()> {
            let executable = executable_path_of_window(window)?;
            let browser = self
                .installed
                .iter()
                .find(|browser| browser.executable_path == executable)?;
            let mut remembered = self.remembered.borrow_mut();
            if remembered.as_ref().map(|b| &b.program_id) == Some(&browser.program_id) {
                return None;
            }
            remember(&browser.program_id).ok()?;
            *remembered = Some(browser.clone());
            Some(())
        }
    }

    fn show_menu(window: &w::HWND) -> w::SysResult<()> {
        let browser_line = OVERBROWSERED.with(|overbrowsered| {
            format!(
                "Most recently used browser: {}",
                overbrowsered
                    .remembered
                    .borrow()
                    .as_ref()
                    .map_or(NO_BROWSER_SEEN_YET, |browser| &browser.display_name)
            )
        });
            let mut menu = w::HMENU::CreatePopupMenu()?;
            let unclickable = co::MF::STRING | co::MF::DISABLED;
            menu.AppendMenu(unclickable, w::IdMenu::None, w::BmpPtrStr::from_str(AUTHOR_LINE))?;
            menu.AppendMenu(co::MF::SEPARATOR, w::IdMenu::None, w::BmpPtrStr::None)?;
            menu.AppendMenu(unclickable, w::IdMenu::None, w::BmpPtrStr::from_str(&browser_line))?;
            menu.AppendMenu(co::MF::SEPARATOR, w::IdMenu::None, w::BmpPtrStr::None)?;
            menu.AppendMenu(
                co::MF::STRING,
                w::IdMenu::Id(QUIT_MENU_ITEM),
                w::BmpPtrStr::from_str("Quit"),
            )?;

            let cursor = w::GetCursorPos()?;
            window.SetForegroundWindow();
            let chosen = menu.TrackPopupMenu(co::TPM::RETURNCMD, cursor, window)?;
            menu.DestroyMenu()?;
            if chosen == Some(QUIT_MENU_ITEM as i32) {
                w::PostQuitMessage(0);
            }
            Ok(())
    }

    pub fn watch_for_browsers_and_serve_menu() {
        let _ = register_as_link_handler();
        OVERBROWSERED.with(|_| ());
        let window = create_window().expect("the message window can be created");

        let mut icon = tray_icon(&window);
        unsafe {
            Shell_NotifyIconW(NIM_ADD, &icon);
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                std::ptr::null_mut(),
                Some(foreground_window_changed_on_our_thread),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
        }

        let mut message = w::MSG::default();
        while w::GetMessage(&mut message, None, 0, 0).unwrap_or(false) {
            unsafe { w::DispatchMessage(&message) };
        }
        unsafe { Shell_NotifyIconW(NIM_DELETE, &mut icon) };
    }

    fn create_window() -> w::SysResult<w::HWND> {
        let instance = w::HINSTANCE::GetModuleHandle(None)?;
        let mut class_name = w::WString::from_str("Overbrowsered");
        let mut class = w::WNDCLASSEX::default();
        class.lpfnWndProc = Some(window_proc);
        class.hInstance = unsafe { instance.raw_copy() };
        class.set_lpszClassName(Some(&mut class_name));
        let atom = unsafe { w::RegisterClassEx(&class)? };

        let message_only_parent = unsafe { w::HWND::from_ptr(-3isize as *mut std::ffi::c_void) };
        unsafe {
            w::HWND::CreateWindowEx(
                co::WS_EX::NoValue,
                w::AtomStr::Atom(atom),
                None,
                co::WS::NoValue,
                w::POINT::default(),
                w::SIZE::default(),
                Some(&message_only_parent),
                w::IdMenu::None,
                &instance,
                None,
            )
        }
    }

    fn tray_icon(window: &w::HWND) -> NOTIFYICONDATAW {
        let mut icon: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        icon.hWnd = window.ptr();
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
        for (slot, character) in icon
            .szTip
            .iter_mut()
            .zip("Overbrowsered".encode_utf16().chain([0]))
        {
            *slot = character;
        }
        icon
    }

    extern "system" fn window_proc(
        window: w::HWND,
        message: co::WM,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        if message.raw() == WM_TRAY_ICON {
            let mouse = unsafe { co::WM::from_raw(lparam as u32) };
            if mouse == co::WM::LBUTTONUP || mouse == co::WM::RBUTTONUP {
                let _ = show_menu(&window);
            }
            return 0;
        }
        if message == co::WM::DESTROY {
            w::PostQuitMessage(0);
            return 0;
        }
        unsafe { DefWindowProcW(window.ptr(), message.raw(), wparam, lparam) }
    }

    unsafe extern "system" fn foreground_window_changed_on_our_thread(
        _hook: HWINEVENTHOOK,
        _event: u32,
        window: *mut std::ffi::c_void,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        let window = unsafe { w::HWND::from_ptr(window) };
        OVERBROWSERED.with(|overbrowsered| overbrowsered.foreground_changed(&window));
    }

    fn executable_path_of_window(window: &w::HWND) -> Option<String> {
        let (_thread, process) = window.GetWindowThreadProcessId();
        let handle =
            w::HPROCESS::OpenProcess(co::PROCESS::QUERY_LIMITED_INFORMATION, false, process).ok()?;
        let path = handle
            .QueryFullProcessImageName(co::PROCESS_NAME::WIN32)
            .ok()?;
        Some(path.to_lowercase())
    }

    fn string_value(key: &w::HKEY, sub_key: Option<&str>, value: Option<&str>) -> Option<String> {
        match key.RegGetValue(sub_key, value, co::RRF::RT_REG_SZ).ok()? {
            w::RegistryValue::Sz(text) => Some(text),
            _ => None,
        }
    }

    fn installed_browsers() -> Vec<Browser> {
        let mut browsers = Vec::new();
        for root in [&w::HKEY::LOCAL_MACHINE, &w::HKEY::CURRENT_USER] {
            let Ok(registered) = root.RegOpenKeyEx(
                Some("SOFTWARE\\RegisteredApplications"),
                co::REG_OPTION::default(),
                co::KEY::READ,
            ) else {
                continue;
            };
            let Ok(values) = registered.RegEnumValue() else {
                continue;
            };
            for registered_name in values.flatten().map(|(name, _)| name) {
                let Some(capabilities) = string_value(&registered, None, Some(&registered_name))
                else {
                    continue;
                };
                let Some(program_id) = string_value(
                    root,
                    Some(&format!("{capabilities}\\URLAssociations")),
                    Some("http"),
                ) else {
                    continue;
                };
                if program_id == OUR_PROGRAM_ID {
                    continue;
                }
                let Some(executable_path) = command_executable(&program_id) else {
                    continue;
                };
                browsers.push(Browser {
                    display_name: string_value(root, Some(&capabilities), Some("ApplicationName"))
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
        let command = string_value(
            &w::HKEY::CLASSES_ROOT,
            Some(&format!("{program_id}\\shell\\open\\command")),
            None,
        )?;
        let command = command.trim();
        Some(
            match command.strip_prefix('"') {
                Some(quoted) => quoted.split('"').next()?,
                None => command.split_whitespace().next()?,
            }
            .to_owned(),
        )
    }

    pub fn launch_argv(program_id: &str) -> Option<Vec<String>> {
        Some(vec![command_executable(program_id)?])
    }

    fn write_key(sub_key: &str, values: &[(Option<&str>, &str)]) -> w::SysResult<()> {
        let (key, _) = w::HKEY::CURRENT_USER.RegCreateKeyEx(
            sub_key,
            None,
            co::REG_OPTION::NON_VOLATILE,
            co::KEY::WRITE,
            None,
        )?;
        for (name, text) in values {
            key.RegSetValueEx(*name, w::RegistryValue::Sz(text.to_string()))?;
        }
        Ok(())
    }

    fn register_as_link_handler() -> w::SysResult<()> {
        let Ok(executable) = std::env::current_exe() else {
            return Ok(());
        };
        let capabilities = format!("{REMEMBERED_BROWSER_KEY}\\Capabilities");
        write_key(
            &format!("Software\\Classes\\{OUR_PROGRAM_ID}"),
            &[(None, "Overbrowsered URL Handler")],
        )?;
        write_key(
            &format!("Software\\Classes\\{OUR_PROGRAM_ID}\\shell\\open\\command"),
            &[(None, &format!("\"{}\" \"%1\"", executable.display()))],
        )?;
        write_key(
            &capabilities,
            &[
                (Some("ApplicationName"), "Overbrowsered"),
                (
                    Some("ApplicationDescription"),
                    "Opens links in your most recently used browser",
                ),
            ],
        )?;
        write_key(
            &format!("{capabilities}\\URLAssociations"),
            &[(Some("http"), OUR_PROGRAM_ID), (Some("https"), OUR_PROGRAM_ID)],
        )?;
        write_key(
            "Software\\RegisteredApplications",
            &[(Some("Overbrowsered"), &capabilities)],
        )
    }

    pub fn remembered_browser() -> Option<String> {
        string_value(&w::HKEY::CURRENT_USER, Some(REMEMBERED_BROWSER_KEY), None)
    }

    fn remember(program_id: &str) -> w::SysResult<()> {
        write_key(REMEMBERED_BROWSER_KEY, &[(None, program_id)])
    }
}
