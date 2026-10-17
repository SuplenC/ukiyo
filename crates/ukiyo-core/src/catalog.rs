//! Where scenes are installed, and finding them by id.
//!
//! Search path, first match wins: `$UKIYO_SCENE_PATH` (colon-separated),
//! `$XDG_DATA_HOME/ukiyo/scenes` (`~/.local/share/ukiyo/scenes`), then
//! `<dir>/ukiyo/scenes` for each of `$XDG_DATA_DIRS`. A scene is a
//! directory holding `scene.toml`; its id is the directory name.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::scene::Scene;

pub fn search_path() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(p) = std::env::var("UKIYO_SCENE_PATH") {
        dirs.extend(p.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    dirs.push(data_home.join("ukiyo/scenes"));
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|d| d.starts_with('/'))
            .map(|d| Path::new(d).join("ukiyo/scenes")),
    );
    dirs
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    pub poster: Option<PathBuf>,
    pub description: String,
}

/// Every scene on `dirs`, sorted by id; an id shadows later ones.
pub fn list(dirs: &[PathBuf]) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("scene.toml").is_file())
            .collect();
        found.sort();
        for p in found {
            let Ok(scene) = Scene::load(&p) else { continue };
            if out.iter().any(|e| e.id == scene.id) {
                continue;
            }
            out.push(Entry {
                poster: scene.poster_path(),
                name: scene.manifest.scene.name.clone(),
                description: scene.manifest.scene.description.clone(),
                id: scene.id,
                dir: p,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// A scene by id, or by path to its directory (or to its `scene.toml`).
pub fn find(dirs: &[PathBuf], id_or_path: &str) -> Option<PathBuf> {
    let p = Path::new(id_or_path);
    if id_or_path.contains('/') || p.join("scene.toml").is_file() {
        let dir = if p.file_name().is_some_and(|n| n == "scene.toml") {
            p.parent()?.to_path_buf()
        } else {
            p.to_path_buf()
        };
        return dir.join("scene.toml").is_file().then_some(dir);
    }
    dirs.iter()
        .map(|d| d.join(id_or_path))
        .find(|d| d.join("scene.toml").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_shadows() {
        let root = std::env::temp_dir().join(format!("ukiyo-cat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (dir, name) in [("a/one", "One A"), ("b/one", "One B"), ("b/two", "Two")] {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("scene.toml"),
                format!("[scene]\nname = \"{name}\"\nposter = \"p.jpg\"\n"),
            )
            .unwrap();
            std::fs::write(d.join("scene.wgsl"), "").unwrap();
        }
        let dirs = vec![root.join("a"), root.join("b"), root.join("missing")];
        let all = list(&dirs);
        let ids: Vec<(&str, &str)> = all
            .iter()
            .map(|e| (e.id.as_str(), e.name.as_str()))
            .collect();
        assert_eq!(ids, [("one", "One A"), ("two", "Two")]);
        assert_eq!(all[0].poster, Some(root.join("a/one/p.jpg")));
        assert_eq!(find(&dirs, "two"), Some(root.join("b/two")));
        assert_eq!(find(&dirs, "three"), None);
        let path = root.join("b/one/scene.toml");
        assert_eq!(
            find(&dirs, path.to_str().unwrap()),
            Some(root.join("b/one"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
