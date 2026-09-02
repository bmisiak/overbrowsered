use crate::{
    APP_DESCRIPTION, AUTHOR_LINE, NO_BROWSER_ADVICE, SET_DEFAULT_PROMPT, default_handler_line,
    most_recent_browser_line,
};
use anyhow::{Context, Result};
use atspi::events::object::StateChangedEvent;
use atspi::events::window::ActivateEvent;
use atspi::{AccessibilityConnection, Event, ObjectEvents, State, WindowEvents};
use freedesktop_desktop_entry::{desktop_entries, get_languages_from_env};
use futures_lite::StreamExt;
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Tray, TrayMethods};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use xdg::BaseDirectories;
use zbus::fdo::DBusProxy;
use zbus::names::BusName;

const TRAY_ICON_ARGB: &[u8] = include_bytes!("../icons/tray-22.argb");
const DESKTOP_FILE: &str = "overbrowsered.desktop";

pub fn report(error: &anyhow::Error) {
    eprintln!("{error:#}");
    let _notification_is_best_effort = Command::new("notify-send")
        .args(["--app-name=Overbrowsered", "Overbrowsered", &format!("{error:#}")])
        .spawn();
}

pub fn open(links: &[String]) -> Result<()> {
    let appid = load_most_recent_browser_id().context(NO_BROWSER_ADVICE)?;
    let locales = get_languages_from_env();
    let entry = desktop_entries(&locales)
        .into_iter()
        .find(|entry| entry.appid == appid)
        .with_context(|| {
            format!(
                "{appid} seems to be gone. Focus another browser window \
                 so Overbrowsered can learn your new one, then try the link again."
            )
        })?;
    let links: Vec<&str> = links.iter().map(String::as_str).collect();
    let argv = entry
        .parse_exec_with_uris(&links, &locales)
        .with_context(|| format!("parsing the Exec line of {appid}"))?;
    let (program, arguments) = argv.split_first().context("empty launch command")?;
    Command::new(program)
        .args(arguments)
        .spawn()
        .with_context(|| format!("launching {program}"))?;
    Ok(())
}

pub fn run() -> Result<()> {
    futures_lite::future::block_on(async {
        register_as_link_handler().context("registering as a browser")?;
        let tray = Overbrowsered {
            most_recent: load_most_recent_browser_id()
                .and_then(|appid| browsers().find(|browser| browser.appid == appid)),
            default_handler: default_browser_desktop_file(),
        }
        .assume_sni_available(true)
        .spawn()
        .await
        .context("connecting to the session bus")?;
        watch_focused_windows_for_browsers(tray).await
    })
}

async fn watch_focused_windows_for_browsers(tray: ksni::Handle<Overbrowsered>) -> Result<()> {
    let mut most_recent = load_most_recent_browser_id();
    let mut programs: HashMap<String, Option<Browser>> = HashMap::new();
    if let Err(error) = enable_accessibility().await {
        eprintln!("cannot enable accessibility: {error:#}");
    }
    let accessibility =
        AccessibilityConnection::new().await.context("connecting to the accessibility bus")?;
    accessibility.register_event::<ActivateEvent>().await?;
    accessibility.register_event::<StateChangedEvent>().await?;
    let bus = DBusProxy::new(accessibility.connection()).await?;

    let mut activations = std::pin::pin!(accessibility.event_stream());
    while let Some(event) = activations.next().await {
        let activated = match event {
            Ok(Event::Window(WindowEvents::Activate(activation))) => activation.item,
            Ok(Event::Object(ObjectEvents::StateChanged(change)))
                if change.state == State::Active && change.enabled =>
            {
                change.item
            }
            _ => continue,
        };
        let Some(bus_name) = activated.name() else {
            continue;
        };
        let Ok(pid) =
            bus.get_connection_unix_process_id(BusName::Unique(bus_name.to_owned())).await
        else {
            continue;
        };
        let Some(program) = program_of(pid) else {
            continue;
        };
        let Some(browser) = programs
            .entry(program)
            .or_insert_with_key(|program| browsers().find(|browser| &browser.program == program))
        else {
            continue;
        };
        if most_recent.as_deref() == Some(browser.appid.as_str()) {
            continue;
        }
        if let Err(error) = save_most_recent_browser_id(&browser.appid) {
            eprintln!("cannot save most recent browser {}: {error:#}", browser.appid);
        }
        most_recent = Some(browser.appid.clone());
        let browser = browser.clone();
        tray.update(|tray| tray.most_recent = Some(browser)).await;
    }
    eprintln!("the accessibility event stream ended; exiting");
    Ok(())
}

