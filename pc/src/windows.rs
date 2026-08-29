use crate::{AUTHOR_LINE, NO_BROWSER_SEEN_YET};
use anyhow::{Context, Result, bail};
use std::cell::RefCell;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, EVENT_SYSTEM_FOREGROUND, LR_DEFAULTCOLOR, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};
use winsafe::prelude::Handle;
use winsafe::{self as w, co};

const TRAY_ICON: &[u8] = include_bytes!("../icons/tray-44.icon");
const ICON_FORMAT_VERSION: u32 = 0x0003_0000;
const HWND_MESSAGE: isize = -3;
const OUR_PROGRAM_ID: &str = "Overbrowsered.Url";
const REMEMBERED_BROWSER_KEY: &str = "Software\\Overbrowsered";
const WM_TRAY_ICON: co::WM = unsafe { co::WM::from_raw(0x8000 + 1) };
const QUIT_MENU_ITEM: u16 = 1;

thread_local! {
    static OVERBROWSERED: Overbrowsered = Overbrowsered::new();
}

pub fn report(error: &anyhow::Error) {
    let _ = w::HWND::NULL.MessageBox(&format!("{error:#}"), "Overbrowsered", co::MB::ICONERROR);
}

pub fn open(links: &[String]) -> Result<()> {
    let program_id =
        remembered_browser().context("Overbrowsered has yet to see you use a browser")?;
    let _com = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE)?;
    for link in links {
        w::ShellExecuteEx(&w::SHELLEXECUTEINFO {
            file: link,
            class: Some(&program_id),
            show: co::SW::SHOWNORMAL,
            ..Default::default()
        })
        .with_context(|| format!("opening {link} with {program_id}"))?;
    }
    Ok(())
}

pub fn run() -> Result<()> {
    register_as_link_handler().context("registering as a browser")?;
    let window = create_window().context("creating the tray window")?;
    let icon = tray_icon(&window)?;
    w::Shell_NotifyIcon(co::NIM::ADD, &icon).context("adding the tray icon")?;
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            std::ptr::null_mut(),
            Some(foreground_window_changed),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if hook.is_null() {
        bail!("cannot watch for foreground window changes");
    }

    let mut message = w::MSG::default();
    while w::GetMessage(&mut message, None, 0, 0)? {
        unsafe { w::DispatchMessage(&message) };
    }
    w::Shell_NotifyIcon(co::NIM::DELETE, &icon)?;
    Ok(())
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

unsafe extern "system" fn foreground_window_changed(
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

fn create_window() -> Result<w::HWND> {
    let instance = w::HINSTANCE::GetModuleHandle(None)?;
    let mut class_name = w::WString::from_str("Overbrowsered");
    let mut class = w::WNDCLASSEX::default();
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = unsafe { instance.raw_copy() };
    class.set_lpszClassName(Some(&mut class_name));
    let atom = unsafe { w::RegisterClassEx(&class)? };

    let message_only_parent = unsafe { w::HWND::from_ptr(HWND_MESSAGE as *mut std::ffi::c_void) };
    Ok(unsafe {
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
        )?
    })
}

extern "system" fn window_proc(
    window: w::HWND,
    message: co::WM,
    wparam: usize,
    lparam: isize,
) -> isize {
    if message == WM_TRAY_ICON {
        let click = unsafe { co::WM::from_raw(lparam as u32) };
        if click == co::WM::LBUTTONUP || click == co::WM::RBUTTONUP {
            let _ = show_menu(&window);
        }
        return 0;
    }
    if message == co::WM::DESTROY {
        w::PostQuitMessage(0);
        return 0;
    }
    unsafe {
        window.DefWindowProc(w::msg::Wm {
            msg_id: message,
            wparam,
            lparam,
        })
    }
}

fn tray_icon(window: &w::HWND) -> Result<w::NOTIFYICONDATA> {
    let icon = unsafe {
        CreateIconFromResourceEx(
            TRAY_ICON.as_ptr(),
            TRAY_ICON.len() as u32,
            1,
            ICON_FORMAT_VERSION,
            0,
            0,
            LR_DEFAULTCOLOR,
        )
    };
    if icon.is_null() {
        bail!("cannot decode the tray icon");
    }
    let mut data = w::NOTIFYICONDATA::default();
    data.hWnd = unsafe { window.raw_copy() };
    data.uID = 1;
    data.uFlags = co::NIF::ICON | co::NIF::MESSAGE | co::NIF::TIP;
    data.uCallbackMessage = WM_TRAY_ICON;
    data.hIcon = unsafe { w::HICON::from_ptr(icon) };
    data.set_szTip("Overbrowsered");
    Ok(data)
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
    menu.AppendMenu(
        unclickable,
        w::IdMenu::None,
        w::BmpPtrStr::from_str(AUTHOR_LINE),
    )?;
    menu.AppendMenu(co::MF::SEPARATOR, w::IdMenu::None, w::BmpPtrStr::None)?;
    menu.AppendMenu(
        unclickable,
        w::IdMenu::None,
        w::BmpPtrStr::from_str(&browser_line),
    )?;
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
        let Ok(names) = registered.RegEnumValue() else {
            continue;
        };
        browsers.extend(names.flatten().filter_map(|(name, _)| {
            let capabilities = string_value(&registered, None, Some(&name))?;
            let program_id = string_value(
                root,
                Some(&format!("{capabilities}\\URLAssociations")),
                Some("http"),
            )
            .filter(|id| *id != OUR_PROGRAM_ID)?;
            Some(Browser {
                display_name: string_value(root, Some(&capabilities), Some("ApplicationName"))
                    .filter(|name| !name.starts_with('@'))
                    .unwrap_or(name),
                executable_path: command_executable(&program_id)?.to_lowercase(),
                program_id,
            })
        }));
    }
    browsers
}

