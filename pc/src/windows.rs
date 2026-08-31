use crate::{
    APP_DESCRIPTION, AUTHOR_LINE, NO_BROWSER_ADVICE, SET_DEFAULT_PROMPT, default_handler_line,
    most_recent_browser_line,
};
use anyhow::{Context, Result, anyhow, bail};
use std::{cell::RefCell, env, ffi::c_void, io, ptr};
use windows_sys::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
        System::{
            Recovery::{RESTART_NO_CRASH, RESTART_NO_HANG, RegisterApplicationRestart},
            Threading::CreateMutexW,
        },
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook},
            Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify},
            WindowsAndMessaging::{
                EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
            },
        },
    },
    w,
};
use winsafe::{co, prelude::*, *};

const OUR_PROGRAM_ID: &str = "Overbrowsered.Url";
const MOST_RECENT_BROWSER_KEY: &str = "Software\\Overbrowsered";
const BROWSER_CLIENT_KEY: &str = "Software\\Clients\\StartMenuInternet\\Overbrowsered";
const CAPABILITIES_KEY: &str = "Software\\Clients\\StartMenuInternet\\Overbrowsered\\Capabilities";
const WM_TRAY_ICON: co::WM = unsafe { co::WM::from_raw(0x8000 + 1) };
const QUIT_MENU_ITEM: u16 = 1;
const SET_DEFAULT_MENU_ITEM: u16 = 2;

thread_local! {
    static OVERBROWSERED: Overbrowsered = Overbrowsered::new();
}

pub fn report(error: &anyhow::Error) {
    let text = format!("{error:#}");
    if HWND::NULL.MessageBox(&text, "Overbrowsered", co::MB::ICONERROR).is_err() {
        eprintln!("{text}");
    }
}

pub fn open(links: &[String]) -> Result<()> {
    let _com_alive_while_launching =
        CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE)?;
    let most_recent_failure = match load_most_recent_browser_id() {
        Some(program_id) => match launch(links, &program_id) {
            Ok(()) => return Ok(()),
            Err(error) => Some(error),
        },
        None => None,
    };
    let installed = installed_browsers();
    let Some(browser) = topmost_browser(&installed)? else {
        return Err(match most_recent_failure {
            Some(cause) => cause.context(NO_BROWSER_ADVICE),
            None => anyhow!(NO_BROWSER_ADVICE),
        });
    };
    launch(links, &browser.program_id)?;
    save_most_recent_browser_id(&browser.program_id)?;
    Ok(())
}

fn launch(links: &[String], program_id: &str) -> Result<()> {
    for link in links {
        ShellExecuteEx(&SHELLEXECUTEINFO {
            file: link,
            class: Some(program_id),
            show: co::SW::SHOWNORMAL,
            ..Default::default()
        })
        .with_context(|| format!("opening {link} with {program_id}"))?;
    }
    Ok(())
}

fn topmost_browser(installed: &[Browser]) -> Result<Option<&Browser>> {
    let mut topmost = None;
    EnumWindows(|window: HWND| {
        if topmost.is_none() && window.IsWindowVisible() {
            topmost = browser_of_window(installed, &window);
        }
        true
    })?;
    Ok(topmost)
}

fn browser_of_window<'a>(installed: &'a [Browser], window: &HWND) -> Option<&'a Browser> {
    let executable = executable_path_of_window(window)?;
    installed.iter().find(|browser| browser.executable_path == executable)
}

pub fn run() -> Result<()> {
    let Some(_singleton_handle) = ensure_only_one_app_instance()? else {
        return Ok(());
    };
    register_for_autorestart().context("registering for restart")?;
    register_as_link_handler().context("registering as a browser")?;
    let window = create_window().context("creating the tray window")?;
    let icon = tray_icon(&window)?;
    Shell_NotifyIcon(co::NIM::ADD, &icon).context("adding the tray icon")?;
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            ptr::null_mut(),
            Some(foreground_window_changed),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if hook.is_null() {
        bail!("cannot watch for foreground window changes");
    }
    OVERBROWSERED.with(Overbrowsered::remember_topmost_browser)?;

    let mut message = MSG::default();
    while GetMessage(&mut message, None, 0, 0)? {
        unsafe { DispatchMessage(&message) };
    }
    Shell_NotifyIcon(co::NIM::DELETE, &icon)?;
    Ok(())
}

