use crate::{
    APP_DESCRIPTION, AUTHOR_LINE, NO_BROWSER_ADVICE, SET_DEFAULT_PROMPT, default_handler_line,
    most_recent_browser_line,
};
use anyhow::{Context, Result, bail};
use std::ffi::{OsString, c_void};
use std::os::windows::io::{HandleOrNull, OwnedHandle};
use std::{cell::RefCell, env, path::Path, ptr};
use windows_registry::{CLASSES_ROOT, CURRENT_USER, HSTRING, LOCAL_MACHINE};
use windows_sys::Win32::{
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
use winsafe::co::{COINIT, ERROR, MB, MF, NIF, NIM, PROCESS, PROCESS_NAME, SW, TPM, WM, WS, WS_EX};
use winsafe::{prelude::*, *};

const OUR_PROGRAM_ID: &str = "Overbrowsered.Url";
const SETTINGS_KEY: &str = r"Software\Overbrowsered";
const MOST_RECENT_BROWSER: &str = "MostRecentBrowser";
const WM_TRAY_ICON: WM = WM::APP;
const QUIT_MENU_ITEM: u16 = 1;
const SET_DEFAULT_MENU_ITEM: u16 = 2;

thread_local! {
    static STATE: State = State::new();
}

pub fn report(error: &anyhow::Error) {
    let text = format!("{error:#}");
    if HWND::NULL.MessageBox(&text, "Overbrowsered", MB::ICONERROR).is_err() {
        eprintln!("{text}");
    }
}

pub fn open(links: &[String]) -> Result<()> {
    let _com_guard = CoInitializeEx(COINIT::APARTMENTTHREADED | COINIT::DISABLE_OLE1DDE)?;
    let installed = installed_browsers();
    let topmost = topmost_browser(&installed).map(|browser| browser.program_id.clone());
    let saved = load_most_recent_browser_id().context(NO_BROWSER_ADVICE);
    let mut failures = Vec::new();
    for candidate in [topmost, saved] {
        match candidate.and_then(|id| launch(links, &id).map(|()| id)) {
            Ok(id) => return save_most_recent_browser_id(&id).context("remembering the browser"),
            Err(failure) => failures.push(format!("{failure:#}")),
        }
    }
    failures.dedup();
    bail!("Overbrowsered could not open the link.\n\n{}", failures.join("\n\n"))
}

fn launch(links: &[String], program_id: &str) -> Result<()> {
    for link in links {
        ShellExecuteEx(&SHELLEXECUTEINFO {
            file: link,
            class: Some(program_id),
            show: SW::SHOWNORMAL,
            ..Default::default()
        })
        .with_context(|| format!("opening {link} with {program_id}"))?;
    }
    Ok(())
}

fn topmost_browser(installed: &[Browser]) -> Result<&Browser> {
    let mut topmost = None;
    EnumWindows(|window: HWND| {
        if topmost.is_none() && window.IsWindowVisible() {
            topmost = browser_of_window(installed, &window);
        }
        true
    })?;
    topmost.context("no browser window is open")
}

fn browser_of_window<'a>(installed: &'a [Browser], window: &HWND) -> Option<&'a Browser> {
    let executable = executable_path_of_window(window).ok()?;
    installed.iter().find(|browser| browser.executable_path == executable)
}

pub fn run() -> Result<()> {
    let Some(_singleton_handle) = ensure_only_one_app_instance()? else {
        return Ok(());
    };
    register_for_autorestart().context("registering for restart")?;
    register_as_link_handler().context("registering as a browser")?;
    let window = create_window().context("creating the tray window")?;
    let icon = tray_icon(window)?;
    Shell_NotifyIcon(NIM::ADD, &icon).context("adding the tray icon")?;
    // SAFETY: The static callback has the required ABI; OUTOFCONTEXT requires a null module.
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
    STATE.with(State::remember_topmost_browser)?;

    let mut message = MSG::default();
    while GetMessage(&mut message, None, 0, 0)? {
        // SAFETY: GetMessage initialized this message for dispatch.
        unsafe { DispatchMessage(&message) };
    }
    Shell_NotifyIcon(NIM::DELETE, &icon)?;
    Ok(())
}