fn command_executable(program_id: &str) -> Option<String> {
    let command = string_value(
        &w::HKEY::CLASSES_ROOT,
        Some(&format!("{program_id}\\shell\\open\\command")),
        None,
    )?;
    w::CommandLineToArgv(command.trim())
        .ok()?
        .into_iter()
        .next()
}

fn write_key(sub_key: &str, name: Option<&str>, value: &str) -> w::SysResult<()> {
    let (key, _) = w::HKEY::CURRENT_USER.RegCreateKeyEx(
        sub_key,
        None,
        co::REG_OPTION::NON_VOLATILE,
        co::KEY::WRITE,
        None,
    )?;
    key.RegSetValueEx(name, w::RegistryValue::Sz(value.to_owned()))?;
    Ok(())
}

fn register_as_link_handler() -> Result<()> {
    let command = format!("\"{}\" \"%1\"", std::env::current_exe()?.display());
    let capabilities = format!("{REMEMBERED_BROWSER_KEY}\\Capabilities");
    for (sub_key, name, value) in [
        (
            format!("Software\\Classes\\{OUR_PROGRAM_ID}"),
            None,
            "Overbrowsered URL Handler",
        ),
        (
            format!("Software\\Classes\\{OUR_PROGRAM_ID}\\shell\\open\\command"),
            None,
            command.as_str(),
        ),
        (
            capabilities.clone(),
            Some("ApplicationName"),
            "Overbrowsered",
        ),
        (
            capabilities.clone(),
            Some("ApplicationDescription"),
            "Opens links in your most recently used browser",
        ),
        (
            format!("{capabilities}\\URLAssociations"),
            Some("http"),
            OUR_PROGRAM_ID,
        ),
        (
            format!("{capabilities}\\URLAssociations"),
            Some("https"),
            OUR_PROGRAM_ID,
        ),
        (
            "Software\\RegisteredApplications".to_owned(),
            Some("Overbrowsered"),
            &capabilities,
        ),
    ] {
        write_key(&sub_key, name, value)?;
    }
    Ok(())
}

fn remembered_browser() -> Option<String> {
    string_value(&w::HKEY::CURRENT_USER, Some(REMEMBERED_BROWSER_KEY), None)
}

fn remember(program_id: &str) -> w::SysResult<()> {
    write_key(REMEMBERED_BROWSER_KEY, None, program_id)
}
