//! Horizon Offline: a terminal front end for the native Rust client.
//! No Java game and no fallback route.
mod incremental;
mod minecraft_settings;
mod ui;
mod updater;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{Duration, Instant},
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SETTINGS_FILE: &str = "horizon-rust.json";

type Rgb = (u8, u8, u8);
const VIOLET: Rgb = (196, 181, 253);
const DEEP: Rgb = (109, 40, 217);
const GREEN: Rgb = (74, 222, 128);
const RED: Rgb = (248, 113, 113);
const YELLOW: Rgb = (250, 204, 21);
const GRAY: Rgb = (148, 163, 184);
const WHITE: Rgb = (241, 245, 249);

// ---------------------------------------------------------------- options

struct Options {
    plan: bool,
    check: bool,
    menu: bool,
    help: bool,
    version: bool,
    licenses: bool,
    no_update: bool,
    updates_check: bool,
    prepare: bool,
    forwarded: Vec<OsString>,
}

fn parse(args: impl IntoIterator<Item = OsString>, interactive: bool) -> Result<Options, String> {
    let mut options = Options {
        plan: false,
        check: false,
        menu: false,
        help: false,
        version: false,
        licenses: false,
        no_update: false,
        updates_check: false,
        prepare: true,
        forwarded: Vec::new(),
    };
    let mut explicit = false;
    for arg in args {
        match arg.to_str() {
            Some("--java" | "--fabric") => {
                return Err(
                    "Java fallback was removed. Only the native Rust client is supported.".into(),
                );
            }
            Some("--rust") => explicit = true,
            Some("--plan" | "-PlanOnly") => {
                options.plan = true;
                explicit = true;
            }
            Some("--check") => {
                options.check = true;
                explicit = true;
            }
            Some("--menu") => {
                options.menu = true;
                explicit = true;
            }
            Some("--no-prepare") => {
                options.prepare = false;
                explicit = true;
            }
            Some("--help" | "-h") => options.help = true,
            Some("--version" | "-V") => options.version = true,
            Some("--licenses") => options.licenses = true,
            Some("--no-update") => options.no_update = true,
            Some("--updates-check") => {
                options.updates_check = true;
                explicit = true;
            }
            _ => {
                options.forwarded.push(arg);
                explicit = true;
            }
        }
    }
    if !explicit && interactive && !options.help {
        options.menu = true;
    }
    if options.menu && (options.plan || options.check || !options.forwarded.is_empty()) {
        return Err("use --menu alone, or --rust/--plan/--check with script options".into());
    }
    Ok(options)
}

// --------------------------------------------------------------- settings

/// The player's choices, kept in `horizon-rust.json` beside the launcher.
#[derive(Clone, Debug, PartialEq)]
struct Settings {
    name: String,
    minecraft_dir: Option<String>,
    /// Paths found by the last successful check; lets the game start without PowerShell.
    launch: Option<Launch>,
}

#[derive(Clone, Debug, PartialEq)]
struct Launch {
    client: String,
    resources: String,
    home: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: "Player".into(),
            minecraft_dir: None,
            launch: None,
        }
    }
}

fn valid_name(name: &str) -> bool {
    (3..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Settings {
    fn load(root: &Path) -> Self {
        let mut settings = Self::default();
        let Ok(text) = fs::read_to_string(root.join(SETTINGS_FILE)) else {
            return settings;
        };
        let Ok(value) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
            return settings;
        };
        if let Some(name) = value["name"].as_str().filter(|n| valid_name(n)) {
            settings.name = name.into();
        }
        if let Some(dir) = value["minecraft_dir"].as_str().filter(|d| !d.is_empty()) {
            settings.minecraft_dir = Some(dir.into());
        }
        let launch = &value["launch"];
        if let (Some(client), Some(resources), Some(home)) = (
            launch["client"].as_str(),
            launch["resources"].as_str(),
            launch["home"].as_str(),
        ) {
            settings.launch = Some(Launch {
                client: client.into(),
                resources: resources.into(),
                home: home.into(),
            });
        }
        settings
    }

    fn save(&self, root: &Path) -> Result<(), String> {
        let launch = self
            .launch
            .as_ref()
            .map(|l| json!({ "client": l.client, "resources": l.resources, "home": l.home }));
        let value =
            json!({ "name": self.name, "minecraft_dir": self.minecraft_dir, "launch": launch });
        let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        fs::write(root.join(SETTINGS_FILE), text)
            .map_err(|e| format!("не удалось сохранить {SETTINGS_FILE}: {e}"))
    }
}

fn has_flag(args: &[OsString], flag: &str) -> bool {
    args.iter()
        .any(|a| a.to_str().is_some_and(|a| a.eq_ignore_ascii_case(flag)))
}

/// Adds the saved nickname and Minecraft folder unless the command line set them.
fn apply_settings(options: &mut Options, settings: &Settings) {
    if !has_flag(&options.forwarded, "-Name") {
        options.forwarded.push("-Name".into());
        options.forwarded.push(settings.name.clone().into());
    }
    let dir = settings
        .minecraft_dir
        .as_ref()
        .filter(|_| !has_flag(&options.forwarded, "-MinecraftDir"));
    if let Some(dir) = dir {
        options.forwarded.push("-MinecraftDir".into());
        options.forwarded.push(dir.clone().into());
    }
}

// --------------------------------------------------------- native launch

/// The folder the launcher works from: the package with `scripts/`, else the folder holding the
/// original `horizon-launcher.exe` (the folder-of-exes mode, see [`Standalone`]).
fn root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    root_for(&exe, standalone_forced())
}

fn root_for(exe: &Path, forced: bool) -> Result<PathBuf, String> {
    let parent = exe.parent().ok_or("cannot locate the executable folder")?;
    for candidate in [Some(parent), parent.parent()].into_iter().flatten() {
        if (!forced && candidate.join(SCRIPT).is_file()) || official_launcher(candidate).is_some() {
            return Ok(candidate.to_path_buf());
        }
    }
    Err(
        "положи оригинальный horizon-launcher.exe (из релиза Horizon) рядом с этим лаунчером"
            .into(),
    )
}

const SCRIPT: &str = "scripts/Start-HorizonFabric.ps1";
const RESOURCES_MANIFEST: &str = "horizon-resources.json";
const CLIENT_EXE: &str = "horizon-client.exe";

fn standalone_forced() -> bool {
    std::env::var_os("HORIZON_RUST_STANDALONE").is_some_and(|v| v == "1")
}

/// The folder-of-exes mode: no scripts and no source tree, just this launcher and the original
/// `horizon-launcher.exe`. Everything is kept in `Horizon Rust Data/` beside them. The original
/// launcher's own `install` downloads (and signature-checks) the client and its `prepare` builds
/// the resources from the player's Minecraft folder; the game then starts from here, offline.
struct Standalone {
    official: PathBuf,
    data: PathBuf,
}

fn standalone(root: &Path) -> Option<Standalone> {
    if !standalone_forced() && root.join(SCRIPT).is_file() {
        return None;
    }
    let legacy = root.join("Horizon Rust Data");
    let data = if legacy.is_dir() {
        legacy
    } else {
        root.join("Horizon Offline Data")
    };
    Some(Standalone {
        official: official_launcher(root)?,
        data,
    })
}

impl Standalone {
    fn home(&self) -> PathBuf {
        self.data.join("game")
    }