fn ensure_only_one_app_instance() -> Result<Option<OwnedHandle>> {
    // SAFETY: CreateMutexW returns null or an owned, CloseHandle-compatible handle.
    let mutex = unsafe {
        HandleOrNull::from_raw_handle(CreateMutexW(ptr::null(), 0, w!("Local\\Overbrowsered.Tray")))
    };
    let last_error = GetLastError();
    let mutex = OwnedHandle::try_from(mutex)
        .map_err(|_| last_error)
        .context("creating tray instance mutex")?;
    Ok((last_error != ERROR::ALREADY_EXISTS).then_some(mutex))
}

fn register_for_autorestart() -> Result<()> {
    // SAFETY: A null command line and these documented flags are valid.
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

struct State {
    installed_browsers: Vec<Browser>,
    most_recent_browser_id: RefCell<Option<String>>,
}

impl State {
    fn new() -> Self {
        Self {
            most_recent_browser_id: RefCell::new(load_most_recent_browser_id()),
            installed_browsers: installed_browsers(),
        }
    }

    fn foreground_changed(&self, window: &HWND) -> Option<()> {
        let browser = browser_of_window(&self.installed_browsers, window)?;
        let mut most_recent_browser_id = self.most_recent_browser_id.borrow_mut();
        if most_recent_browser_id.as_deref() == Some(browser.program_id.as_str()) {
            return None;
        }
        save_most_recent_browser_id(&browser.program_id).ok()?;
        *most_recent_browser_id = Some(browser.program_id.clone());
        Some(())
    }

    fn remember_topmost_browser(&self) -> Result<()> {
        if let Ok(browser) = topmost_browser(&self.installed_browsers) {
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
    // SAFETY: EVENT_SYSTEM_FOREGROUND supplies the borrowed foreground HWND.
    let window = unsafe { HWND::from_ptr(window) };
    STATE.with(|state| state.foreground_changed(&window));
}

fn create_window() -> Result<HWND> {
    let mut class_name = WString::from_str("Overbrowsered");
    let mut class = WNDCLASSEX::default();
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = HINSTANCE::GetModuleHandle(None)?;
    class.set_lpszClassName(Some(&mut class_name));
    SetLastError(ERROR::SUCCESS);

    // SAFETY: The class name outlives registration, the procedure is static, and atom/instance match.
    let window = unsafe {
        let atom = RegisterClassEx(&class)?;
        HWND::CreateWindowEx(
            WS_EX::NoValue,
            AtomStr::Atom(atom),
            None,
            WS::NoValue,
            POINT::default(),
            SIZE::default(),
            None,
            IdMenu::None,
            &class.hInstance,
            None,
        )?
    };
    Ok(window)
}

extern "system" fn window_proc(window: HWND, message: WM, wparam: usize, lparam: isize) -> isize {
    if message == WM_TRAY_ICON {
        let click = lparam as u32;
        if (click == WM::LBUTTONUP.raw() || click == WM::RBUTTONUP.raw())
            && let Err(error) = show_menu(&window)
        {
            report(&error);
        }
        return 0;
    }
    if message == WM::DESTROY {
        PostQuitMessage(0);
        return 0;
    }
    if RegisterWindowMessage("TaskbarCreated") == Ok(message.raw()) {
        if let Err(error) = restore_tray_icon(window) {
            report(&error);
        }
        return 0;
    }
    // SAFETY: Windows supplied these parameters unchanged to this window procedure.
    unsafe { window.DefWindowProc(msg::Wm { msg_id: message, wparam, lparam }) }
}

fn restore_tray_icon(window: HWND) -> Result<()> {
    Shell_NotifyIcon(NIM::ADD, &tray_icon(window)?).context("re-adding the tray icon")
}

fn tray_icon(window: HWND) -> Result<NOTIFYICONDATA> {
    let mut data = NOTIFYICONDATA::default();
    data.hWnd = window;
    data.uID = 1;
    data.uFlags = NIF::ICON | NIF::MESSAGE | NIF::TIP;
    data.uCallbackMessage = WM_TRAY_ICON;
    data.hIcon = HINSTANCE::GetModuleHandle(None)?.LoadIcon(IdIdiStr::Id(2))?.leak();
    data.set_szTip("Overbrowsered");
    Ok(data)
}

fn show_menu(window: &HWND) -> Result<()> {
    let (browser_line, default_line, we_are_default) = STATE.with(|state| {
        let most_recent_browser_id = state.most_recent_browser_id.borrow();
        let most_recent_browser_name = most_recent_browser_id.as_deref().map(|id| {
            state
                .installed_browsers
                .iter()
                .find(|browser| browser.program_id == id)
                .map_or(id, |browser| browser.display_name.as_str())
        });
        let browser_line = most_recent_browser_line(most_recent_browser_name);
        let default_handler = default_http_handler();
        let we_are_default = default_handler.as_deref() == Some(OUR_PROGRAM_ID);
        let handler_name = default_handler.as_ref().and_then(|id| {
            state.installed_browsers.iter().find(|browser| &browser.program_id == id)
        });
        let default_line = default_handler_line(
            we_are_default,
            handler_name.map(|browser| browser.display_name.as_str()),
        );
        (browser_line, default_line, we_are_default)
    });
    let mut menu = HMENU::CreatePopupMenu()?;
    let unclickable = MF::STRING | MF::DISABLED;
    menu.AppendMenu(unclickable, IdMenu::None, BmpPtrStr::from_str(AUTHOR_LINE))?;
    menu.AppendMenu(MF::SEPARATOR, IdMenu::None, BmpPtrStr::None)?;
    menu.AppendMenu(unclickable, IdMenu::None, BmpPtrStr::from_str(&browser_line))?;
    menu.AppendMenu(unclickable, IdMenu::None, BmpPtrStr::from_str(&default_line))?;
    if !we_are_default {
        menu.AppendMenu(
            MF::STRING,
            IdMenu::Id(SET_DEFAULT_MENU_ITEM),
            BmpPtrStr::from_str(SET_DEFAULT_PROMPT),
        )?;
    }
    menu.AppendMenu(MF::SEPARATOR, IdMenu::None, BmpPtrStr::None)?;
    menu.AppendMenu(MF::STRING, IdMenu::Id(QUIT_MENU_ITEM), BmpPtrStr::from_str("Quit"))?;

    let cursor = GetCursorPos()?;
    window.SetForegroundWindow();
    let chosen = menu.TrackPopupMenu(TPM::RETURNCMD, cursor, window)?;
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
    // SAFETY: ASSOCCHANGED requires IDLIST and two unused null item pointers.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, ptr::null(), ptr::null()) };
    ShellExecuteEx(&SHELLEXECUTEINFO {
        file: "ms-settings:defaultapps?registeredAppUser=Overbrowsered",
        show: SW::SHOWNORMAL,
        ..Default::default()
    })
    .context("opening the default apps settings")?;
    Ok(())
}

fn executable_path_of_window(window: &HWND) -> SysResult<String> {
    let (_thread, process) = window.GetWindowThreadProcessId();
    let handle = HPROCESS::OpenProcess(PROCESS::QUERY_LIMITED_INFORMATION, false, process)?;
    let path = handle.QueryFullProcessImageName(PROCESS_NAME::WIN32)?;
    Ok(path.to_lowercase())
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

    let client = CURRENT_USER.create(r"Software\Clients\StartMenuInternet\Overbrowsered")?;
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
    registered.set_string(
        "Overbrowsered",
        r"Software\Clients\StartMenuInternet\Overbrowsered\Capabilities",
    )?;
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
