//! Apply a signature-checked release as a file patch, keeping identical installed files.
//! The official launcher downloads into a private staging home; it never reinstalls the live
//! runtime. A journal and changed-file backups let a failed/interrupted patch roll back.
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const MANIFEST: &str = ".horizon-rust-files.json";
const JOURNAL: &str = "journal.json";
const COMMITTED: &str = "committed";
const CLIENT: &str = "horizon-client.exe";

#[derive(Default, Debug, PartialEq)]
pub(super) struct Summary {
    pub kept: usize,
    pub replaced: usize,
    pub added: usize,
    pub removed: usize,
    pub resources_kept: bool,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    from: String,
    to: String,
    replace: Vec<PathBuf>,
    add: Vec<PathBuf>,
    remove: Vec<PathBuf>,
    resources: bool,
}

fn failure(e: impl std::fmt::Display) -> String {
    format!("Не удалось обновить файлы Horizon: {e}")
}

fn tag_safe(tag: &str) -> bool {
    !tag.is_empty()
        && tag != "."
        && tag != ".."
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn relative_safe(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && !path.to_string_lossy().contains(':')
}

fn protected(path: &Path) -> bool {
    matches!(path.components().next(), Some(Component::Normal(name)) if
        [".local", "saves", "logs", "screenshots", "resourcepacks", "shaderpacks",
         "horizon-mods", "horizon-rust.json"].iter().any(|p| name.as_encoded_bytes().eq_ignore_ascii_case(p.as_bytes())))
}

/// Reject junctions/symlinks along a path before reading, replacing or removing it.
fn contained(root: &Path, relative: &Path) -> io::Result<PathBuf> {
    if !relative_safe(relative) {
        return Err(io::Error::other("unsafe update path"));
    }
    let mut path = root.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(io::Error::other("update path is a link"));
            }
            Ok(metadata) if index + 1 < components.len() && !metadata.is_dir() => {
                return Err(io::Error::other(
                    "update directory conflicts with an installed file",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(path)
}

fn files(root: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        if protected(&path) {
            continue;
        }
        contained(root, &path)?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files(root, &path, out)?;
        } else if kind.is_file() {
            out.push(path);
        } else {
            return Err(io::Error::other("unsupported release file"));
        }
    }
    out.sort();
    Ok(())
}

fn same(a: &Path, b: &Path) -> io::Result<bool> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    let (mut left, mut right) = ([0u8; 65536], [0u8; 65536]);
    loop {
        let count = a.read(&mut left)?;
        b.read_exact(&mut right[..count])?;
        if left[..count] != right[..count] {
            return Ok(false);
        }
        if count == 0 {
            return Ok(true);
        }
    }
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Same resource contract as the official launcher: the Minecraft pins and the preparer's
/// declared output version. Unknown old preparers are reusable only when their program matches.
fn resources_compatible(from: &Path, to: &Path) -> bool {
    let pins = |root: &Path| {
        read_json(&root.join("research-lock.json"))
            .map(|v| ["version", "client_sha1", "server_sha1"].map(|k| v["minecraft"][k].clone()))
    };
    let (Some(a), Some(b)) = (pins(from), pins(to)) else {
        return false;
    };
    if a != b || a.iter().any(|v| !v.is_string()) {
        return false;
    }
    let version = |root: &Path| {
        fs::read_to_string(root.join("crates/horizon-prepare/src/lib.rs"))
            .ok()?
            .lines()
            .find_map(|line| {
                line.strip_prefix("pub const OUTPUT_VERSION: u32 = ")?
                    .strip_suffix(';')?
                    .parse::<u32>()
                    .ok()
            })
    };
    match (version(from), version(to)) {
        (Some(a), Some(b)) => a == b,
        (None, None) => same(
            &from.join("horizon-prepare.exe"),
            &to.join("horizon-prepare.exe"),
        )
        .unwrap_or(false),
        _ => false,
    }
}

fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to)
}

fn sync_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, value).map_err(io::Error::other)?;
    file.sync_all()
}

fn validate(journal: &Journal) -> io::Result<()> {
    if !tag_safe(&journal.from) || !tag_safe(&journal.to) || journal.from == journal.to {
        return Err(io::Error::other("unsafe update version"));
    }
    for path in journal
        .replace
        .iter()
        .chain(&journal.add)
        .chain(&journal.remove)
    {
        if !relative_safe(path) || protected(path) {
            return Err(io::Error::other("unsafe update journal"));
        }
    }
    Ok(())
}

