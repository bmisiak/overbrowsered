use crate::{
    APP_DESCRIPTION, AUTHOR_LINE, NO_BROWSER_ADVICE, SET_DEFAULT_PROMPT, default_handler_line,
    most_recent_browser_line,
};
use anyhow::{Context, Result, anyhow, bail};
use std::ffi::{OsString, c_void};
use std::{cell::RefCell, env, io, path::Path, ptr};
use windows_registry::{CLASSES_ROOT, CURRENT_USER, HSTRING, LOCAL_MACHINE};
use windows_sys::Win32::{
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
};
use windows_sys::w;
use winsafe::{co, prelude::*, *};

const OUR_PROGRAM_ID: &str = "Overbrowsered.Url";
const SETTINGS_KEY: &str = r"Software\Overbrowsered";
const MOST_RECENT_BROWSER: &str = "MostRecentBrowser";
const BROWSER_CLIENT_KEY: &str = r"Software\Clients\StartMenuInternet\Overbrowsered";
const CAPABILITIES_KEY: &str = r"Software\Clients\StartMenuInternet\Overbrowsered\Capabilities";
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
    let mut class_name = WString::from_str("Overbrowsered");
    let mut class = WNDCLASSEX::default();
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = HINSTANCE::GetModuleHandle(None)?;
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
            &class.hInstance,
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

fn installed_browsers() -> Vec<Browser> {
    let mut browsers = Vec::new();
    for root in [LOCAL_MACHINE, CURRENT_USER] {
        let Ok(registered) = root.open(r"SOFTWARE\RegisteredApplications") else {
            continue;
        };
        let Ok(applications) = registered.values() else {
            continue;
        };
        browsers.extend(applications.filter_map(|(name, capabilities)| {
            let capabilities = root.open(String::try_from(capabilities).ok()?).ok()?;
            let urls = capabilities.open("URLAssociations").ok()?;
            let program_id = urls.get_string("http").ok().filter(|id| id != OUR_PROGRAM_ID)?;
            Some(Browser {
                display_name: capabilities
                    .get_string("ApplicationName")
                    .ok()
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
    let class = CLASSES_ROOT.open(program_id).ok()?;
    let command = class.open(r"shell\open\command").ok()?.get_string("").ok()?;
    CommandLineToArgv(command.trim()).ok()?.into_iter().next()
}

fn shell_command(executable: &Path) -> HSTRING {
    let mut command = OsString::from("\"");
    command.push(executable);
    command.push("\" \"%1\"");
    command.into()
}

fn icon_location(executable: &Path) -> HSTRING {
    let mut location = OsString::from("\"");
    location.push(executable);
    location.push("\",0");
    location.into()
}

fn register_as_link_handler() -> Result<()> {
    let executable = env::current_exe()?;
    let command = shell_command(&executable);
    let icon = icon_location(&executable);

    let class = CURRENT_USER.create(r"Software\Classes")?.create(OUR_PROGRAM_ID)?;
    class.set_string("", "Overbrowsered URL Handler")?;
    class.create(r"shell\open\command")?.set_hstring("", &command)?;
    class.create("DefaultIcon")?.set_hstring("", &icon)?;

    let client = CURRENT_USER.create(BROWSER_CLIENT_KEY)?;
    client.set_string("", "Overbrowsered")?;
    client.create(r"shell\open\command")?.set_hstring("", &command)?;
    client.create("DefaultIcon")?.set_hstring("", &icon)?;

    let capabilities = client.create("Capabilities")?;
    capabilities.set_string("ApplicationName", "Overbrowsered")?;
    capabilities.set_string("ApplicationDescription", APP_DESCRIPTION)?;
    capabilities.set_hstring("ApplicationIcon", &icon)?;
    capabilities.create("Startmenu")?.set_string("StartMenuInternet", "Overbrowsered")?;

    let urls = capabilities.create("URLAssociations")?;
    urls.set_string("http", OUR_PROGRAM_ID)?;
    urls.set_string("https", OUR_PROGRAM_ID)?;

    let files = capabilities.create("FileAssociations")?;
    files.set_string(".htm", OUR_PROGRAM_ID)?;
    files.set_string(".html", OUR_PROGRAM_ID)?;

    let registered = CURRENT_USER.create(r"Software\RegisteredApplications")?;
    registered.set_string("Overbrowsered", CAPABILITIES_KEY)?;
    Ok(())
}

fn default_http_handler() -> Option<String> {
    let associations = CURRENT_USER
        .open(r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\http")
        .ok()?;
    [r"UserChoiceLatest\ProgId", "UserChoice"]
        .into_iter()
        .find_map(|choice| associations.open(choice).ok()?.get_string("ProgId").ok())
}

fn load_most_recent_browser_id() -> Option<String> {
    let settings = CURRENT_USER.open(SETTINGS_KEY).ok()?;
    settings.get_string(MOST_RECENT_BROWSER).or_else(|_| settings.get_string("")).ok()
}

fn save_most_recent_browser_id(program_id: &str) -> windows_registry::Result<()> {
    CURRENT_USER.create(SETTINGS_KEY)?.set_string(MOST_RECENT_BROWSER, program_id)
}
