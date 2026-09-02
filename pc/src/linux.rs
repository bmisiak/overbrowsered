use crate::{
    APP_DESCRIPTION, AUTHOR_LINE, NO_BROWSER_ADVICE, SET_DEFAULT_PROMPT, default_handler_line,
    most_recent_browser_line,
};
use anyhow::{Context, Result};
use atspi::events::object::StateChangedEvent;
use atspi::events::window::ActivateEvent;
use atspi::{AccessibilityConnection, Event, ObjectEvents, State, WindowEvents};
use freedesktop_desktop_entry::{DesktopEntry, desktop_entries, get_languages_from_env};
use futures_lite::StreamExt;
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Tray, TrayMethods};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use xdg::BaseDirectories;
use zbus::fdo::DBusProxy;
use zbus::names::BusName;

const TRAY_ICON_ARGB: &[u8] = include_bytes!("../icons/tray-22.argb");
const APP_ICON_PNG: &[u8] = include_bytes!("../icons/app-256.png");
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
    let entry =
        browser_entries().into_iter().find(|entry| entry.appid == appid).with_context(|| {
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

async fn watch_focused_windows_for_browsers() -> Result<()> {
    let mut most_recent = load_most_recent_browser_id();
    let mut browsers: HashMap<String, Option<String>> = HashMap::new();
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
        let appid = match browsers.get(bus_name.as_str()) {
            Some(known) => known.clone(),
            None => {
                let pid = bus.get_connection_unix_process_id(BusName::Unique(bus_name.to_owned()));
                let appid = pid.await.ok().and_then(browser_of);
                browsers.insert(bus_name.to_string(), appid.clone());
                appid
            }
        };
        let Some(appid) = appid else {
            continue;
        };
        if most_recent.as_deref() == Some(appid.as_str()) {
            continue;
        }
        if let Err(error) = save_most_recent_browser_id(&appid) {
            eprintln!("cannot save most recent browser {appid}: {error:#}");
        }
        most_recent = Some(appid);
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

struct Overbrowsered;

impl Tray for Overbrowsered {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "overbrowsered".into()
    }

    fn menu_about_to_show(&mut self) {
        // Overriding this makes ksni rebuild the menu whenever the host opens it.
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![ksni::Icon { width: 22, height: 22, data: TRAY_ICON_ARGB.to_vec() }]
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let unclickable =
            |label: String| StandardItem { label, enabled: false, ..Default::default() }.into();
        let most_recent = load_most_recent_browser_id().map(|appid| browser_name(&appid));
        let default_handler = default_browser_desktop_file();
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
                    activate: Box::new(|_| {
                        if let Err(error) = Command::new("xdg-settings")
                            .args(["set", "default-web-browser", DESKTOP_FILE])
                            .status()
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
    let output = Command::new("xdg-settings").args(["get", "default-web-browser"]).output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|file| !file.is_empty())
}

fn browser_entries() -> Vec<DesktopEntry> {
    desktop_entries(&get_languages_from_env())
        .into_iter()
        .filter(|entry| entry.appid != "overbrowsered")
        .filter(|entry| {
            entry.mime_type().is_some_and(|types| types.contains(&"x-scheme-handler/http"))
        })
        .collect()
}

fn browser_name(appid: &str) -> String {
    let locales = get_languages_from_env();
    browser_entries()
        .iter()
        .find(|entry| entry.appid == appid)
        .and_then(|entry| entry.name(&locales))
        .map_or_else(|| appid.to_owned(), |name| name.into_owned())
}

fn browser_of(pid: u32) -> Option<String> {
    let browsers = browser_entries();
    let by_appid = |appid: &str| browsers.iter().find(|entry| entry.appid == appid);
    let found = if let Some(file) = process_variable(pid, "GIO_LAUNCHED_DESKTOP_FILE") {
        by_appid(Path::new(&file).file_stem()?.to_str()?)
    } else if let Some(appid) = systemd_unit_appid(pid) {
        by_appid(&appid)
    } else if let Ok(flatpak_info) =
        std::fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info"))
    {
        by_appid(flatpak_info.lines().find_map(|line| line.strip_prefix("name="))?)
    } else if let Some(snap) = process_variable(pid, "SNAP_INSTANCE_NAME") {
        browsers.iter().find(|entry| entry.desktop_entry("X-SnapInstanceName") == Some(&snap))
    } else {
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let bin_dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH")?).collect();
        let private_dir = |program: &Path| {
            let dir = std::fs::canonicalize(program).ok()?.parent()?.to_path_buf();
            (!bin_dirs.contains(&dir)).then_some(dir)
        };
        browsers.iter().find(|entry| {
            exec_program(entry).is_some_and(|program| {
                program.file_name() == exe.file_name()
                    || private_dir(&program).as_deref() == exe.parent()
            })
        })
    };
    found.map(|entry| entry.appid.clone())
}

fn process_variable(pid: u32, name: &str) -> Option<String> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let value = environ
        .split(|byte| *byte == 0)
        .find_map(|pair| pair.strip_prefix(format!("{name}=").as_bytes()))?;
    Some(String::from_utf8_lossy(value).into_owned())
}

fn systemd_unit_appid(pid: u32) -> Option<String> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    cgroup.lines().next()?.split('/').find_map(appid_in_systemd_unit)
}

fn appid_in_systemd_unit(unit: &str) -> Option<String> {
    let (name, kind) = unit.strip_prefix("app-")?.rsplit_once('.')?;
    let name = match kind {
        "scope" => name.rsplit_once('-')?.0,
        "service" => name.split_once('@').map_or(name, |(name, _)| name),
        _ => return None,
    };
    let appid = name.split_once('-').map_or(name, |(_launcher, appid)| appid);
    Some(appid.replace("\\x2d", "-"))
}

fn exec_program(entry: &DesktopEntry) -> Option<PathBuf> {
    let program =
        entry.parse_exec().ok()?.into_iter().find(|word| word != "env" && !word.contains('='))?;
    let program = Path::new(&program);
    if program.is_absolute() {
        return Some(program.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| p.exists())
}

fn register_as_link_handler() -> Result<()> {
    let executable = std::env::current_exe()?;
    let data_home = BaseDirectories::new().get_data_home().context("HOME is unset")?;
    let directory = data_home.join("applications");
    // Declare every type `xdg-settings set default-web-browser` registers, in the order it
    // produces. If any is missing, the command patches this file and sleeps 4 seconds per type. Crazy shit.
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Overbrowsered\nComment={APP_DESCRIPTION}\nExec={} %u\nIcon=overbrowsered\nTerminal=false\nStartupNotify=false\nCategories=Network;WebBrowser;\nMimeType=x-scheme-handler/unknown;x-scheme-handler/about;text/html;x-scheme-handler/http;x-scheme-handler/https;\n",
        executable.display()
    );
    let icon = data_home.join("icons/hicolor/256x256/apps/overbrowsered.png");
    if std::fs::read(&icon).ok().as_deref() != Some(APP_ICON_PNG) {
        std::fs::create_dir_all(icon.parent().context("icon path has no parent")?)?;
        std::fs::write(&icon, APP_ICON_PNG)?;
    }
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

#[cfg(test)]
mod tests {
    use super::appid_in_systemd_unit;

    #[test]
    fn systemd_unit_names_carry_the_appid() {
        let appid = |unit| appid_in_systemd_unit(unit).unwrap();
        assert_eq!(appid("app-gnome-org.gnome.Epiphany-13869.scope"), "org.gnome.Epiphany");
        assert_eq!(appid("app-org.kde.konsole-1234.scope"), "org.kde.konsole");
        assert_eq!(appid("app-gnome-google\\x2dchrome-5.scope"), "google-chrome");
        assert_eq!(appid("app-gnome-org.gnome.Ptyxis@abc.service"), "org.gnome.Ptyxis");
        assert_eq!(appid("app-org.kde.dolphin.service"), "org.kde.dolphin");
        assert_eq!(appid_in_systemd_unit("dbus-:1.2-org.mozilla.firefox@0.service"), None);
        assert_eq!(appid_in_systemd_unit("session-20.scope"), None);
    }
}