fn rollback(versions: &Path, work: &Path, journal: &Journal) -> io::Result<()> {
    validate(journal)?;
    let old = contained(versions, Path::new(&journal.from))?;
    let new = contained(versions, Path::new(&journal.to))?;
    if !old.exists() && new.exists() {
        fs::rename(&new, &old)?;
    }
    for relative in &journal.add {
        let path = contained(&old, relative)?;
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    for relative in journal.replace.iter().chain(&journal.remove) {
        let backup = contained(&work.join("backup"), relative)?;
        if backup.is_file() {
            let target = contained(&old, relative)?;
            if target.exists() {
                fs::remove_file(&target)?;
            }
            move_file(&backup, &target)?;
        }
    }
    let resources = work.join("prepared-resources");
    if journal.resources && resources.is_dir() {
        let target = contained(&old, Path::new(".local/horizon-client"))?;
        if target.exists() {
            return Err(io::Error::other("resources appeared during rollback"));
        }
        move_file(&resources, &target)?;
    }
    fs::remove_file(work.join(JOURNAL))?;
    Ok(())
}

fn recover(home: &Path) -> io::Result<()> {
    let transactions = contained(home, Path::new(".rust-updates"))?;
    if !transactions.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(transactions)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let work = entry.path();
        if work.join(COMMITTED).exists() || !work.join(JOURNAL).exists() {
            continue;
        }
        let journal =
            serde_json::from_slice(&fs::read(work.join(JOURNAL))?).map_err(io::Error::other)?;
        rollback(&home.join("versions"), &work, &journal)?;
    }
    Ok(())
}

fn patch(
    versions: &Path,
    from_tag: &str,
    to_tag: &str,
    incoming: &Path,
    work: &Path,
) -> io::Result<Summary> {
    if !tag_safe(from_tag) || !tag_safe(to_tag) {
        return Err(io::Error::other("unsafe version"));
    }
    let active = contained(versions, Path::new(from_tag))?;
    let target = contained(versions, Path::new(to_tag))?;
    if target.exists() {
        return Err(io::Error::other("target version already exists"));
    }
    if !incoming.join(CLIENT).is_file() || !incoming.join(".installed").is_file() {
        return Err(io::Error::other(
            "установка не подтверждена: нет полного клиента",
        ));
    }
    let mut managed = Vec::new();
    files(incoming, Path::new(""), &mut managed)?;
    managed.retain(|p| p != Path::new(".installed") && p != Path::new(MANIFEST));
    let previous: Vec<PathBuf> = match fs::read(active.join(MANIFEST)) {
        Ok(data) => serde_json::from_slice(&data).map_err(io::Error::other)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    sync_json(&incoming.join(MANIFEST), &managed)?;
    fs::write(incoming.join(".installed"), to_tag)?;
    let resources = contained(&active, Path::new(".local/horizon-client"))?;
    let mut summary = Summary {
        resources_kept: resources.is_dir() && resources_compatible(&active, incoming),
        ..Summary::default()
    };
    let mut journal = Journal {
        from: from_tag.into(),
        to: to_tag.into(),
        replace: Vec::new(),
        add: Vec::new(),
        remove: Vec::new(),
        resources: resources.is_dir() && !summary.resources_kept,
    };
    for relative in managed.iter().chain(
        [Path::new(MANIFEST), Path::new(".installed")]
            .map(Path::to_path_buf)
            .iter(),
    ) {
        let old = contained(&active, relative)?;
        let new = contained(incoming, relative)?;
        if old.is_file() {
            if same(&old, &new)? {
                summary.kept += 1;
            } else {
                journal.replace.push(relative.clone());
            }
        } else if old.exists() {
            return Err(io::Error::other(
                "file conflicts with an installed directory",
            ));
        } else {
            journal.add.push(relative.clone());
        }
    }
    for relative in previous {
        if !relative_safe(&relative)
            || protected(&relative)
            || relative == Path::new(".installed")
            || relative == Path::new(MANIFEST)
        {
            return Err(io::Error::other("unsafe managed-file manifest"));
        }
        if !managed.contains(&relative) && contained(&active, &relative)?.is_file() {
            journal.remove.push(relative);
        }
    }
    validate(&journal)?;
    // Persist the entire plan before the first live mutation. Backups are renames on the same
    // volume; the old directory and unchanged files are never copied or extracted again.
    sync_json(&work.join(JOURNAL), &journal)?;
    let apply = || -> io::Result<()> {
        for relative in journal.replace.iter().chain(&journal.remove) {
            move_file(
                &contained(&active, relative)?,
                &contained(&work.join("backup"), relative)?,
            )?;
        }
        for relative in journal.replace.iter().chain(&journal.add) {
            move_file(
                &contained(incoming, relative)?,
                &contained(&active, relative)?,
            )?;
        }
        if journal.resources {
            move_file(&resources, &work.join("prepared-resources"))?;
        }
        fs::rename(&active, &target)?;
        File::create(work.join(COMMITTED))?.sync_all()?;
        Ok(())
    };
    if let Err(error) = apply() {
        rollback(versions, work, &journal).map_err(|rollback| {
            io::Error::other(format!(
                "{error}; восстановление: {rollback}. Резервная копия: {}",
                work.display()
            ))
        })?;
        return Err(error);
    }
    let visible = |p: &&PathBuf| **p != Path::new(MANIFEST) && **p != Path::new(".installed");
    summary.replaced = journal.replace.iter().filter(visible).count();
    summary.added = journal.add.iter().filter(visible).count();
    summary.removed = journal.remove.len();
    Ok(summary)
}

fn home(command: &Command) -> Result<PathBuf, String> {
    let key = OsStr::new("HORIZON_LAUNCHER_HOME");
    let value = command
        .get_envs()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.map(ToOwned::to_owned))
        .unwrap_or_else(|| std::env::var_os(key));
    if let Some(value) = value.filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    #[cfg(windows)]
    let default = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let default = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")));
    default
        .map(|p| p.join("horizon-launcher"))
        .ok_or_else(|| failure("не найдена папка лаунчера"))
}