    /// The newest fully installed client folder: the program plus the `.installed` mark.
    fn installed(&self) -> Option<PathBuf> {
        fs::read_dir(self.data.join("versions"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.join(CLIENT_EXE).is_file())
            .filter_map(|p| {
                let stamp = fs::metadata(p.join(".installed")).ok()?.modified().ok()?;
                Some((stamp, p))
            })
            .max()
            .map(|(_, p)| p)
    }

    fn resources_of(dir: &Path) -> PathBuf {
        let version = fs::read_to_string(dir.join("research-lock.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["minecraft"]["version"].as_str().map(String::from))
            .unwrap_or_else(|| "26.3".into());
        dir.join(".local/horizon-client").join(version)
    }

    fn report(&self) -> Report {
        let dir = self.installed();
        let resources = dir.as_deref().map(Self::resources_of);
        let text = |p: Option<PathBuf>| {
            p.map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        Report {
            ready: true,
            client_exists: dir.is_some(),
            resources_prepared: resources
                .as_ref()
                .is_some_and(|r| r.join(RESOURCES_MANIFEST).is_file()),
            client: text(dir.map(|d| d.join(CLIENT_EXE))),
            resources: text(resources),
            home: self.home().to_string_lossy().into_owned(),
            problems: Vec::new(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.official);
        command
            .env("HORIZON_LAUNCHER_HOME", &self.data)
            .env("HORIZON_HOME", self.home())
            .env("HORIZON_LAUNCHER_NO_SELF_UPDATE", "1");
        command
    }

    fn run_official(&self, args: &[String]) -> Result<(), String> {
        fs::create_dir_all(&self.data).map_err(|e| e.to_string())?;
        let status = ui::run_command(self.command().args(args), "Подготовка ресурсов Minecraft")?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "horizon-launcher {} завершился с ошибкой ({status})",
                args.join(" ")
            ))
        }
    }

    fn install(&self) -> Result<InstallOutcome, String> {
        install_client(|| self.command())
    }

    fn prepare(&self, settings: &Settings) -> Result<(), String> {
        println!("\n  Ресурсы Minecraft 26.3 · первая подготовка может занять несколько минут.");
        let mut args = vec!["prepare".to_string()];
        if let Some(dir) = &settings.minecraft_dir {
            args.push("--minecraft".into());
            args.push(dir.clone());
        }
        self.run_official(&args)
    }

    /// Downloads and prepares whatever is missing, then says where the game is.
    fn ensure(&self, settings: &Settings) -> Result<Launch, String> {
        if self.installed().is_none() {
            self.install()?;
        }
        let dir = self
            .installed()
            .ok_or("клиент Horizon не установился: проверь интернет и повтори (пункт 6)")?;
        let resources = Self::resources_of(&dir);
        if !resources.join(RESOURCES_MANIFEST).is_file() {
            self.prepare(settings)?;
            if !resources.join(RESOURCES_MANIFEST).is_file() {
                return Err("ресурсы не подготовились. Нужна установленная Minecraft 26.3: укажи её папку (пункт 4)".into());
            }
        }
        Ok(Launch {
            client: dir.join(CLIENT_EXE).to_string_lossy().into_owned(),
            resources: resources.to_string_lossy().into_owned(),
            home: self.home().to_string_lossy().into_owned(),
        })
    }
}

fn command(root: &Path, options: &Options) -> Command {
    let mut command = Command::new("powershell.exe");
    command
        .env_remove("PSModulePath")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("scripts/Start-HorizonFabric.ps1"));
    if options.prepare && !options.forwarded.iter().any(|a| a == "-Prepare") {
        command.arg("-Prepare");
    }
    if options.plan || options.check {
        command.arg("-PlanOnly");
    }
    command.args(&options.forwarded);
    command
}

struct Report {
    ready: bool,
    client_exists: bool,
    resources_prepared: bool,
    client: String,
    resources: String,
    home: String,
    problems: Vec<String>,
}

fn preflight(root: &Path, options: &Options) -> Result<Report, String> {
    if let Some(folder_mode) = standalone(root) {
        return Ok(folder_mode.report());
    }
    let out = command(root, options)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let value: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}').trim()).map_err(|e| {
            format!(
                "native preflight: {e}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        })?;
    let text_of = |key: &str| value[key].as_str().unwrap_or_default().to_string();
    Ok(Report {
        ready: value["ReadyForRequestedAction"].as_bool().unwrap_or(false),
        client_exists: value["ClientExists"].as_bool().unwrap_or(false),
        resources_prepared: value["ResourcesPrepared"].as_bool() == Some(true),
        client: text_of("ClientExecutable"),
        resources: text_of("Resources"),
        home: text_of("HorizonHome"),
        problems: value["Problems"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    })
}

/// Keeps the paths of a fully ready install, or forgets them when it is not ready.
fn remember(settings: &mut Settings, root: &Path, report: &Report) {
    let complete = report.client_exists
        && report.resources_prepared
        && !report.client.is_empty()
        && !report.resources.is_empty()
        && !report.home.is_empty();
    let launch = complete.then(|| Launch {
        client: report.client.clone(),
        resources: report.resources.clone(),
        home: report.home.clone(),
    });
    if settings.launch != launch {
        settings.launch = launch;
        let _ = settings.save(root);
    }
}

/// The client started the way Start-HorizonFabric.ps1 starts it, minus PowerShell and its checks.
fn direct_command(settings: &Settings) -> Option<Command> {
    launch_command(settings.launch.as_ref()?, &settings.name)
}

fn launch_command(launch: &Launch, name: &str) -> Option<Command> {
    if !Path::new(&launch.client).is_file()
        || !Path::new(&launch.resources)
            .join(RESOURCES_MANIFEST)
            .is_file()
    {
        return None;
    }
    let mut command = Command::new(&launch.client);
    command
        .args(["--resources", &launch.resources, "--name", name])
        .env("HORIZON_HOME", &launch.home)
        .env_remove("HORIZON_SESSION_FILE");
    Some(command)
}

/// The original Horizon launcher, unmodified, kept beside this one or in `bin/`.
fn official_launcher(root: &Path) -> Option<PathBuf> {
    ["horizon-launcher.exe", "bin/horizon-launcher.exe"]
        .iter()
        .map(|p| root.join(p))
        .find(|p| p.is_file())
}

#[derive(Debug, PartialEq)]
enum InstallOutcome {
    AlreadyCurrent(String),
    Installed(String),
    Updated(String),
}

impl InstallOutcome {
    fn changed(&self) -> bool {
        !matches!(self, Self::AlreadyCurrent(_))
    }

    fn message(&self) -> String {
        match self {
            Self::AlreadyCurrent(tag) => {
                format!("Уже обновлено. Установлена последняя версия Horizon: {tag}.")
            }
            Self::Installed(tag) => format!("Клиент Horizon {tag} установлен."),
            Self::Updated(tag) => format!("Клиент Horizon обновлён до {tag}."),
        }
    }
}

#[derive(serde::Deserialize)]
struct AvailableRelease {
    tag: String,
    available_here: bool,
    installed: bool,
    prerelease: bool,
}

fn installed_tags(value: Value) -> Result<Vec<String>, String> {
    serde_json::from_value(value["installed"].clone())
        .map_err(|e| format!("не удалось прочитать список установленных версий: {e}"))
}

/// Use the official launcher's ordering and channel, exactly as `install latest` does, but
/// install the checked tag so a release published during the check cannot change the result.
fn update_client(
    mut query: impl FnMut(&[&str]) -> Result<Value, String>,
    install: impl FnOnce(&str, &[String]) -> Result<(), String>,
) -> Result<InstallOutcome, String> {
    let before = installed_tags(query(&["versions", "--installed", "--json"])?)?;
    let releases: Vec<AvailableRelease> =
        serde_json::from_value(query(&["versions", "--json"])?)
            .map_err(|e| format!("не удалось прочитать список обновлений: {e}"))?;
    let latest = releases
        .iter()
        .find(|r| r.available_here && !r.prerelease && !r.tag.is_empty())
        .ok_or("пока нет доступной версии Horizon для этой системы. Повтори проверку позже")?;
    if latest.installed && before.contains(&latest.tag) {
        return Ok(InstallOutcome::AlreadyCurrent(latest.tag.clone()));
    }
    install(&latest.tag, &before)?;
    let after = installed_tags(query(&["versions", "--installed", "--json"])?)?;
    if !after.contains(&latest.tag) {
        return Err(format!(
            "установка Horizon {} не подтверждена. Повтори попытку",
            latest.tag
        ));
    }
    if before.is_empty() {
        Ok(InstallOutcome::Installed(latest.tag.clone()))
    } else {
        Ok(InstallOutcome::Updated(latest.tag.clone()))
    }
}

/// Drain both pipes while waiting, including on timeout: a full release list must never block
/// the child on a pipe buffer. An unreachable server returns to the menu after 30 seconds.
fn official_json(command: &mut Command, timeout: Duration) -> Result<Value, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe
                .ok_or_else(|| io::Error::other("нет канала вывода"))?
                .read_to_end(&mut bytes);
            result.map(|_| bytes)
        })
    };
    let stdout = reader(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = reader(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25))
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(match result {
                    Err(e) => e.to_string(),
                    _ => "сервер не ответил за 30 секунд. Проверь интернет и повтори".into(),
                });
            }
        }
    };
    let read = |thread: std::thread::JoinHandle<io::Result<Vec<u8>>>| {
        thread
            .join()
            .map_err(|_| "не удалось прочитать ответ лаунчера".to_string())?
            .map_err(|e| e.to_string())
    };
    let out = read(stdout);
    let err = read(stderr);
    let status = status?;
    let out = out?;
    let err = err?;
    if !status.success() {
        return Err(format!(
            "horizon-launcher ({status}): {}",
            String::from_utf8_lossy(&err).trim()
        ));
    }
    serde_json::from_slice(&out).map_err(|e| format!("некорректный ответ horizon-launcher: {e}"))
}

