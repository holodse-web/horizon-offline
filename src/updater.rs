//! Two independent GitHub update channels. Only verified executable payloads are installed;
//! game data and settings are outside this module's write set.
use crate::ui::Task;
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    digest::{Context, SHA256},
    signature::{ED25519, UnparsedPublicKey},
};
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const OUR_REPO: &str = "holodse-web/horizon-offline";
const OFFICIAL_REPO: &str = "horizon-rust/horizon";
const OFFICIAL_ASSET: &str = "horizon-launcher-windows-x64.zip";
// Horizon's public Ed25519 release key, from crates/horizon-release-sign/release-keys.txt.
const OFFICIAL_KEY: &str = "MU4oiIQcCxkZvbwSjjXs7SySCuM6ywRQ3nGaJYZG0gw=";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u64, u64, u64);

impl Version {
    fn parse(text: &str) -> Option<Self> {
        let text = text
            .strip_prefix("launcher@")
            .or_else(|| text.strip_prefix("launcher-v"))
            .or_else(|| text.strip_prefix('v'))
            .unwrap_or(text);
        let parts = text
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        (parts.len() == 3).then(|| Self(parts[0], parts[1], parts[2]))
    }
    fn text(self) -> String {
        format!("{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

impl Release {
    fn asset(&self, name: &str) -> Result<&Asset, String> {
        self.assets
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| format!("{}: отсутствует {name}", self.tag_name))
    }
}

#[derive(Clone)]
struct Http {
    agent: ureq::Agent,
    api: String,
    fixture: bool,
}

impl Http {
    fn new() -> Self {
        let mut api = "https://api.github.com".to_string();
        let mut fixture = false;
        // A loopback-only test server is available in development builds, never in releases.
        if cfg!(debug_assertions) {
            if let Ok(value) = std::env::var("HORIZON_OFFLINE_TEST_API") {
                if value.starts_with("http://127.0.0.1:") {
                    api = value.trim_end_matches('/').into();
                    fixture = true;
                }
            }
        }
        Self {
            api,
            fixture,
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(15))
                .user_agent(concat!("Horizon-Offline/", env!("CARGO_PKG_VERSION")))
                .build(),
        }
    }

    fn bytes(&self, url: &str, limit: u64, timeout: u64) -> Result<Vec<u8>, String> {
        let response = self
            .agent
            .get(url)
            .timeout(Duration::from_secs(timeout))
            .set("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| format!("GitHub: {e}"))?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > limit {
            return Err("Ответ GitHub слишком большой".into());
        }
        Ok(bytes)
    }

    fn release_list(&self, repo: &str, latest: bool) -> Result<Vec<Release>, String> {
        let suffix = if latest {
            "/releases/latest"
        } else {
            "/releases?per_page=100"
        };
        let bytes = self.bytes(
            &format!("{}/repos/{repo}{suffix}", self.api),
            4 * 1024 * 1024,
            12,
        )?;
        if latest {
            serde_json::from_slice(&bytes).map(|r| vec![r])
        } else {
            serde_json::from_slice(&bytes)
        }
        .map_err(|e| format!("Неверный ответ GitHub: {e}"))
    }

    fn validate_url(&self, repo: &str, asset: &Asset) -> Result<(), String> {
        if asset
            .browser_download_url
            .starts_with(&format!("https://github.com/{repo}/releases/download/"))
            || (self.fixture
                && asset
                    .browser_download_url
                    .starts_with(&(self.api.clone() + "/")))
        {
            Ok(())
        } else {
            Err("Релиз содержит посторонний адрес загрузки".into())
        }
    }

    fn signed_hash(&self, release: &Release, name: &str) -> Result<String, String> {
        let sums = release.asset("SHA256SUMS")?;
        let sig = release.asset("SHA256SUMS.sig")?;
        self.validate_url(OFFICIAL_REPO, sums)?;
        self.validate_url(OFFICIAL_REPO, sig)?;
        let sums = self.bytes(&sums.browser_download_url, 1024 * 1024, 15)?;
        let sig = self.bytes(&sig.browser_download_url, 16384, 15)?;
        verify_signed_hash(&release.tag_name, name, &sums, &sig, OFFICIAL_KEY)
    }

    fn download(
        &self,
        repo: &str,
        asset: &Asset,
        signed: Option<&str>,
        target: &Path,
        task: &Task,
    ) -> Result<(), String> {
        self.validate_url(repo, asset)?;
        let expected = asset
            .digest
            .as_deref()
            .and_then(|s| s.strip_prefix("sha256:"))
            .filter(|s| valid_hash(s))
            .ok_or("У релиза нет корректной контрольной суммы SHA-256")?;
        if signed.is_some_and(|hash| !hash.eq_ignore_ascii_case(expected)) {
            return Err("Подписанная контрольная сумма не совпала с GitHub".into());
        }
        if asset.size == 0 || asset.size > 256 * 1024 * 1024 {
            return Err("Некорректный размер обновления".into());
        }
        let response = self
            .agent
            .get(&asset.browser_download_url)
            .timeout(Duration::from_secs(600))
            .call()
            .map_err(|e| format!("Не удалось скачать обновление: {e}"))?;
        let mut reader = response.into_reader();
        let mut file = File::create(target).map_err(|e| e.to_string())?;
        let mut hash = Context::new(&SHA256);
        let mut total = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > asset.size {
                return Err("Размер скачанного обновления не совпал".into());
            }
            file.write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
            hash.update(&buffer[..count]);
            task.progress(total, asset.size);
        }
        file.sync_all().map_err(|e| e.to_string())?;
        if total != asset.size || !hex(hash.finish().as_ref()).eq_ignore_ascii_case(expected) {
            return Err("Обновление повреждено: размер или SHA-256 не совпали".into());
        }
        Ok(())
    }
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn verify_signed_hash(
    tag: &str,
    name: &str,
    sums: &[u8],
    signatures: &[u8],
    key: &str,
) -> Result<String, String> {
    let key = STANDARD.decode(key).map_err(|e| e.to_string())?;
    let mut message = format!("horizon release {tag}\n").into_bytes();
    message.extend_from_slice(sums);
    let verified = String::from_utf8_lossy(signatures)
        .lines()
        .filter_map(|line| STANDARD.decode(line.trim()).ok())
        .any(|sig| {
            UnparsedPublicKey::new(&ED25519, &key)
                .verify(&message, &sig)
                .is_ok()
        });
    if !verified {
        return Err("Подпись Horizon не прошла проверку; обновление не установлено".into());
    }
    std::str::from_utf8(sums)
        .map_err(|e| e.to_string())?
        .lines()
        .find_map(|line| {
            let (hash, file) = line.split_once(' ')?;
            let file = file.strip_prefix([' ', '*']).unwrap_or(file);
            (file == name && valid_hash(hash)).then(|| hash.to_ascii_lowercase())
        })
        .ok_or_else(|| format!("В подписанном списке нет {name}"))
}

fn newest(releases: Vec<Release>, official: bool) -> Option<(Version, Release)> {
    releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter(|r| {
            !official || r.tag_name.starts_with("launcher@") || r.tag_name.starts_with("launcher-v")
        })
        .filter_map(|r| Some((Version::parse(&r.tag_name)?, r)))
        .filter(|(v, r)| {
            if official {
                r.asset(OFFICIAL_ASSET).is_ok()
            } else {
                r.asset(&format!("Horizon-Offline-{}.zip", v.text()))
                    .is_ok()
            }
        })
        .max_by_key(|(v, _)| *v)
}

fn binary_version(path: &Path, name: &str, work: &Path) -> Result<Version, String> {
    let output = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .env("HORIZON_LAUNCHER_HOME", work.join("launcher"))
        .env("HORIZON_HOME", work.join("game"))
        .env("HORIZON_LAUNCHER_NO_SELF_UPDATE", "1")
        .env("HORIZON_OFFLINE_NO_UPDATE", "1")
        .output()
        .map_err(|e| format!("Не удалось узнать версию {name}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{name} не сообщил версию"));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .strip_prefix(name)
        .and_then(|v| Version::parse(v.trim()))
        .ok_or_else(|| format!("{name}: неизвестный формат версии"))
}

fn unpack_exe(archive: &Path, filename: &str, target: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let matches: Vec<_> = (0..zip.len())
        .filter(|i| {
            zip.by_index(*i).ok().is_some_and(|f| {
                f.enclosed_name()
                    .is_some_and(|p| p.file_name().is_some_and(|n| n == filename))
                    && !f.is_dir()
            })
        })
        .collect();
    if matches.len() != 1 {
        return Err(format!("Архив должен содержать один {filename}"));
    }
    let mut entry = zip.by_index(matches[0]).map_err(|e| e.to_string())?;
    if entry.size() > 128 * 1024 * 1024 || entry.size() < 2 {
        return Err("Некорректный размер EXE".into());
    }
    let mut file = File::create(target).map_err(|e| e.to_string())?;
    std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let mut magic = [0u8; 2];
    File::open(target)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map_err(|e| e.to_string())?;
    if magic != *b"MZ" {
        return Err("Обновление не является Windows EXE".into());
    }
    Ok(())
}

struct Lock {
    file: Option<File>,
    path: PathBuf,
}
impl Lock {
    fn take(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let path = root.join("update.lock");
        let mut options = OpenOptions::new();
        options.write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.create(true).truncate(false).share_mode(0);
        }
        #[cfg(not(windows))]
        options.create_new(true);
        let file = options
            .open(&path)
            .map_err(|_| "Обновления уже проверяются в другом окне".to_string())?;
        Ok(Self {
            file: Some(file),
            path,
        })
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

pub(super) struct Outcome {
    pub messages: Vec<String>,
    pub restarted: bool,
    pub connected: bool,
}

fn install(
    http: &Http,
    release: &Release,
    version: Version,
    official: bool,
    exe: &Path,
    work: &Path,
) -> Result<(), String> {
    fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let (repo, binary, asset_name, title) = if official {
        (
            OFFICIAL_REPO,
            "horizon-launcher.exe",
            OFFICIAL_ASSET.to_string(),
            "Лаунчер Horizon",
        )
    } else {
        (
            OUR_REPO,
            "horizon-offline.exe",
            format!("Horizon-Offline-{}.zip", version.text()),
            "Horizon Offline",
        )
    };
    let asset = release.asset(&asset_name)?;
    let task = Task::new(&format!("{title} {} · скачивание", version.text()));
    let signed = if official {
        Some(http.signed_hash(release, &asset_name)?)
    } else {
        None
    };
    let archive = work.join("download.zip");
    http.download(repo, asset, signed.as_deref(), &archive, &task)?;
    task.finish();
    let task = Task::new(&format!("{title} · проверка и установка"));
    let staged = work.join(binary);
    unpack_exe(&archive, binary, &staged)?;
    let command_name = binary.trim_end_matches(".exe");
    if binary_version(&staged, command_name, work)? != version {
        return Err("Версия скачанного EXE не совпала с релизом".into());
    }
    let backup = work
        .parent()
        .ok_or("Нет папки резервных копий")?
        .join(format!("{binary}.previous"));
    fs::copy(exe, &backup).map_err(|e| format!("Не удалось сохранить предыдущую версию: {e}"))?;
    if official {
        // The official launcher is not running here. Rename keeps the replacement atomic.
        fs::rename(&staged, exe)
            .map_err(|e| format!("Не удалось заменить Horizon; прежний файл сохранён: {e}"))?;
    } else {
        self_replace::self_replace(&staged)
            .map_err(|e| format!("Не удалось заменить программу: {e}"))?;
    }
    task.finish();
    Ok(())
}

/// All network failures are nonfatal: an existing prepared game remains available offline.
pub(super) fn run(root: &Path, apply: bool) -> Outcome {
    let mut outcome = Outcome {
        messages: Vec::new(),
        restarted: false,
        connected: false,
    };
    let work = root.join(".horizon-updates");
    let restart_exe = std::env::current_exe().ok();
    let lock = match Lock::take(&work) {
        Ok(lock) => lock,
        Err(error) => {
            outcome.messages.push(error);
            return outcome;
        }
    };
    let http = Http::new();
    let task = Task::new("Проверка обновлений · Horizon Offline / Horizon Launcher");
    let (ours, official) = std::thread::scope(|scope| {
        let ours = scope.spawn(|| http.release_list(OUR_REPO, true));
        let official = scope.spawn(|| http.release_list(OFFICIAL_REPO, false));
        (
            ours.join()
                .unwrap_or_else(|_| Err("Ошибка проверки программы".into())),
            official
                .join()
                .unwrap_or_else(|_| Err("Ошибка проверки Horizon".into())),
        )
    });
    outcome.connected = ours.is_ok() || official.is_ok();
    if outcome.connected {
        task.finish();
    } else {
        drop(task);
    }
    let mut restart = false;
    // Install the official launcher first; a wrapper restart then uses the new original binary.
    for (releases, official) in [(official, true), (ours, false)] {
        let title = if official {
            "Лаунчер Horizon"
        } else {
            "Horizon Offline"
        };
        let result = (|| -> Result<String, String> {
            let releases = releases?;
            let (version, release) = newest(releases, official)
                .ok_or("Нет подходящего стабильного релиза Windows x64")?;
            let exe = if official {
                crate::official_launcher(root).ok_or("horizon-launcher.exe не найден")?
            } else {
                std::env::current_exe().map_err(|e| e.to_string())?
            };
            let current = if official {
                binary_version(&exe, "horizon-launcher", &work)?
            } else {
                Version::parse(crate::VERSION).ok_or("Некорректная версия программы")?
            };
            if version <= current {
                return Ok(format!("{title} {} · уже обновлено", current.text()));
            }
            if !apply {
                return Ok(format!(
                    "{title} {} → {} · доступно обновление",
                    current.text(),
                    version.text()
                ));
            }
            install(
                &http,
                &release,
                version,
                official,
                &exe,
                &work.join(if official { "official" } else { "offline" }),
            )?;
            if !official {
                restart = true;
            }
            Ok(format!("{title} · обновлено до {}", version.text()))
        })();
        outcome.messages.push(result.unwrap_or_else(|e| {
            crate::log_failure(&format!("{title}: {e}"));
            format!("{title} · обновление недоступно · можно играть офлайн · повтор: 8")
        }));
    }
    drop(lock);
    if restart {
        let result = restart_exe
            .as_ref()
            .ok_or("Не найден путь новой программы".to_string())
            .and_then(|exe| {
                Command::new(exe)
                    .args(std::env::args_os().skip(1))
                    .spawn()
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            });
        match result {
            Ok(()) => {
                println!("\n  ✓ Horizon Offline обновлён. Перезапускаю…");
                outcome.restarted = true;
            }
            Err(error) => {
                let backup = work.join("horizon-offline.exe.previous");
                let temporary = work.join("restore.exe");
                let restored = restart_exe.as_ref().is_some_and(|exe| {
                    fs::copy(&backup, &temporary)
                        .and_then(|_| fs::rename(&temporary, exe))
                        .is_ok()
                });
                outcome.messages.push(format!(
                    "Перезапуск не удался: {error}. {}",
                    if restored {
                        "Предыдущая версия восстановлена."
                    } else {
                        "Резервная копия — .horizon-updates/horizon-offline.exe.previous."
                    }
                ));
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    #[test]
    fn versions_are_numeric_and_never_treat_prereleases_as_stable() {
        assert!(Version::parse("v0.10.0") > Version::parse("v0.9.9"));
        assert_eq!(Version::parse("launcher@0.2.28"), Some(Version(0, 2, 28)));
        for bad in ["v1.2", "v1.2.3-beta", "nightly", "../1.2.3", "1.2.3.4"] {
            assert!(Version::parse(bad).is_none());
        }
    }

    #[test]
    fn official_selection_excludes_game_and_prereleases() {
        let release = |tag: &str, prerelease| Release {
            tag_name: tag.into(),
            draft: false,
            prerelease,
            assets: vec![Asset {
                name: OFFICIAL_ASSET.into(),
                browser_download_url: "".into(),
                size: 1,
                digest: None,
            }],
        };
        let selected = newest(
            vec![
                release("v99.0.0", false),
                release("launcher@0.2.99", true),
                release("launcher@0.2.28", false),
            ],
            true,
        )
        .unwrap();
        assert_eq!(selected.0, Version(0, 2, 28));
    }

    #[test]
    fn signature_binds_the_tag_and_every_checksum_byte() {
        let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let public = STANDARD.encode(key.public_key().as_ref());
        let sums = format!("{}  archive.zip\n", "a".repeat(64));
        let signature = STANDARD.encode(
            key.sign(
                &[
                    b"horizon release launcher@1.2.3\n".as_slice(),
                    sums.as_bytes(),
                ]
                .concat(),
            )
            .as_ref(),
        );
        assert_eq!(
            verify_signed_hash(
                "launcher@1.2.3",
                "archive.zip",
                sums.as_bytes(),
                signature.as_bytes(),
                &public
            )
            .unwrap(),
            "a".repeat(64)
        );
        assert!(
            verify_signed_hash(
                "launcher@1.2.4",
                "archive.zip",
                sums.as_bytes(),
                signature.as_bytes(),
                &public
            )
            .is_err()
        );
        assert!(
            verify_signed_hash(
                "launcher@1.2.3",
                "archive.zip",
                b"tampered",
                signature.as_bytes(),
                &public
            )
            .is_err()
        );
        assert!(
            verify_signed_hash(
                "launcher@1.2.3",
                "missing.zip",
                sums.as_bytes(),
                signature.as_bytes(),
                &public
            )
            .is_err()
        );
    }
}