/// On Windows an exclusive file handle is released by the OS even after a crash.
struct Lock {
    file: Option<File>,
    path: PathBuf,
}
impl Lock {
    fn take(home: &Path) -> io::Result<Self> {
        fs::create_dir_all(home)?;
        let path = home.join(".rust-update.lock");
        let mut options = OpenOptions::new();
        options.write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.create(true).truncate(false).share_mode(0);
        }
        #[cfg(not(windows))]
        options.create_new(true);
        Ok(Self {
            file: Some(options.open(&path)?),
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

pub(super) fn recover_pending(command: &Command) -> Result<(), String> {
    let home = home(command)?;
    if !home.join(".rust-updates").is_dir() {
        return Ok(());
    }
    let _lock = Lock::take(&home).map_err(failure)?;
    recover(&home).map_err(failure)
}

pub(super) fn install(
    make_command: &impl Fn() -> Command,
    tag: &str,
    installed: &[String],
) -> Result<Option<Summary>, String> {
    let home = home(&make_command())?;
    let _lock = Lock::take(&home).map_err(|e| {
        failure(format!(
            "обновление занято другим процессом или папка недоступна: {e}"
        ))
    })?;
    recover(&home).map_err(failure)?;
    let versions = home.join("versions");
    let base = installed
        .iter()
        .filter(|t| tag_safe(t) && t.as_str() != tag)
        .filter_map(|tag| {
            let dir = contained(&versions, Path::new(tag)).ok()?;
            if !dir.join(CLIENT).is_file() {
                return None;
            }
            Some((
                fs::metadata(dir.join(".installed")).ok()?.modified().ok()?,
                tag,
            ))
        })
        .max()
        .map(|(_, tag)| tag);
    let run = |command: &mut Command| -> Result<(), String> {
        let status = command
            .args(["install", tag])
            .stdin(Stdio::null())
            .status()
            .map_err(failure)?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "Не удалось установить Horizon {tag} ({status}). Повтори попытку"
            ))
        }
    };
    let Some(base) = base else {
        run(&mut make_command())?;
        return Ok(None);
    };
    if !tag_safe(tag) {
        return Err(failure("некорректный номер версии"));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(failure)?
        .as_nanos();
    let work = contained(
        &home,
        Path::new(&format!(".rust-updates/{}-{stamp}", std::process::id())),
    )
    .map_err(failure)?;
    fs::create_dir_all(&work).map_err(failure)?;
    let staging = work.join("staging");
    let result = run(make_command()
        .env("HORIZON_LAUNCHER_HOME", &staging)
        .env("HORIZON_HOME", staging.join("game")))
    .and_then(|()| {
        patch(
            &versions,
            base,
            tag,
            &staging.join("versions").join(tag),
            &work,
        )
        .map_err(failure)
    });
    // This is always the private staging child of our own transaction, never a game directory.
    if staging.parent() == Some(work.as_path()) && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result.map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    static NEXT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "horizon-file-patch-{}-{stamp}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn put(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fn release(&self, prefix: &str, client: &str) {
            for (path, contents) in [
                (CLIENT, client),
                (".installed", ""),
                ("unchanged.txt", "same"),
                (
                    "research-lock.json",
                    r#"{"minecraft":{"version":"26.3","client_sha1":"c","server_sha1":"s"}}"#,
                ),
                (
                    "crates/horizon-prepare/src/lib.rs",
                    "pub const OUTPUT_VERSION: u32 = 1;",
                ),
            ] {
                self.put(&format!("{prefix}/{path}"), contents);
            }
        }
        fn setup(&self) {
            self.release("versions/old", "old program");
            self.release("incoming", "new program");
            self.put("work/placeholder", "");
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.0.parent() == Some(std::env::temp_dir().as_path()) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn patch_keeps_identical_files_and_resources_and_updates_only_managed_files() {
        let f = Fixture::new();
        f.setup();
        f.put("incoming/nested/new.txt", "new file");
        f.put("versions/old/obsolete.txt", "old managed file");
        f.put("versions/old/player-extra.txt", "player extra");
        f.put(
            "versions/old/.local/horizon-client/26.3/horizon-resources.json",
            "resources",
        );
        f.put(
            "versions/old/.horizon-rust-files.json",
            r#"["obsolete.txt","horizon-client.exe"]"#,
        );
        fs::hard_link(
            f.0.join("versions/old/unchanged.txt"),
            f.0.join("same-file-witness"),
        )
        .unwrap();
        let result = patch(
            &f.0.join("versions"),
            "old",
            "new",
            &f.0.join("incoming"),
            &f.0.join("work"),
        )
        .unwrap();
        assert_eq!((result.replaced, result.added, result.removed), (1, 1, 1));
        assert!(result.resources_kept && result.kept >= 3);
        assert_eq!(
            fs::read_to_string(f.0.join("versions/new/horizon-client.exe")).unwrap(),
            "new program"
        );
        assert_eq!(
            fs::read_to_string(f.0.join("versions/new/player-extra.txt")).unwrap(),
            "player extra"
        );
        assert!(!f.0.join("versions/new/obsolete.txt").exists());
        assert!(
            f.0.join("versions/new/.local/horizon-client/26.3/horizon-resources.json")
                .is_file()
        );
        fs::write(f.0.join("same-file-witness"), "same physical file").unwrap();
        assert_eq!(
            fs::read_to_string(f.0.join("versions/new/unchanged.txt")).unwrap(),
            "same physical file"
        );
        assert_eq!(
            fs::read_to_string(f.0.join("work/backup/horizon-client.exe")).unwrap(),
            "old program"
        );
        assert!(f.0.join("work/committed").is_file());
    }

    #[test]
    fn legacy_installs_keep_untracked_files_and_incompatible_resources_are_backed_up() {
        let f = Fixture::new();
        f.setup();
        f.put("versions/old/unknown.txt", "keep");
        f.put(
            "versions/old/.local/minecraft/26.3/local.jar",
            "player original copy",
        );
        f.put(
            "versions/old/.local/horizon-client/26.3/horizon-resources.json",
            "old format",
        );
        f.put(
            "incoming/crates/horizon-prepare/src/lib.rs",
            "pub const OUTPUT_VERSION: u32 = 2;",
        );
        let result = patch(
            &f.0.join("versions"),
            "old",
            "new",
            &f.0.join("incoming"),
            &f.0.join("work"),
        )
        .unwrap();
        assert!(!result.resources_kept);
        assert!(f.0.join("versions/new/unknown.txt").is_file());
        assert!(
            f.0.join("versions/new/.local/minecraft/26.3/local.jar")
                .is_file()
        );
        assert!(!f.0.join("versions/new/.local/horizon-client").exists());
        assert!(
            f.0.join("work/prepared-resources/26.3/horizon-resources.json")
                .is_file()
        );
    }

    #[test]
    fn interrupted_patch_rolls_back_even_after_directory_was_renamed() {
        for renamed in [false, true] {
            let f = Fixture::new();
            f.setup();
            let work = f.0.join(".rust-updates/interrupted");
            fs::create_dir_all(&work).unwrap();
            let journal = Journal {
                from: "old".into(),
                to: "new".into(),
                replace: vec![CLIENT.into()],
                add: vec!["new-file.txt".into()],
                remove: vec![],
                resources: false,
            };
            sync_json(&work.join(JOURNAL), &journal).unwrap();
            move_file(
                &f.0.join("versions/old/horizon-client.exe"),
                &work.join("backup/horizon-client.exe"),
            )
            .unwrap();
            f.put("versions/old/horizon-client.exe", "partial new program");
            f.put("versions/old/new-file.txt", "partial addition");
            if renamed {
                fs::rename(f.0.join("versions/old"), f.0.join("versions/new")).unwrap();
            }
            recover(&f.0).unwrap();
            assert_eq!(
                fs::read_to_string(f.0.join("versions/old/horizon-client.exe")).unwrap(),
                "old program"
            );
            assert!(!f.0.join("versions/old/new-file.txt").exists());
            assert!(!f.0.join("versions/new").exists());
            assert!(!work.join(JOURNAL).exists());
            recover(&f.0).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn locked_client_rolls_back_other_changes_and_keeps_old_runtime() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = Fixture::new();
        f.setup();
        f.put("versions/old/aaa.txt", "old");
        f.put("incoming/aaa.txt", "new");
        let _running = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(f.0.join("versions/old/horizon-client.exe"))
            .unwrap();
        assert!(
            patch(
                &f.0.join("versions"),
                "old",
                "new",
                &f.0.join("incoming"),
                &f.0.join("work")
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(f.0.join("versions/old/aaa.txt")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(f.0.join("versions/old/horizon-client.exe")).unwrap(),
            "old program"
        );
        assert!(!f.0.join("versions/new").exists());
        assert!(!f.0.join("work/journal.json").exists());
    }

    #[test]
    fn unsafe_manifests_and_tags_are_rejected_before_live_changes() {
        for manifest in [
            r#"["../escape"]"#,
            r#"[".local/keep"]"#,
            r#"["file:stream"]"#,
        ] {
            let f = Fixture::new();
            f.setup();
            f.put("versions/old/.horizon-rust-files.json", manifest);
            assert!(
                patch(
                    &f.0.join("versions"),
                    "old",
                    "new",
                    &f.0.join("incoming"),
                    &f.0.join("work")
                )
                .is_err()
            );
            assert_eq!(
                fs::read_to_string(f.0.join("versions/old/horizon-client.exe")).unwrap(),
                "old program"
            );
        }
        assert!(!tag_safe("../outside"));
        assert!(!relative_safe(Path::new("C:/outside")));
    }

    #[test]
    fn directory_file_conflicts_are_rejected_before_any_live_changes() {
        let f = Fixture::new();
        f.setup();
        f.put("versions/old/nested", "player file");
        f.put("incoming/nested/new.txt", "release file");
        assert!(
            patch(
                &f.0.join("versions"),
                "old",
                "new",
                &f.0.join("incoming"),
                &f.0.join("work")
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(f.0.join("versions/old/horizon-client.exe")).unwrap(),
            "old program"
        );
        assert_eq!(
            fs::read_to_string(f.0.join("versions/old/nested")).unwrap(),
            "player file"
        );
        assert!(!f.0.join("work/journal.json").exists());
    }

    #[test]
    fn updates_hold_an_exclusive_lock_and_release_it() {
        let f = Fixture::new();
        let lock = Lock::take(&f.0).unwrap();
        assert!(Lock::take(&f.0).is_err());
        drop(lock);
        assert!(Lock::take(&f.0).is_ok());
    }
}