/// Downloads only a missing version; signature checks remain in the original launcher.
fn install_client(make_command: impl Fn() -> Command) -> Result<InstallOutcome, String> {
    incremental::recover_pending(&make_command())?;
    let mut checking = Some(ui::Task::new("Клиент Horizon · проверка версии"));
    let result = update_client(
        |args| {
            official_json(make_command().args(args), Duration::from_secs(30))
                .map_err(|e| format!("Не удалось проверить версии Horizon: {e}"))
        },
        |tag, installed| {
            if let Some(task) = checking.take() {
                task.finish();
            }
            println!(
                "\n  Скачиваю Horizon {tag} (подпись проверяет официальный horizon-launcher)."
            );
            if let Some(summary) = incremental::install(&make_command, tag, installed)? {
                println!(
                    "  Файлы: без изменений — {}, заменено — {}, добавлено — {}, удалено устаревших — {}.",
                    summary.kept, summary.replaced, summary.added, summary.removed
                );
                if summary.resources_kept {
                    println!(
                        "  Готовые ресурсы Minecraft сохранены, повторная подготовка не нужна."
                    );
                }
            }
            Ok(())
        },
    );
    if let Some(task) = checking {
        if result.is_ok() {
            task.finish();
        }
    }
    result
}

fn install_runtime(root: &Path) -> Result<InstallOutcome, String> {
    if let Some(folder_mode) = standalone(root) {
        return folder_mode.install();
    }
    let exe = official_launcher(root)
        .ok_or("horizon-launcher.exe не найден: положи оригинальный лаунчер Horizon в эту папку")?;
    install_client(|| {
        let mut command = Command::new(&exe);
        command.env("HORIZON_LAUNCHER_NO_SELF_UPDATE", "1");
        command
    })
}

/// One check of the files; downloads the client first when it is missing altogether.
fn check_and_install(root: &Path, settings: &Settings) -> Option<Report> {
    let first = check_options(settings)
        .and_then(|o| preflight(root, &o))
        .ok()?;
    if first.client_exists || official_launcher(root).is_none() {
        return Some(first);
    }
    match install_runtime(root) {
        Ok(_) => check_options(settings)
            .and_then(|o| preflight(root, &o))
            .ok()
            .or(Some(first)),
        Err(e) => {
            eprintln!("  {e}");
            Some(first)
        }
    }
}

/// Starts the game. With remembered paths there is no PowerShell and no checking; the first
/// start (or after the folder changed) checks once, and preparation still goes through the script.
fn start_game(
    root: &Path,
    settings: &mut Settings,
    options: &Options,
    custom_args: bool,
) -> Result<bool, String> {
    if let Some(folder_mode) = standalone(root) {
        let launch = folder_mode.ensure(settings)?;
        fs::create_dir_all(&launch.home).map_err(|e| e.to_string())?;
        import_options(root, settings, Path::new(&launch.home), false);
        let mut command =
            launch_command(&launch, &settings.name).ok_or("файлы клиента не найдены")?;
        println!("\n  Запускаю клиент…");
        return command
            .current_dir(&launch.home)
            .status()
            .map(|s| s.success())
            .map_err(|e| e.to_string());
    }
    if !custom_args {
        let report = direct_command(settings)
            .is_none()
            .then(|| check_and_install(root, settings))
            .flatten();
        if let Some(report) = report {
            if report.ready && !report.home.is_empty() {
                import_options(root, settings, Path::new(&report.home), false);
            }
            remember(settings, root, &report);
        }
        if let Some(mut command) = direct_command(settings) {
            if let Some(launch) = &settings.launch {
                fs::create_dir_all(&launch.home).map_err(|e| e.to_string())?;
                import_options(root, settings, Path::new(&launch.home), false);
                command.current_dir(&launch.home);
            }
            return command
                .status()
                .map(|s| s.success())
                .map_err(|e| e.to_string());
        }
    }
    command(root, options)
        .status()
        .map(|s| s.success())
        .map_err(|e| e.to_string())
}

fn check_options(settings: &Settings) -> Result<Options, String> {
    let mut options = parse(["--check".into()], false)?;
    apply_settings(&mut options, settings);
    Ok(options)
}

fn import_options(root: &Path, settings: &Settings, home: &Path, manual: bool) {
    match minecraft_settings::import(root, settings.minecraft_dir.as_deref(), home, manual) {
        Ok(minecraft_settings::Imported::Copied { source, backup }) => {
            println!(
                "\n  Настройки Minecraft импортированы: {}",
                source.display()
            );
            if let Some(backup) = backup {
                println!("  Предыдущие настройки сохранены: {}", backup.display());
            }
        }
        Ok(minecraft_settings::Imported::NotFound) if manual => {
            println!("\n  options.txt не найден. Укажи папку Minecraft (пункт 4) и повтори.")
        }
        Ok(_) if manual => println!("\n  Настройки уже совпадают с Minecraft."),
        Err(e) => eprintln!("\n  {e}"),
        _ => {}
    }
}

// ------------------------------------------------------------------- look

struct Style {
    color: bool,
}