async fn enable_accessibility() -> Result<()> {
    let session = zbus::Connection::session().await?;
    let status =
        zbus::Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Status").await?;
    if status.get_property::<bool>("IsEnabled").await? {
        return Ok(());
    }
    status.set_property("IsEnabled", true).await?;
    Ok(())
}

#[derive(Clone)]
struct Browser {
    appid: String,
    display_name: String,
    program: String,
}

struct Overbrowsered {
    most_recent: Option<Browser>,
    default_handler: Option<String>,
}

impl Tray for Overbrowsered {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "overbrowsered".into()
    }

    fn menu_about_to_show(&mut self) {
        self.default_handler = default_browser_desktop_file();
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![ksni::Icon { width: 22, height: 22, data: TRAY_ICON_ARGB.to_vec() }]
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let unclickable =
            |label: String| StandardItem { label, enabled: false, ..Default::default() }.into();
        let most_recent = self.most_recent.as_ref().map(|browser| browser.display_name.clone());
        let default_handler = &self.default_handler;
        let we_are_default = default_handler.as_deref() == Some(DESKTOP_FILE);
        let mut items = vec![
            unclickable(AUTHOR_LINE.into()),
            MenuItem::Separator,
            unclickable(most_recent_browser_line(most_recent.as_deref())),
            unclickable(default_handler_line(
                we_are_default,
                default_handler.as_deref().map(|file| file.trim_end_matches(".desktop")),
            )),
        ];
        if !we_are_default {
            items.push(
                StandardItem {
                    label: SET_DEFAULT_PROMPT.into(),
                    activate: Box::new(|tray: &mut Self| {
                        if let Err(error) = Command::new("xdg-settings")
                            .args(["set", "default-web-browser", DESKTOP_FILE])
                            .status()
                        {
                            eprintln!("cannot run xdg-settings: {error}");
                        }
                        tray.default_handler = default_browser_desktop_file();
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

fn default_browser_desktop_file() -> Option<String> {
    let output = Command::new("xdg-settings").args(["get", "default-web-browser"]).output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|file| !file.is_empty())
}

fn program_of(pid: u32) -> Option<String> {
    if let Ok(flatpak_info) = std::fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info")) {
        return flatpak_info.lines().find_map(|line| line.strip_prefix("name=")).map(str::to_owned);
    }
    let executable = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some(executable.file_name()?.to_str()?.to_owned())
}

fn browsers() -> impl Iterator<Item = Browser> {
    let locales = get_languages_from_env();
    desktop_entries(&locales).into_iter().filter_map(move |entry| {
        if entry.appid == "overbrowsered" || !entry.mime_type()?.contains(&"x-scheme-handler/http")
        {
            return None;
        }
        let program = match entry.flatpak() {
            Some(app) => app.to_owned(),
            None => Path::new(entry.parse_exec().ok()?.first()?).file_name()?.to_str()?.to_owned(),
        };
        Some(Browser {
            appid: entry.appid.clone(),
            display_name: entry
                .name(&locales)
                .map_or_else(|| entry.appid.clone(), |name| name.into_owned()),
            program,
        })
    })
}

fn register_as_link_handler() -> Result<()> {
    let executable = std::env::current_exe()?;
    let directory =
        BaseDirectories::new().get_data_home().context("HOME is unset")?.join("applications");
    // Declare every type `xdg-settings set default-web-browser` registers, in the order it
    // produces. If any is missing, the command patches this file and sleeps 4 seconds per type. Crazy shit.
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Overbrowsered\nComment={APP_DESCRIPTION}\nExec={} %u\nIcon=overbrowsered\nTerminal=false\nStartupNotify=false\nCategories=Network;WebBrowser;\nMimeType=x-scheme-handler/unknown;x-scheme-handler/about;text/html;x-scheme-handler/http;x-scheme-handler/https;\n",
        executable.display()
    );
    let path = directory.join(DESKTOP_FILE);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(entry.as_str()) {
        return Ok(());
    }
    std::fs::create_dir_all(&directory)?;
    std::fs::write(path, entry)?;
    if let Err(error) = Command::new("update-desktop-database").arg(&directory).spawn() {
        eprintln!("cannot run update-desktop-database: {error}");
    }
    Ok(())
}

fn load_most_recent_browser_id() -> Option<String> {
    let path = BaseDirectories::with_prefix("overbrowsered").get_config_file("browser")?;
    let appid = std::fs::read_to_string(path).ok()?;
    Some(appid.trim().to_owned())
}

fn save_most_recent_browser_id(appid: &str) -> Result<()> {
    let path = BaseDirectories::with_prefix("overbrowsered").place_config_file("browser")?;
    std::fs::write(path, appid)?;
    Ok(())
}
