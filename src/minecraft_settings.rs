//! Import only Minecraft's options.txt, leaving the source install and existing player data alone.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, PartialEq)]
pub(super) enum Imported {
    Copied {
        source: PathBuf,
        backup: Option<PathBuf>,
    },
    AlreadyPresent,
    Unchanged,
    NotFound,
}

pub(super) fn source(root: &Path, explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(folder) = explicit {
        let file = Path::new(folder).join("options.txt");
        return file.is_file().then_some(file);
    }
    let mut candidates = Vec::new();
    if let Some(folder) = std::env::var_os("HORIZON_MINECRAFT_DIR") {
        candidates.push(PathBuf::from(folder));
    }
    let mut homes = vec![
        root.join("Horizon Offline Data"),
        root.join("Horizon Rust Data"),
    ];
    if let Some(home) = std::env::var_os("HORIZON_LAUNCHER_HOME") {
        homes.push(home.into());
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        homes.push(PathBuf::from(&appdata).join("horizon-launcher"));
    }
    for home in homes {
        if let Ok(data) = fs::read(home.join("settings.json")) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) {
                if let Some(folder) = value["minecraft_dir"].as_str().filter(|s| !s.is_empty()) {
                    candidates.push(PathBuf::from(folder));
                }
            }
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(&appdata).join(".minecraft"));
        candidates.push(PathBuf::from(appdata).join(".tlauncher"));
    }
    for key in ["USERPROFILE", "HOME"] {
        if let Some(home) = std::env::var_os(key) {
            candidates.push(PathBuf::from(home).join(".minecraft"));
        }
    }
    candidates
        .into_iter()
        .map(|p| p.join("options.txt"))
        .find(|p| p.is_file())
}

pub(super) fn import(
    root: &Path,
    explicit: Option<&str>,
    home: &Path,
    replace: bool,
) -> Result<Imported, String> {
    let target = home.join("options.txt");
    if target.exists() && !replace {
        return Ok(Imported::AlreadyPresent);
    }
    let Some(source) = source(root, explicit) else {
        return Ok(Imported::NotFound);
    };
    import_file(&source, &target)
}

fn import_file(source: &Path, target: &Path) -> Result<Imported, String> {
    let error = |e: std::io::Error| format!("Не удалось импортировать настройки Minecraft: {e}");
    if fs::metadata(source).map_err(error)?.len() > 4 * 1024 * 1024 {
        return Err("options.txt слишком большой для файла настроек".into());
    }
    let data = fs::read(source).map_err(error)?;
    if fs::read(target).ok().as_ref() == Some(&data) {
        return Ok(Imported::Unchanged);
    }
    if fs::symlink_metadata(target).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("options.txt в папке игры является ссылкой. Импорт отменён".into());
    }
    let home = target.parent().ok_or("не найдена папка настроек")?;
    fs::create_dir_all(home).map_err(error)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let pending = home.join(format!(".options-import-{}-{stamp}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&pending)
        .map_err(error)?;
    file.write_all(&data)
        .and_then(|()| file.sync_all())
        .map_err(error)?;
    drop(file);
    let backup = if target.exists() {
        let folder = home.join(".options-backups");
        fs::create_dir_all(&folder).map_err(error)?;
        let path = folder.join(format!("options-{stamp}.txt"));
        if let Err(e) = fs::rename(target, &path) {
            let _ = fs::remove_file(&pending);
            return Err(error(e));
        }
        Some(path)
    } else {
        None
    };
    if let Err(e) = fs::rename(&pending, target) {
        if let Some(backup) = &backup {
            fs::rename(backup, target).map_err(|restore| {
                format!(
                    "{e}; восстановление: {restore}. Копия: {}",
                    backup.display()
                )
            })?;
        }
        let _ = fs::remove_file(&pending);
        return Err(error(e));
    }
    Ok(Imported::Copied {
        source: source.to_path_buf(),
        backup,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_original_options_once_and_manual_import_keeps_a_backup() {
        let root = std::env::temp_dir().join(format!(
            "horizon-options-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let original = root.join("original Minecraft");
        let home = root.join("offline game");
        fs::create_dir_all(&original).unwrap();
        let first =
            "lang:ru_ru\nsoundVolume_master:0.45\nkey_key.forward:key.keyboard.up\nfov:0.5\n";
        fs::write(original.join("options.txt"), first).unwrap();
        let explicit = original.to_str();
        assert!(matches!(
            import(&root, explicit, &home, false).unwrap(),
            Imported::Copied { backup: None, .. }
        ));
        assert_eq!(fs::read_to_string(home.join("options.txt")).unwrap(), first);
        fs::write(home.join("options.txt"), "player changed settings").unwrap();
        assert_eq!(
            import(&root, explicit, &home, false).unwrap(),
            Imported::AlreadyPresent
        );
        let result = import(&root, explicit, &home, true).unwrap();
        let Imported::Copied {
            backup: Some(backup),
            ..
        } = result
        else {
            panic!("no settings backup")
        };
        assert_eq!(
            fs::read_to_string(backup).unwrap(),
            "player changed settings"
        );
        assert_eq!(
            fs::read_to_string(original.join("options.txt")).unwrap(),
            first
        );
        assert_eq!(
            import(&root, explicit, &home, true).unwrap(),
            Imported::Unchanged
        );
        assert_eq!(
            import(
                &root,
                Some(root.join("missing").to_str().unwrap()),
                &home,
                true
            )
            .unwrap(),
            Imported::NotFound
        );
        assert!(!home.join("accounts.json").exists());
        if root.parent() == Some(std::env::temp_dir().as_path()) {
            fs::remove_dir_all(root).unwrap();
        }
    }
}