fn short_path(text: &str, width: usize) -> String {
    let chars: Vec<_> = text.chars().collect();
    if chars.len() <= width {
        return text.into();
    }
    format!(
        "{}…{}",
        chars[..12].iter().collect::<String>(),
        chars[chars.len() - (width - 13)..]
            .iter()
            .collect::<String>()
    )
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

impl Style {
    fn fg(&self, c: Rgb, text: &str) -> String {
        if self.color {
            format!("\x1b[38;2;{};{};{}m{text}\x1b[0m", c.0, c.1, c.2)
        } else {
            text.to_string()
        }
    }
    fn bold(&self, c: Rgb, text: &str) -> String {
        if self.color {
            format!("\x1b[1;38;2;{};{};{}m{text}\x1b[0m", c.0, c.1, c.2)
        } else {
            text.to_string()
        }
    }
}

// Five-pixel glyphs packed into half-blocks: a Minecraft-style wordmark using the terminal's
// own font, with square pixels and no system font installation.
fn pixel_glyph(letter: char) -> [&'static str; 5] {
    match letter {
        'H' => ["10001", "10001", "11111", "10001", "10001"],
        'O' => ["01110", "10001", "10001", "10001", "01110"],
        'R' => ["11110", "10001", "11110", "10100", "10010"],
        'I' => ["11111", "00100", "00100", "00100", "11111"],
        'Z' => ["11111", "00010", "00100", "01000", "11111"],
        'N' => ["10001", "11001", "10101", "10011", "10001"],
        'F' => ["11111", "10000", "11110", "10000", "10000"],
        'L' => ["10000", "10000", "10000", "10000", "11111"],
        'E' => ["11111", "10000", "11110", "10000", "11111"],
        _ => ["00000"; 5],
    }
}

fn logo_rows() -> Vec<String> {
    let mut rows = Vec::new();
    for word in ["HORIZON", "OFFLINE"] {
        for row in (0..5).step_by(2) {
            let letters: Vec<String> = word
                .chars()
                .map(|c| {
                    let glyph = pixel_glyph(c);
                    (0..5)
                        .map(|column| {
                            match (
                                glyph[row].as_bytes()[column] == b'1',
                                row + 1 < 5 && glyph[row + 1].as_bytes()[column] == b'1',
                            ) {
                                (true, true) => '█',
                                (true, false) => '▀',
                                (false, true) => '▄',
                                _ => ' ',
                            }
                        })
                        .collect()
                })
                .collect();
            rows.push(letters.join(" "));
        }
    }
    rows
}

fn banner(st: &Style) -> String {
    let mut out = String::new();
    for (i, row) in logo_rows().iter().enumerate() {
        out += &format!("  {}\n", st.fg(lerp(VIOLET, DEEP, i as f32 / 7.0), row));
    }
    out += &format!(
        "\n  {}  {}\n  {} {}\n  {}\n",
        st.bold(WHITE, "HORIZON OFFLINE"),
        st.fg(GRAY, &format!("v{VERSION} · Minecraft 26.3")),
        st.fg(WHITE, "by"),
        st.bold(VIOLET, "RRM Network"),
        st.fg(DEEP, &"─".repeat(51)),
    );
    out
}

enum Probe<'a> {
    Skipped,
    Failed(&'a str),
    Done(&'a Report),
}

fn dot(st: &Style, c: Rgb) -> String {
    st.fg(c, "●")
}

fn status_panel(st: &Style, settings: &Settings, probe: &Probe) -> String {
    let label = |t: &str| st.fg(GRAY, &format!("{t:<10}"));
    let mut out = String::new();
    out += &format!("  {} {}\n", label("Игрок"), st.bold(WHITE, &settings.name));
    out += &format!(
        "  {} {}\n",
        label("Minecraft"),
        short_path(
            settings
                .minecraft_dir
                .as_deref()
                .unwrap_or("автопоиск · .minecraft / TLauncher"),
            58
        )
    );
    match probe {
        Probe::Skipped => {
            let text = if direct_command(settings).is_some() {
                "готов к запуску · проверка файлов: 3"
            } else {
                "проверить файлы: 3 · или нажми Играть"
            };
            out += &format!("  {} {}\n", label("Статус"), st.fg(GRAY, text));
        }
        Probe::Failed(e) => {
            out += &format!(
                "  {} {} {}\n",
                label("Статус"),
                dot(st, RED),
                st.fg(RED, e.lines().next().unwrap_or("ошибка"))
            );
        }
        Probe::Done(r) => {
            let client = if r.client_exists {
                format!("{} найден", dot(st, GREEN))
            } else if r.ready {
                format!("{} установится при запуске", dot(st, YELLOW))
            } else {
                format!("{} не найден", dot(st, RED))
            };
            let resources = if r.resources_prepared {
                format!("{} готовы", dot(st, GREEN))
            } else if r.ready {
                format!("{} подготовятся при запуске", dot(st, YELLOW))
            } else {
                format!("{} не готовы", dot(st, RED))
            };
            out += &format!(
                "  {} {client}\n  {} {resources}\n",
                label("Клиент"),
                label("Ресурсы")
            );
            if !r.home.is_empty() {
                out += &format!(
                    "  {} {}\n",
                    label("Данные"),
                    st.fg(GRAY, &short_path(&r.home, 58))
                );
            }
            for problem in &r.problems {
                out += &format!("  {} {}\n", st.fg(RED, "!"), st.fg(RED, problem));
            }
        }
    }
    out
}

fn menu_text(st: &Style, settings: &Settings) -> String {
    let key = |k: &str| st.bold(VIOLET, &format!("[{k}]"));
    format!(
        "  {} {}  {}\n\n  {} Ник игрока            {} Папка Minecraft\n  {} Проверить файлы       {} Папка игры\n  {} Установить / обновить Horizon\n  {} Импортировать настройки Minecraft\n  {} Обновления программы и лаунчера\n\n  {} Выход                {}\n",
        key("1"),
        st.bold(WHITE, "► ИГРАТЬ"),
        st.fg(GRAY, &format!("как {}", settings.name)),
        key("2"),
        key("4"),
        key("3"),
        key("5"),
        key("6"),
        key("7"),
        key("8"),
        key("0"),
        st.fg(GRAY, "Enter — играть"),
    )
}

#[cfg(windows)]
fn console_output() -> Option<fs::File> {
    use std::ffi::c_void;
    use std::os::windows::io::FromRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(id: u32) -> *mut c_void;
        fn GetFileType(handle: *mut c_void) -> u32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            sharing: u32,
            security: *mut c_void,
            creation: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
    }
    // A console may have a write-only stdout handle, which Rust's IsTerminal rejects. Open a
    // read/write handle to that same console for mode queries and clearing. Pipes/files stay plain.
    // SAFETY: all pointers are valid for the duration of these Win32 calls; File owns the new handle.
    unsafe {
        if GetFileType(GetStdHandle(-11i32 as u32)) != 2 {
            return None;
        }
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let handle = CreateFileW(
            name.as_ptr(),
            0xC0000000,
            3,
            std::ptr::null_mut(),
            3,
            0,
            std::ptr::null_mut(),
        );
        if handle.is_null() || handle as isize == -1 {
            return None;
        }
        Some(fs::File::from_raw_handle(handle))
    }
}

fn terminal_output() -> bool {
    #[cfg(windows)]
    {
        console_output().is_some()
    }
    #[cfg(not(windows))]
    {
        io::stdout().is_terminal()
    }
}

/// Redraw the console independently of whether coloured text was requested.
/// Redirected output stays a plain transcript, while an interactive terminal keeps one screen.
fn clear() {
    if !terminal_output() {
        return;
    }
    if enable_vt() {
        print!("\x1b[H\x1b[2J\x1b[3J\x1b]0;HORIZON OFFLINE — RRM Network\x07");
        let _ = io::stdout().flush();
    } else {
        clear_console_buffer();
    }
}

#[cfg(windows)]
fn clear_console_buffer() {
    use std::{ffi::c_void, os::windows::io::AsRawHandle};
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Coord {
        x: i16,
        y: i16,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Rect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }
    #[repr(C)]
    #[derive(Default)]
    struct BufferInfo {
        size: Coord,
        cursor: Coord,
        attributes: u16,
        window: Rect,
        max_size: Coord,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleScreenBufferInfo(handle: *mut c_void, info: *mut BufferInfo) -> i32;
        fn FillConsoleOutputCharacterW(
            handle: *mut c_void,
            character: u16,
            count: u32,
            start: Coord,
            written: *mut u32,
        ) -> i32;
        fn FillConsoleOutputAttribute(
            handle: *mut c_void,
            attributes: u16,
            count: u32,
            start: Coord,
            written: *mut u32,
        ) -> i32;
        fn SetConsoleCursorPosition(handle: *mut c_void, position: Coord) -> i32;
    }
    // SAFETY: the handle comes from Windows; every pointer refers to a valid local structure.
    // GetConsoleScreenBufferInfo also ensures a redirected file/pipe cannot be cleared.
    let Some(console) = console_output() else {
        return;
    };
    unsafe {
        let handle = console.as_raw_handle();
        let mut info = BufferInfo::default();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0
            || info.size.x <= 0
            || info.size.y <= 0
        {
            return;
        }
        let cells = info.size.x as u32 * info.size.y as u32;
        let mut written = 0;
        let origin = Coord::default();
        FillConsoleOutputCharacterW(handle, b' ' as u16, cells, origin, &mut written);
        FillConsoleOutputAttribute(handle, info.attributes, cells, origin, &mut written);
        SetConsoleCursorPosition(handle, origin);
    }
}

#[cfg(not(windows))]
fn clear_console_buffer() {}

#[cfg(windows)]
fn enable_vt() -> bool {
    use std::{ffi::c_void, os::windows::io::AsRawHandle};
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    // SAFETY: plain Win32 console calls with a handle the system returned.
    let Some(console) = console_output() else {
        return false;
    };
    unsafe {
        let handle = console.as_raw_handle();
        let mut mode = 0u32;
        if handle.is_null() || handle as isize == -1 || GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_vt() -> bool {
    true
}

// ------------------------------------------------------------------- menu

fn prompt(text: &str) -> Option<String> {
    print!("{text}");
    io::stdout().flush().ok()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line).ok()? == 0 {
        return None;
    }
    Some(line.trim().to_string())
}

/// `false` when input ended (pipe closed).
fn pause() -> bool {
    prompt("\n  Enter — назад в меню...").is_some()
}

fn open_folder(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!(
            "папки ещё нет: {} (появится после первого запуска)",
            path.display()
        ));
    }
    let program = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(program)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn menu(root: &Path, st: &Style, mut updates: Vec<String>) -> Result<bool, String> {
    let mut probe: Option<Result<Report, String>> = None;
    // Folder-of-exes mode checks only files, so it is always shown. In script mode the menu opens
    // at once and the slow PowerShell check runs only on request (item 3).
    let cheap = standalone(root).is_some();
    let mut checked = true;
    loop {
        let mut settings = Settings::load(root);
        if !checked || (cheap && probe.is_none()) {
            checked = true;
            if !cheap {
                println!("\n  {}", st.fg(GRAY, "Проверяю файлы…"));
            }
            probe = Some(check_options(&settings).and_then(|o| preflight(root, &o)));
            if let (false, Some(Ok(report))) = (cheap, &probe) {
                remember(&mut settings, root, report);
            }
        }
        clear();
        print!("\n{}", banner(st));
        let shown = match &probe {
            None => Probe::Skipped,
            Some(Err(e)) => Probe::Failed(e),
            Some(Ok(r)) => Probe::Done(r),
        };
        print!("{}", status_panel(st, &settings, &shown));
        for line in &updates {
            println!("  {}", st.fg(GRAY, line));
        }
        println!();
        print!("{}", menu_text(st, &settings));
        let Some(choice) = prompt("\n  Выбери [1]: ") else {
            return Ok(true);
        };
        match choice.as_str() {
            "" | "1" => {
                println!(
                    "\n  {} Запускаю Horizon Offline как {}…\n",
                    st.fg(VIOLET, "►"),
                    settings.name
                );
                let mut options = parse(["--rust".into()], false)?;
                apply_settings(&mut options, &settings);
                match start_game(root, &mut settings, &options, false) {
                    Ok(true) => println!("\n  {}", st.fg(GREEN, "Игра завершена.")),
                    Ok(false) => println!("\n  {}", st.fg(RED, "Запуск завершился с ошибкой.")),
                    Err(e) => println!("\n  {}", st.fg(RED, &e)),
                }
                probe = None;
                if !pause() {
                    return Ok(true);
                }
            }
            "2" => {
                let Some(name) = prompt("\n  Новый ник (3–16: латиница, цифры, _): ")
                else {
                    return Ok(true);
                };
                if name.is_empty() {
                    continue;
                }
                if valid_name(&name) {
                    settings.name = name;
                    if let Err(e) = settings.save(root) {
                        println!("  {}", st.fg(RED, &e));
                        if !pause() {
                            return Ok(true);
                        }
                    }
                } else {
                    println!(
                        "  {}",
                        st.fg(RED, "Ник: 3–16 символов, только латиница, цифры и _.")
                    );
                    if !pause() {
                        return Ok(true);
                    }
                }
            }
            "3" => checked = false,
            "4" => {
                println!(
                    "\n  {}",
                    st.fg(
                        GRAY,
                        "Папка, где лежит versions/26.3 (например .minecraft или папка TLauncher)."
                    )
                );
                let Some(dir) = prompt("  Путь (пусто — авто): ") else {
                    return Ok(true);
                };
                let dir = dir.trim_matches('"').to_string();
                if !dir.is_empty() && !Path::new(&dir).is_dir() {
                    println!("  {}", st.fg(RED, "Такой папки нет."));
                    if !pause() {
                        return Ok(true);
                    }
                    continue;
                }
                settings.minecraft_dir = (!dir.is_empty()).then_some(dir);
                settings.launch = None;
                if let Err(e) = settings.save(root) {
                    println!("  {}", st.fg(RED, &e));
                    if !pause() {
                        return Ok(true);
                    }
                }
                probe = None;
            }
            "5" => {
                let home = match &probe {
                    Some(Ok(r)) if !r.home.is_empty() => Ok(PathBuf::from(&r.home)),
                    _ => Err("сначала дай проверить файлы (пункт 3)".to_string()),
                };
                if let Err(e) = home.and_then(|h| open_folder(&h)) {
                    println!("  {}", st.fg(YELLOW, &e));
                    if !pause() {
                        return Ok(true);
                    }
                }
            }
            "6" => {
                match install_runtime(root) {
                    Ok(outcome) => {
                        if outcome.changed() {
                            settings.launch = None;
                            if let Err(e) = settings.save(root) {
                                println!("  {}", st.fg(RED, &e));
                            }
                            probe = None;
                            checked = false;
                        }
                        println!("\n  {}", st.fg(GREEN, &outcome.message()));
                    }
                    Err(e) => println!("\n  {}", st.fg(RED, &e)),
                }
                if !pause() {
                    return Ok(true);
                }
            }
            "7" => {
                let home = match &probe {
                    Some(Ok(report)) if !report.home.is_empty() => Ok(PathBuf::from(&report.home)),
                    _ if settings.launch.is_some() => {
                        Ok(PathBuf::from(&settings.launch.as_ref().unwrap().home))
                    }
                    _ => check_options(&settings)
                        .and_then(|o| preflight(root, &o))
                        .and_then(|r| {
                            if r.home.is_empty() {
                                Err("не найдена папка игры: сначала проверь файлы (пункт 3)".into())
                            } else {
                                Ok(PathBuf::from(r.home))
                            }
                        }),
                };
                match home {
                    Ok(home) => import_options(root, &settings, &home, true),
                    Err(e) => println!("\n  {}", st.fg(RED, &e)),
                }
                if !pause() {
                    return Ok(true);
                }
            }
            "8" => {
                let result = updater::run(root, true);
                if result.restarted {
                    return Ok(true);
                }
                updates = result.messages;
            }
            "0" | "q" | "Q" => return Ok(true),
            _ => {
                println!(
                    "  {}",
                    st.fg(YELLOW, "Введи номер 1–8 или 0. Enter — играть.")
                );
                if !pause() {
                    return Ok(true);
                }
            }
        }
    }
}

fn check(root: &Path, options: &Options, settings: &Settings, st: &Style) -> Result<bool, String> {
    let report = preflight(root, options)?;
    println!("\n{}", banner(st));
    println!("{}", status_panel(st, settings, &Probe::Done(&report)));
    if report.ready && report.client_exists && report.resources_prepared {
        println!(
            "  {}\n",
            st.bold(GREEN, "ГОТОВО: файлы найдены, игра не запускалась.")
        );
    } else if report.ready {
        println!(
            "  {}\n",
            st.bold(
                YELLOW,
                "МОЖНО ЗАПУСКАТЬ: недостающее скачается и подготовится при запуске."
            )
        );
    } else {
        println!(
            "  {}\n",
            st.bold(RED, "НЕ ГОТОВО: исправь то, что отмечено выше.")
        );
    }
    Ok(report.ready)
}

fn run() -> Result<bool, String> {
    let options = parse(
        std::env::args_os().skip(1),
        io::stdin().is_terminal() && terminal_output(),
    )?;
    let st = Style {
        color: terminal_output() && std::env::var_os("NO_COLOR").is_none() && enable_vt(),
    };
    if options.version {
        println!("horizon-offline {VERSION}");
        return Ok(true);
    }
    if options.licenses {
        print!("{}", include_str!("../assets/THIRD_PARTY_LICENSES.txt"));
        return Ok(true);
    }
    if options.help {
        println!(
            "HORIZON OFFLINE {VERSION} · by RRM Network\n\n  --rust          Start the native Rust client\n  --plan          Native preflight; no game\n  --check         Check files; no game\n  --menu          Terminal menu (default in a terminal)\n  --updates-check Check program versions without installing\n  --no-update     Skip automatic updates for this run\n  --version       Print program version\n  --licenses      Show all license notices\n\nNickname and Minecraft folder are saved in {SETTINGS_FILE} beside the launcher.\nOnly the native Rust client is started. Fabric/Java mod JARs are not executed."
        );
        return Ok(true);
    }
    let root = root()?;
    if options.updates_check
        && (options.no_update || std::env::var_os("HORIZON_OFFLINE_NO_UPDATE").is_some())
    {
        println!("  Проверка обновлений отключена.");
        return Ok(true);
    }
    let mut updates = Vec::new();
    if !options.no_update
        && !options.check
        && !options.plan
        && std::env::var_os("HORIZON_OFFLINE_NO_UPDATE").is_none()
    {
        if options.menu {
            ui::intro(&st);
        }
        let result = updater::run(&root, !options.updates_check);
        if result.restarted {
            return Ok(true);
        }
        updates = result.messages;
        if options.updates_check {
            for line in updates {
                println!("  {line}");
            }
            return Ok(true);
        }
        if result.connected && standalone(&root).is_some_and(|s| s.installed().is_some()) {
            match install_runtime(&root) {
                Ok(outcome) => {
                    if outcome.changed() {
                        let mut settings = Settings::load(&root);
                        settings.launch = None;
                        settings.save(&root)?;
                    }
                    updates.push(outcome.message());
                }
                Err(error) => {
                    updates.push(format!("Клиент Horizon: проверка недоступна · {error}"))
                }
            }
        }
    }
    let recovery_command = standalone(&root)
        .map(|s| s.command())
        .unwrap_or_else(|| Command::new("horizon-launcher.exe"));
    incremental::recover_pending(&recovery_command)?;
    if options.menu {
        return menu(&root, &st, updates);
    }
    let mut settings = Settings::load(&root);
    let mut options = options;
    let custom_args = !options.forwarded.is_empty() || options.plan;
    apply_settings(&mut options, &settings);
    if options.check || (options.plan && standalone(&root).is_some()) {
        check(&root, &options, &settings, &st)
    } else {
        start_game(&root, &mut settings, &options, custom_args)
    }
}

const LOG_FILE: &str = "horizon-offline.log";

/// Where a failure goes besides the screen: a log beside the launcher (best effort).
fn log_failure(message: &str) {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    else {
        return;
    };
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG_FILE))
    {
        let _ = writeln!(file, "[unix {seconds}] {message}");
    }
}

