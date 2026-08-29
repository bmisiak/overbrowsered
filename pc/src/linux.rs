use crate::{AUTHOR_LINE, NO_BROWSER_SEEN_YET};
use anyhow::{Context, Result, bail};
use atspi::events::window::ActivateEvent;
use atspi::{AccessibilityConnection, Event, WindowEvents};
use freedesktop_desktop_entry::{desktop_entries, get_languages_from_env};
use futures_lite::StreamExt;
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Tray, TrayMethods};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::RwLock;
use zbus::fdo::DBusProxy;
use zbus::names::BusName;

const TRAY_ICON_ARGB: &[u8] = include_bytes!("../icons/tray-22.argb");
const DESKTOP_FILE: &str = "overbrowsered.desktop";

pub fn report(error: &anyhow::Error) {
    eprintln!("{error:#}");
    let _notification_is_best_effort = Command::new("notify-send")
        .args([
            "--app-name=Overbrowsered",
            "Overbrowsered",
            &format!("{error:#}"),
        ])
        .spawn();
}

pub fn open(links: &[String]) -> Result<()> {
    let appid = load_most_recent_browser_id().context(
        "Overbrowsered could not find a browser to open this link. \
         Focus any browser window once so it can learn which one you use, \
         then try the link again.",
    )?;
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
        Overbrowsered
            .assume_sni_available(true)
            .spawn()
            .await
            .context("connecting to the session bus")?;
        watch_focused_windows_for_browsers().await
    })
}

fn most_recent_browser_id() -> Option<String> {
    MOST_RECENT_BROWSER_ID
        .read()
        .expect("no lock holder panics")
        .clone()
}

fn set_most_recent_browser_id(appid: String) {
    *MOST_RECENT_BROWSER_ID
        .write()
        .expect("no lock holder panics") = Some(appid);
}

async fn watch_focused_windows_for_browsers() -> Result<()> {
    let installed = installed_browsers();
    if let Some(appid) = load_most_recent_browser_id() {
        set_most_recent_browser_id(appid);
    }
    let accessibility = AccessibilityConnection::new()
        .await
        .context("connecting to the accessibility bus")?;
    accessibility.register_event::<ActivateEvent>().await?;
    let bus = DBusProxy::new(accessibility.connection()).await?;

    let mut activations = std::pin::pin!(accessibility.event_stream());
    while let Some(event) = activations.next().await {
        let Ok(Event::Window(WindowEvents::Activate(activation))) = event else {
            continue;
        };
        let Some(bus_name) = activation.item.name() else {
            continue;
        };
        let Ok(pid) = bus
            .get_connection_unix_process_id(BusName::Unique(bus_name.to_owned()))
            .await
        else {
            continue;
        };
        let running_as = recognize_process(pid);
        let Some(browser) = installed
            .iter()
            .find(|b| Some(&b.recognized_by) == running_as.as_ref())
        else {
            continue;
        };
        if most_recent_browser_id().as_deref() == Some(browser.appid.as_str()) {
            continue;
        }
        if let Err(error) = save_most_recent_browser_id(&browser.appid) {
            eprintln!(
                "cannot save most recent browser {}: {error:#}",
                browser.appid
            );
        }
        set_most_recent_browser_id(browser.appid.clone());
    }
    bail!("the accessibility event stream ended")
}

#[derive(PartialEq)]
enum RecognizedBy {
    Executable(String),
    FlatpakApp(String),
}

struct Browser {
    appid: String,
    display_name: String,
    recognized_by: RecognizedBy,
}

static MOST_RECENT_BROWSER_ID: RwLock<Option<String>> = RwLock::new(None);

struct Overbrowsered;

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
        let most_recent = match most_recent_browser_id() {
            None => NO_BROWSER_SEEN_YET.to_owned(),
            Some(appid) => installed_browsers()
                .into_iter()
                .find(|browser| browser.appid == appid)
                .map_or(appid, |browser| browser.display_name),
        };
        let default_handler = default_browser_desktop_file();
        let we_are_default = default_handler.as_deref() == Some(DESKTOP_FILE);
        let mut items = vec![
            unclickable(AUTHOR_LINE.into()),
            MenuItem::Separator,
            unclickable(format!("Most recently used browser: {most_recent}")),
            unclickable(if we_are_default {
                "Default browser: me 👌".to_owned()
            } else {
                format!(
                    "Default browser: {} ☹️",
                    default_handler
                        .as_deref()
                        .map_or("not me", |file| file.trim_end_matches(".desktop"))
                )
            }),
        ];
        if !we_are_default {
            items.push(
                StandardItem {
                    label: "⚠️ For Overbrowsered to work, click here to set it as the default \"browser\".".into(),
                    activate: Box::new(|_| {
                        if let Err(error) = Command::new("xdg-settings")
                            .args(["set", "default-web-browser", DESKTOP_FILE])
                            .spawn()
                        {
                            eprintln!("cannot run xdg-settings: {error}");
                        }
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
    let output = Command::new("xdg-settings")
        .args(["get", "default-web-browser"])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|file| !file.is_empty())
}

fn recognize_process(pid: u32) -> Option<RecognizedBy> {
    if let Ok(flatpak_info) = std::fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info")) {
        let app = flatpak_info
            .lines()
            .find_map(|line| line.strip_prefix("name="))?;
        return Some(RecognizedBy::FlatpakApp(app.to_owned()));
    }
    let path = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some(RecognizedBy::Executable(
        path.file_name()?.to_str()?.to_owned(),
    ))
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
                recognized_by: match entry.flatpak() {
                    Some(app) => RecognizedBy::FlatpakApp(app.to_owned()),
                    None => RecognizedBy::Executable(
                        Path::new(entry.parse_exec().ok()?.first()?)
                            .file_name()?
                            .to_str()?
                            .to_owned(),
                    ),
                },
            })
        })
        .collect()
}

fn register_as_link_handler() -> Result<()> {
    let executable = std::env::current_exe()?;
    let directory = xdg_directory("XDG_DATA_HOME", ".local/share")?.join("applications");
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
    if let Err(error) = Command::new("update-desktop-database")
        .arg(&directory)
        .spawn()
    {
        eprintln!("cannot run update-desktop-database: {error}");
    }
    Ok(())
}

fn xdg_directory(variable: &str, home_fallback: &str) -> Result<PathBuf> {
    if let Ok(directory) = std::env::var(variable) {
        return Ok(PathBuf::from(directory));
    }
    let home = std::env::var("HOME").context("HOME is unset")?;
    Ok(Path::new(&home).join(home_fallback))
}

fn config_directory() -> Result<PathBuf> {
    Ok(xdg_directory("XDG_CONFIG_HOME", ".config")?.join("overbrowsered"))
}

fn load_most_recent_browser_id() -> Option<String> {
    let appid = std::fs::read_to_string(config_directory().ok()?.join("browser")).ok()?;
    Some(appid.trim().to_owned())
}

fn save_most_recent_browser_id(appid: &str) -> Result<()> {
    let directory = config_directory()?;
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("browser"), appid)?;
    Ok(())
}