struct SingletonAppInstanceHandle(HANDLE);

impl Drop for SingletonAppInstanceHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn ensure_only_one_app_instance() -> Result<Option<SingletonAppInstanceHandle>> {
    let handle = unsafe { CreateMutexW(ptr::null(), 0, w!("Local\\Overbrowsered.Tray")) };
    let last_error = unsafe { GetLastError() };
    if handle.is_null() {
        return Err(io::Error::from_raw_os_error(last_error as i32))
            .context("creating tray instance mutex");
    }
    let mutex = SingletonAppInstanceHandle(handle);
    if last_error == ERROR_ALREADY_EXISTS {
        return Ok(None);
    }
    Ok(Some(mutex))
}

fn register_for_autorestart() -> Result<()> {
    let result =
        unsafe { RegisterApplicationRestart(ptr::null(), RESTART_NO_CRASH | RESTART_NO_HANG) };
    if result < 0 {
        bail!("RegisterApplicationRestart failed with HRESULT {:#010x}", result as u32);
    }
    Ok(())
}

struct Browser {
    program_id: String,
    display_name: String,
    executable_path: String,
}

struct Overbrowsered {
    installed: Vec<Browser>,
    most_recent_browser_id: RefCell<Option<String>>,
}

impl Overbrowsered {
    fn new() -> Self {
        Self {
            most_recent_browser_id: RefCell::new(load_most_recent_browser_id()),
            installed: installed_browsers(),
        }
    }

    fn foreground_changed(&self, window: &HWND) -> Option<()> {
        let browser = browser_of_window(&self.installed, window)?;
        let mut most_recent_browser_id = self.most_recent_browser_id.borrow_mut();
        if most_recent_browser_id.as_deref() == Some(browser.program_id.as_str()) {
            return None;
        }
        save_most_recent_browser_id(&browser.program_id).ok()?;
        *most_recent_browser_id = Some(browser.program_id.clone());
        Some(())
    }

    fn remember_topmost_browser(&self) -> Result<()> {
        if let Some(browser) = topmost_browser(&self.installed)? {
            save_most_recent_browser_id(&browser.program_id)?;
            *self.most_recent_browser_id.borrow_mut() = Some(browser.program_id.clone());
        }
        Ok(())
    }
}

unsafe extern "system" fn foreground_window_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    window: *mut c_void,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let window = unsafe { HWND::from_ptr(window) };
    OVERBROWSERED.with(|overbrowsered| overbrowsered.foreground_changed(&window));
}

fn create_window() -> Result<HWND> {
    let instance = HINSTANCE::GetModuleHandle(None)?;
    let mut class_name = WString::from_str("Overbrowsered");
    let mut class = WNDCLASSEX::default();
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = unsafe { instance.raw_copy() };
    class.set_lpszClassName(Some(&mut class_name));
    let atom = unsafe { RegisterClassEx(&class)? };

    Ok(unsafe {
        HWND::CreateWindowEx(
            co::WS_EX::NoValue,
            AtomStr::Atom(atom),
            None,
            co::WS::NoValue,
            POINT::default(),
            SIZE::default(),
            None,
            IdMenu::None,
            &instance,
            None,
        )?
    })
}

extern "system" fn window_proc(
    window: HWND,
    message: co::WM,
    wparam: usize,
    lparam: isize,
) -> isize {
    if message == WM_TRAY_ICON {
        let click = unsafe { co::WM::from_raw(lparam as u32) };
        if (click == co::WM::LBUTTONUP || click == co::WM::RBUTTONUP)
            && let Err(error) = show_menu(&window)
        {
            report(&error);
        }
        return 0;
    }
    if message == co::WM::DESTROY {
        PostQuitMessage(0);
        return 0;
    }
    if RegisterWindowMessage("TaskbarCreated") == Ok(message.raw()) {
        if let Err(error) = restore_tray_icon(&window) {
            report(&error);
        }
        return 0;
    }
    unsafe { window.DefWindowProc(msg::Wm { msg_id: message, wparam, lparam }) }
}

fn restore_tray_icon(window: &HWND) -> Result<()> {
    Shell_NotifyIcon(co::NIM::ADD, &tray_icon(window)?).context("re-adding the tray icon")
}