/// Shows what went wrong and, in a console window opened by double-click, waits so it can be read.
fn fail(message: Option<&str>) {
    if let Some(message) = message {
        eprintln!("\n  Horizon Offline: {message}");
        log_failure(message);
    }
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        let _ = prompt("\n  Enter — закрыть...");
    }
}

fn main() -> ExitCode {
    let outcome = std::panic::catch_unwind(run).unwrap_or_else(|panic| {
        let text = panic
            .downcast_ref::<&str>()
            .map(ToString::to_string)
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "неизвестная ошибка".into());
        Err(format!("внутренняя ошибка: {text}"))
    });
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            fail(None);
            ExitCode::FAILURE
        }
        Err(e) => {
            fail(Some(&e));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn simulate_update(
        before: &[&str],
        releases: Result<Value, String>,
        after: &[&str],
        install_result: Result<(), String>,
    ) -> (Result<InstallOutcome, String>, Vec<String>, usize) {
        let mut replies = std::collections::VecDeque::from([
            Ok(json!({ "installed": before })),
            releases,
            Ok(json!({ "installed": after })),
        ]);
        let mut queries = 0;
        let mut installs = Vec::new();
        let result = update_client(
            |args| {
                let expected: &[&str] = if queries == 1 {
                    &["versions", "--json"]
                } else {
                    &["versions", "--installed", "--json"]
                };
                assert_eq!(args, expected);
                queries += 1;
                replies.pop_front().unwrap()
            },
            |tag, _| {
                installs.push(tag.into());
                install_result
            },
        );
        (result, installs, queries)
    }

    fn release(tag: &str, installed: bool) -> Value {
        json!({ "tag": tag, "installed": installed, "available_here": true, "prerelease": false })
    }

    #[test]
    fn current_version_is_reported_without_installing_or_invalidating_paths() {
        let (result, installs, queries) = simulate_update(
            &["nightly-current"],
            Ok(json!([release("nightly-current", true)])),
            &[],
            Ok(()),
        );
        let outcome = result.unwrap();
        assert_eq!(
            outcome,
            InstallOutcome::AlreadyCurrent("nightly-current".into())
        );
        assert!(!outcome.changed());
        assert!(outcome.message().contains("Уже обновлено"));
        assert!(installs.is_empty());
        assert_eq!(queries, 2);
    }

    #[test]
    fn first_install_and_update_report_the_exact_checked_version() {
        for before in [vec![], vec!["retired-build"]] {
            let (result, installs, queries) = simulate_update(
                &before,
                Ok(json!([release("nightly-new", false)])),
                &["nightly-new"],
                Ok(()),
            );
            let expected = if before.is_empty() {
                InstallOutcome::Installed("nightly-new".into())
            } else {
                InstallOutcome::Updated("nightly-new".into())
            };
            let outcome = result.unwrap();
            assert_eq!(outcome, expected);
            assert!(outcome.changed());
            assert_eq!(installs, ["nightly-new"]);
            assert_eq!(queries, 3);
        }
    }

    #[test]
    fn updates_follow_the_official_channel_and_platform() {
        let mut archived = release("nightly-archived", false);
        archived["prerelease"] = json!(true);
        let mut unsupported = release("other-platform", false);
        unsupported["available_here"] = json!(false);
        let (result, installs, _) = simulate_update(
            &["v0.2.1"],
            Ok(json!([
                unsupported,
                archived,
                release("nightly-current", false),
                release("v0.2.1", true)
            ])),
            &["nightly-current"],
            Ok(()),
        );
        assert_eq!(
            result.unwrap(),
            InstallOutcome::Updated("nightly-current".into())
        );
        assert_eq!(installs, ["nightly-current"]);
    }

    #[test]
    fn missing_local_version_is_not_reported_as_current() {
        let (result, installs, _) = simulate_update(
            &[],
            Ok(json!([release("nightly-current", true)])),
            &["nightly-current"],
            Ok(()),
        );
        assert_eq!(
            result.unwrap(),
            InstallOutcome::Installed("nightly-current".into())
        );
        assert_eq!(installs, ["nightly-current"]);
    }

    #[test]
    fn network_empty_and_malformed_replies_never_report_success_or_install() {
        for releases in [
            Err("offline".into()),
            Ok(json!([])),
            Ok(json!({})),
            Ok(json!([{"tag": "bad"}])),
        ] {
            let (result, installs, queries) = simulate_update(&["old"], releases, &[], Ok(()));
            assert!(result.is_err());
            assert!(installs.is_empty());
            assert_eq!(queries, 2);
        }
        assert!(installed_tags(json!({})).is_err());
    }

    #[test]
    fn failed_or_unconfirmed_installs_are_errors() {
        let (result, installs, queries) = simulate_update(
            &["old"],
            Ok(json!([release("new", false)])),
            &["new"],
            Err("download failed".into()),
        );
        assert_eq!(result.unwrap_err(), "download failed");
        assert_eq!(installs, ["new"]);
        assert_eq!(queries, 2);
        let (result, _, queries) = simulate_update(
            &["old"],
            Ok(json!([release("new", false)])),
            &["old"],
            Ok(()),
        );
        assert!(result.unwrap_err().contains("не подтверждена"));
        assert_eq!(queries, 3);
    }

    #[cfg(windows)]
    #[test]
    fn official_json_drains_pipes_and_rejects_errors_bad_json_and_timeouts() {
        let query = |script: &str, timeout| {
            official_json(
                Command::new("powershell.exe").args(["-NoProfile", "-Command", script]),
                timeout,
            )
        };
        let timeout = Duration::from_secs(10);
        let large = query("[Console]::Error.Write(('e' * 100000)); [Console]::Write(('x' * 100000 | ConvertTo-Json -Compress))", timeout).unwrap();
        assert_eq!(large.as_str().unwrap().len(), 100000);
        assert!(
            query("[Console]::Error.Write('offline'); exit 7", timeout)
                .unwrap_err()
                .contains("offline")
        );
        assert!(query("[Console]::Write('bad json')", timeout).is_err());
        let started = Instant::now();
        assert!(query("Start-Sleep -Seconds 30", Duration::from_millis(200)).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn java_routes_are_rejected_before_any_process() {
        for flag in ["--java", "--fabric"] {
            assert!(parse([OsString::from(flag)], false).is_err());
        }
    }

    #[test]
    fn planning_stays_native_and_noninteractive() {
        let p = parse(
            ["--rust", "--plan", "-Name", "Player"].map(OsString::from),
            true,
        )
        .unwrap();
        assert!(p.plan && !p.menu);
        assert_eq!(p.forwarded, ["-Name", "Player"].map(OsString::from));
        let args: Vec<_> = command(Path::new("C:/package"), &p)
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|a| a.ends_with("Start-HorizonFabric.ps1")));
        assert!(!args.iter().any(|a| a.contains("FabricJava")));
    }

    #[test]
    fn menu_is_only_default_for_a_terminal() {
        assert!(parse([], true).unwrap().menu);
        assert!(!parse([], false).unwrap().menu);
    }

    #[test]
    fn nicknames_follow_the_client_rule() {
        for ok in ["Player", "Ab_1", "a".repeat(16).as_str()] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "ab", "has space", "ник", "x".repeat(17).as_str(), "a-b"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn logo_letters_have_even_rows() {
        for letter in "HORIZONFL E".chars().filter(|c| *c != ' ') {
            assert!(
                pixel_glyph(letter)
                    .iter()
                    .all(|row| row.len() == 5 && row.bytes().all(|b| b == b'0' || b == b'1'))
            );
        }
        let rows = logo_rows();
        assert_eq!(rows.len(), 6);
        assert!(rows.iter().all(|r| r.chars().count() == 41));
    }

    #[test]
    fn plain_output_has_no_escape_codes_and_keeps_labels() {
        let st = Style { color: false };
        let text = format!("{}{}", banner(&st), menu_text(&st, &Settings::default()));
        assert!(!text.contains('\x1b'));
        assert!(text.contains("HORIZON OFFLINE") && text.contains("by RRM Network"));
        assert!(text.contains("[7]") && text.contains("Импортировать настройки Minecraft"));
        assert!(!text.contains("Fabric Java"));
    }

    #[test]
    fn credits_use_white_by_and_violet_rrm_and_long_paths_fit_the_menu() {
        let st = Style { color: true };
        let text = banner(&st);
        assert!(text.contains(&st.fg(WHITE, "by")));
        assert!(text.contains(&st.bold(VIOLET, "RRM Network")));
        assert_eq!(short_path(&"П".repeat(150), 58).chars().count(), 58);
        let plain = format!(
            "{}{}{}",
            banner(&Style { color: false }),
            status_panel(
                &Style { color: false },
                &Settings::default(),
                &Probe::Skipped
            ),
            menu_text(&Style { color: false }, &Settings::default())
        );
        assert!(plain.lines().count() <= 30);
        assert!(plain.lines().all(|line| line.chars().count() <= 78));
    }

    #[test]
    fn saved_settings_are_forwarded_unless_given_explicitly() {
        let settings = Settings {
            name: "Steve_1".into(),
            minecraft_dir: Some("D:/Games/.minecraft".into()),
            launch: None,
        };
        let mut o = parse(["--rust".into()], false).unwrap();
        apply_settings(&mut o, &settings);
        assert_eq!(
            o.forwarded,
            ["-Name", "Steve_1", "-MinecraftDir", "D:/Games/.minecraft"].map(OsString::from)
        );
        let mut o = parse(["--rust", "-name", "Alex"].map(OsString::from), false).unwrap();
        apply_settings(&mut o, &settings);
        assert_eq!(
            o.forwarded
                .iter()
                .filter(|a| a.to_str() == Some("-Name"))
                .count(),
            0
        );
        assert!(has_flag(&o.forwarded, "-name") && o.forwarded.contains(&OsString::from("Alex")));
    }

    #[test]
    fn settings_round_trip_and_bad_files_fall_back() {
        let dir = std::env::temp_dir().join(format!("horizon-rust-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        let saved = Settings {
            name: "Alex_9".into(),
            minecraft_dir: Some("E:/mc".into()),
            launch: Some(Launch {
                client: "c.exe".into(),
                resources: "r".into(),
                home: "h".into(),
            }),
        };
        saved.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), saved);
        fs::write(dir.join(SETTINGS_FILE), "{ not json").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        fs::write(dir.join(SETTINGS_FILE), r#"{"name":"bad name!"}"#).unwrap();
        assert_eq!(Settings::load(&dir).name, "Player");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn direct_launch_needs_existing_files_and_matches_the_script_environment() {
        let dir = std::env::temp_dir().join(format!("horizon-rust-direct-{}", std::process::id()));
        let resources = dir.join("res");
        fs::create_dir_all(&resources).unwrap();
        fs::write(resources.join("horizon-resources.json"), "{}").unwrap();
        let client = dir.join("horizon-client.exe");
        fs::write(&client, "").unwrap();
        let home = dir.join("home");
        let launch = Launch {
            client: client.to_string_lossy().into_owned(),
            resources: resources.to_string_lossy().into_owned(),
            home: home.to_string_lossy().into_owned(),
        };
        let mut settings = Settings::default();
        assert!(direct_command(&settings).is_none(), "no remembered paths");
        settings.launch = Some(launch.clone());
        let command = direct_command(&settings).expect("files exist");
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["--resources", &launch.resources, "--name", "Player"]);
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&(OsStr::new("HORIZON_HOME"), Some(OsStr::new(&launch.home)))));
        assert!(envs.contains(&(OsStr::new("HORIZON_SESSION_FILE"), None)));
        fs::remove_file(&client).unwrap();
        assert!(
            direct_command(&settings).is_none(),
            "stale client path falls back to the script"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn official_launcher_is_found_beside_us_or_in_bin() {
        let dir =
            std::env::temp_dir().join(format!("horizon-rust-official-{}", std::process::id()));
        fs::create_dir_all(dir.join("bin")).unwrap();
        assert!(official_launcher(&dir).is_none());
        fs::write(dir.join("bin/horizon-launcher.exe"), "").unwrap();
        assert_eq!(
            official_launcher(&dir),
            Some(dir.join("bin/horizon-launcher.exe"))
        );
        assert!(install_runtime(&dir.join("missing")).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn folder_of_exes_mode_downloads_prepares_and_launches_offline() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("horizon-rust-folder-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // A stand-in for the original launcher: `install` makes a version folder with a client,
        // `prepare` writes the resources manifest, and both record what they were given.
        let script = format!(
            "#!/bin/sh\necho \"$1 $2 $3 home=$HORIZON_LAUNCHER_HOME game=$HORIZON_HOME noupdate=$HORIZON_LAUNCHER_NO_SELF_UPDATE\" >> '{log}'\nV=\"$HORIZON_LAUNCHER_HOME/versions/nightly-test\"\ncase \"$1\" in\n versions) if [ \"$2\" = --installed ]; then if [ -f \"$V/.installed\" ]; then echo '{{\"installed\":[\"nightly-test\"]}}'; else echo '{{\"installed\":[]}}'; fi; else echo '[{{\"tag\":\"nightly-test\",\"available_here\":true,\"installed\":false,\"prerelease\":false}}]'; fi;;\n install) mkdir -p \"$V\"; printf '#!/bin/sh\\n' > \"$V/{client}\"; chmod +x \"$V/{client}\"; touch \"$V/.installed\";;\n prepare) mkdir -p \"$V/.local/horizon-client/26.3\"; echo '{{}}' > \"$V/.local/horizon-client/26.3/{manifest}\";;\nesac\n",
            log = dir.join("calls.log").display(),
            client = CLIENT_EXE,
            manifest = RESOURCES_MANIFEST,
        );
        let official = dir.join("horizon-launcher.exe");
        fs::write(&official, script).unwrap();
        fs::set_permissions(&official, fs::Permissions::from_mode(0o755)).unwrap();

        let folder = standalone(&dir).expect("no scripts/ and an original launcher: folder mode");
        let before = folder.report();
        assert!(before.ready && !before.client_exists && !before.resources_prepared);

        let settings = Settings {
            minecraft_dir: Some("D:/mc".into()),
            ..Settings::default()
        };
        let launch = folder.ensure(&settings).expect("installs and prepares");
        assert!(launch.client.ends_with(CLIENT_EXE));
        assert!(launch.resources.contains("horizon-client"));
        assert!(launch.home.ends_with("game") && launch.home.contains("Horizon Offline Data"));
        assert!(launch_command(&launch, "Player").is_some());
        let after = folder.report();
        assert!(after.client_exists && after.resources_prepared);

        let calls = fs::read_to_string(dir.join("calls.log")).unwrap();
        assert!(calls.contains("install nightly-test"), "{calls}");
        assert!(calls.contains("prepare --minecraft D:/mc"), "{calls}");
        assert!(
            calls.contains("noupdate=1") && calls.contains("Horizon Offline Data"),
            "{calls}"
        );

        // Second start: nothing is downloaded or prepared again.
        folder.ensure(&settings).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("calls.log"))
                .unwrap()
                .lines()
                .count(),
            5
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_portable_folder_does_not_pick_up_an_unrelated_parent_package() {
        let folder =
            std::env::temp_dir().join(format!("horizon-portable-root-{}", std::process::id()));
        let portable = folder.join("portable");
        fs::create_dir_all(folder.join("scripts")).unwrap();
        fs::create_dir_all(&portable).unwrap();
        fs::write(folder.join(SCRIPT), "fixture").unwrap();
        fs::write(portable.join("horizon-launcher.exe"), "fixture").unwrap();
        let exe = portable.join("horizon-offline.exe");
        assert_eq!(root_for(&exe, false).unwrap(), portable);
        fs::remove_file(portable.join("horizon-launcher.exe")).unwrap();
        assert_eq!(root_for(&exe, false).unwrap(), folder);
        if folder.parent() == Some(std::env::temp_dir().as_path()) {
            fs::remove_dir_all(folder).unwrap();
        }
    }

    #[test]
    fn script_packages_keep_the_script_mode() {
        let dir = std::env::temp_dir().join(format!("horizon-rust-scripts-{}", std::process::id()));
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("scripts/Start-HorizonFabric.ps1"), "").unwrap();
        fs::write(dir.join("horizon-launcher.exe"), "").unwrap();
        assert!(standalone(&dir).is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_a_fully_ready_install_is_remembered() {
        let dir =
            std::env::temp_dir().join(format!("horizon-rust-remember-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut settings = Settings::default();
        let mut report = Report {
            ready: true,
            client_exists: true,
            resources_prepared: true,
            client: "c.exe".into(),
            resources: "r".into(),
            home: "h".into(),
            problems: vec![],
        };
        remember(&mut settings, &dir, &report);
        assert!(settings.launch.is_some());
        assert_eq!(Settings::load(&dir).launch, settings.launch);
        report.resources_prepared = false;
        remember(&mut settings, &dir, &report);
        assert!(settings.launch.is_none());
        assert!(Settings::load(&dir).launch.is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn status_panel_reports_each_state() {
        let st = Style { color: false };
        let s = Settings::default();
        let mut r = Report {
            ready: true,
            client_exists: true,
            resources_prepared: false,
            client: "c.exe".into(),
            resources: "r".into(),
            home: "H:/home".into(),
            problems: vec!["нет Java".into()],
        };
        let text = status_panel(&st, &s, &Probe::Done(&r));
        assert!(
            text.contains("подготовятся") && text.contains("нет Java") && text.contains("H:/home")
        );
        r.resources_prepared = true;
        assert!(status_panel(&st, &s, &Probe::Done(&r)).contains("готовы"));
        r.ready = false;
        r.client_exists = false;
        assert!(status_panel(&st, &s, &Probe::Done(&r)).contains("не найден"));
        assert!(status_panel(&st, &s, &Probe::Failed("boom\nmore")).contains("boom"));
    }
}
