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
use zbus::fdo::DBusProxy;
use zbus::names::BusName;

const TRAY_ICON_ARGB: &[u8] = include_bytes!("../icons/tray-22.argb");
const DESKTOP_FILE: &str = "overbrowsered.desktop";

pub fn report(error: &anyhow::Error) {
    eprintln!("{error:#}");
}

pub fn open(links: &[String]) -> Result<()> {
    let appid = remembered_browser().context("Overbrowsered has yet to see you use a browser")?;
    let locales = get_languages_from_env();
    let entry = desktop_entries(&locales)
        .into_iter()
        .find(|entry| entry.appid == appid)
        .with_context(|| format!("{appid} is no longer installed"))?;
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
        let installed = installed_browsers();
        let tray = Overbrowsered {
            browser: remembered_browser().and_then(|appid| {
                installed
                    .iter()
                    .find(|browser| browser.appid == appid)
                    .cloned()
            }),
        }
        .assume_sni_available(true)
        .spawn()
        .await
        .context("connecting to the session bus")?;
        watch_for_activations(&installed, &tray).await
    })
}

async fn watch_for_activations(
    installed: &[Browser],
    tray: &ksni::Handle<Overbrowsered>,
) -> Result<()> {
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
        let Some(name) = activation.item.name() else {
            continue;
        };
        let Ok(process) = bus
            .get_connection_unix_process_id(BusName::Unique(name.to_owned()))
            .await
        else {
            continue;
        };
        let running_as = recognize_process(process);
        let Some(browser) = installed
            .iter()
            .find(|b| Some(&b.recognized_by) == running_as.as_ref())
        else {
            continue;
        };
        let newly_activated = tray
            .update(|tray| {
                let newly_activated =
                    tray.browser.as_ref().map(|b| &b.appid) != Some(&browser.appid);
                tray.browser = Some(browser.clone());
                newly_activated
            })
            .await;
        if newly_activated == Some(true) {
            if let Err(error) = remember(&browser.appid) {
                eprintln!("cannot remember {}: {error:#}", browser.appid);
            }
        }
    }
    bail!("the accessibility event stream ended")
}

#[derive(Clone, PartialEq)]
enum RecognizedBy {
    Executable(String),
    FlatpakApp(String),
}

#[derive(Clone)]
struct Browser {
    appid: String,
    display_name: String,
    recognized_by: RecognizedBy,
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

fn file_name(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

fn recognize_process(process: u32) -> Option<RecognizedBy> {
    if let Ok(flatpak_info) = std::fs::read_to_string(format!("/proc/{process}/root/.flatpak-info"))
    {
        let app = flatpak_info
            .lines()
            .find_map(|line| line.strip_prefix("name="))?;
        return Some(RecognizedBy::FlatpakApp(app.to_owned()));
    }
    let path = std::fs::read_link(format!("/proc/{process}/exe")).ok()?;
    Some(RecognizedBy::Executable(file_name(&path)?.to_owned()))
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
                        file_name(Path::new(entry.parse_exec().ok()?.first()?))?.to_owned(),
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
    let _ = Command::new("update-desktop-database")
        .arg(&directory)
        .spawn();
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

fn remembered_browser() -> Option<String> {
    let appid = std::fs::read_to_string(config_directory().ok()?.join("browser")).ok()?;
    Some(appid.trim().to_owned())
}

fn remember(appid: &str) -> Result<()> {
    let directory = config_directory()?;
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("browser"), appid)?;
    Ok(())
}