fn tray_icon(window: &HWND) -> Result<NOTIFYICONDATA> {
    let mut data = NOTIFYICONDATA::default();
    data.hWnd = unsafe { window.raw_copy() };
    data.uID = 1;
    data.uFlags = co::NIF::ICON | co::NIF::MESSAGE | co::NIF::TIP;
    data.uCallbackMessage = WM_TRAY_ICON;
    data.hIcon = HINSTANCE::GetModuleHandle(None)?.LoadIcon(IdIdiStr::Id(2))?.leak();
    data.set_szTip("Overbrowsered");
    Ok(data)
}

fn show_menu(window: &HWND) -> Result<()> {
    let (browser_line, default_line, we_are_default) = OVERBROWSERED.with(|overbrowsered| {
        let most_recent_browser_id = overbrowsered.most_recent_browser_id.borrow();
        let most_recent = most_recent_browser_id.as_deref().map(|id| {
            overbrowsered
                .installed
                .iter()
                .find(|browser| browser.program_id == id)
                .map_or(id, |browser| browser.display_name.as_str())
        });
        let browser_line = most_recent_browser_line(most_recent);
        let default_handler = default_http_handler();
        let we_are_default = default_handler.as_deref() == Some(OUR_PROGRAM_ID);
        let handler_name = default_handler.as_ref().and_then(|id| {
            overbrowsered.installed.iter().find(|browser| &browser.program_id == id)
        });
        let default_line = default_handler_line(
            we_are_default,
            handler_name.map(|browser| browser.display_name.as_str()),
        );
        (browser_line, default_line, we_are_default)
    });
    let mut menu = HMENU::CreatePopupMenu()?;
    let unclickable = co::MF::STRING | co::MF::DISABLED;
    menu.AppendMenu(unclickable, IdMenu::None, BmpPtrStr::from_str(AUTHOR_LINE))?;
    menu.AppendMenu(co::MF::SEPARATOR, IdMenu::None, BmpPtrStr::None)?;
    menu.AppendMenu(unclickable, IdMenu::None, BmpPtrStr::from_str(&browser_line))?;
    menu.AppendMenu(unclickable, IdMenu::None, BmpPtrStr::from_str(&default_line))?;
    if !we_are_default {
        menu.AppendMenu(
            co::MF::STRING,
            IdMenu::Id(SET_DEFAULT_MENU_ITEM),
            BmpPtrStr::from_str(SET_DEFAULT_PROMPT),
        )?;
    }
    menu.AppendMenu(co::MF::SEPARATOR, IdMenu::None, BmpPtrStr::None)?;
    menu.AppendMenu(co::MF::STRING, IdMenu::Id(QUIT_MENU_ITEM), BmpPtrStr::from_str("Quit"))?;

    let cursor = GetCursorPos()?;
    window.SetForegroundWindow();
    let chosen = menu.TrackPopupMenu(co::TPM::RETURNCMD, cursor, window)?;
    menu.DestroyMenu()?;
    if chosen == Some(QUIT_MENU_ITEM as i32) {
        PostQuitMessage(0);
    }
    if chosen == Some(SET_DEFAULT_MENU_ITEM as i32) {
        open_default_apps_settings()?;
    }
    Ok(())
}

fn open_default_apps_settings() -> Result<()> {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, ptr::null(), ptr::null()) };
    ShellExecuteEx(&SHELLEXECUTEINFO {
        file: "ms-settings:defaultapps?registeredAppUser=Overbrowsered",
        show: co::SW::SHOWNORMAL,
        ..Default::default()
    })
    .context("opening the default apps settings")?;
    Ok(())
}

fn executable_path_of_window(window: &HWND) -> Option<String> {
    let (_thread, process) = window.GetWindowThreadProcessId();
    let handle =
        HPROCESS::OpenProcess(co::PROCESS::QUERY_LIMITED_INFORMATION, false, process).ok()?;
    let path = handle.QueryFullProcessImageName(co::PROCESS_NAME::WIN32).ok()?;
    Some(path.to_lowercase())
}

fn string_value(key: &HKEY, sub_key: Option<&str>, value: Option<&str>) -> Option<String> {
    match key.RegGetValue(sub_key, value, co::RRF::RT_REG_SZ).ok()? {
        RegistryValue::Sz(text) => Some(text),
        _ => None,
    }
}

fn installed_browsers() -> Vec<Browser> {
    let mut browsers = Vec::new();
    for root in [&HKEY::LOCAL_MACHINE, &HKEY::CURRENT_USER] {
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
            let browser_capabilities_key = string_value(&registered, None, Some(&name))?;
            let program_id = string_value(
                root,
                Some(&format!("{browser_capabilities_key}\\URLAssociations")),
                Some("http"),
            )
            .filter(|id| *id != OUR_PROGRAM_ID)?;
            Some(Browser {
                display_name: string_value(
                    root,
                    Some(&browser_capabilities_key),
                    Some("ApplicationName"),
                )
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
        &HKEY::CLASSES_ROOT,
        Some(&format!("{program_id}\\shell\\open\\command")),
        None,
    )?;
    CommandLineToArgv(command.trim()).ok()?.into_iter().next()
}

fn write_key(sub_key: &str, name: Option<&str>, value: &str) -> SysResult<()> {
    if string_value(&HKEY::CURRENT_USER, Some(sub_key), name).as_deref() == Some(value) {
        return Ok(());
    }
    let (key, _) = HKEY::CURRENT_USER.RegCreateKeyEx(
        sub_key,
        None,
        co::REG_OPTION::NON_VOLATILE,
        co::KEY::WRITE,
        None,
    )?;
    key.RegSetValueEx(name, RegistryValue::Sz(value.to_owned()))?;
    Ok(())
}

fn register_as_link_handler() -> Result<()> {
    let executable = env::current_exe()?;
    let command = format!("\"{}\" \"%1\"", executable.display());
    let icon = format!("\"{}\",0", executable.display());
    let class = format!("Software\\Classes\\{OUR_PROGRAM_ID}");
    let urls = format!("{CAPABILITIES_KEY}\\URLAssociations");
    let files = format!("{CAPABILITIES_KEY}\\FileAssociations");
    for (sub_key, name, value) in [
        (class.clone(), None, "Overbrowsered URL Handler"),
        (format!("{class}\\shell\\open\\command"), None, command.as_str()),
        (format!("{class}\\DefaultIcon"), None, icon.as_str()),
        (BROWSER_CLIENT_KEY.to_owned(), None, "Overbrowsered"),
        (format!("{BROWSER_CLIENT_KEY}\\shell\\open\\command"), None, command.as_str()),
        (format!("{BROWSER_CLIENT_KEY}\\DefaultIcon"), None, icon.as_str()),
        (CAPABILITIES_KEY.to_owned(), Some("ApplicationName"), "Overbrowsered"),
        (CAPABILITIES_KEY.to_owned(), Some("ApplicationDescription"), APP_DESCRIPTION),
        (CAPABILITIES_KEY.to_owned(), Some("ApplicationIcon"), icon.as_str()),
        (format!("{CAPABILITIES_KEY}\\Startmenu"), Some("StartMenuInternet"), "Overbrowsered"),
        (urls.clone(), Some("http"), OUR_PROGRAM_ID),
        (urls, Some("https"), OUR_PROGRAM_ID),
        (files.clone(), Some(".htm"), OUR_PROGRAM_ID),
        (files, Some(".html"), OUR_PROGRAM_ID),
        ("Software\\RegisteredApplications".to_owned(), Some("Overbrowsered"), CAPABILITIES_KEY),
    ] {
        write_key(&sub_key, name, value)?;
    }
    Ok(())
}

fn default_http_handler() -> Option<String> {
    let associations = "Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\http";
    string_value(
        &HKEY::CURRENT_USER,
        Some(&format!("{associations}\\UserChoiceLatest\\ProgId")),
        Some("ProgId"),
    )
    .or_else(|| {
        string_value(
            &HKEY::CURRENT_USER,
            Some(&format!("{associations}\\UserChoice")),
            Some("ProgId"),
        )
    })
}

fn load_most_recent_browser_id() -> Option<String> {
    string_value(&HKEY::CURRENT_USER, Some(MOST_RECENT_BROWSER_KEY), None)
}

fn save_most_recent_browser_id(program_id: &str) -> SysResult<()> {
    write_key(MOST_RECENT_BROWSER_KEY, None, program_id)
}
