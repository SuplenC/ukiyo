//! Config operations shared by the CLI (no daemon running) and the daemon
//! (`config get/set/reset/schema` over IPC): finding an option by key,
//! validating a value for it, persisting to the user config.

use serde_json::{Value as Json, json};
use toml::Value;
use ukiyo_core::catalog;
use ukiyo_core::config::{self, Config, OptDef, Paths};
use ukiyo_core::scene::{Manifest, Scene};

/// A scene manifest, as the option lookups need it.
pub type SceneRef<'a> = Option<(&'a str, &'a Manifest)>;

/// Load an installed scene's manifest by id.
pub fn load_manifest(id: &str) -> Result<Manifest, String> {
    let dir = catalog::find(&catalog::search_path(), id)
        .ok_or_else(|| format!("no scene `{id}` is installed"))?;
    Scene::load(&dir)
        .map(|s| s.manifest)
        .map_err(|e| e.to_string())
}

/// The option `key` names: a global option, `scenes.<id>.<global option>`
/// (a per-scene override) or `scenes.<id>.<param>`. Params of `current`
/// are looked up without touching the disk; other scenes are loaded from
/// the catalog.
pub fn find_def(key: &str, current: SceneRef) -> Result<OptDef, String> {
    let globals = config::global_schema();
    if let Some(d) = globals.iter().find(|d| d.key == key) {
        return Ok(d.clone());
    }
    let unknown = || format!("unknown option `{key}` (see `ukiyo config schema`)");
    let rest = key.strip_prefix("scenes.").ok_or_else(unknown)?;
    let (id, name) = rest.split_once('.').ok_or_else(unknown)?;
    if id.is_empty() || name.is_empty() {
        return Err(unknown());
    }
    if let Some(d) = globals.iter().find(|d| d.key == name) {
        if d.key == "scene" {
            return Err(unknown());
        }
        let mut d = d.clone();
        d.key = key.to_string();
        return Ok(d);
    }
    let scene_schema = match current {
        Some((cid, m)) if cid == id => config::scene_schema(id, m),
        _ => config::scene_schema(id, &load_manifest(id)?),
    };
    scene_schema
        .into_iter()
        .find(|d| d.key == key)
        .ok_or_else(|| format!("scene `{id}` has no option `{name}`"))
}

/// A value from the wire or the command line for `def`. Strings are parsed
/// as the command line would (`on`, `0.5`, `cargo,make`); JSON numbers,
/// bools and lists are checked as they are.
pub fn parse_value(def: &OptDef, v: &Json) -> Result<Value, String> {
    let r = match v {
        Json::String(s) => def.parse_cli(s),
        other => {
            let t = config::from_json(other).ok_or("unsupported value")?;
            def.coerce(&t)
        }
    };
    r.map_err(|e| format!("`{}`: {e}", def.key))
}

/// Validate and write `key = value` to the user config. Returns the
/// stored value.
pub fn set(paths: &Paths, key: &str, v: &Json, current: SceneRef) -> Result<Value, String> {
    let def = find_def(key, current)?;
    let value = parse_value(&def, v)?;
    config::edit::set(&paths.user, key, &value)?;
    Ok(value)
}

/// Remove `key` (an option, or `scenes.<id>` for all of a scene's
/// overrides) from the user config. Returns whether anything was removed.
pub fn reset(paths: &Paths, key: &str, current: SceneRef) -> Result<bool, String> {
    let whole_scene = key
        .strip_prefix("scenes.")
        .is_some_and(|id| !id.is_empty() && !id.contains('.'));
    if !whole_scene {
        // Only known options; a typo shouldn't silently do nothing. Params of
        // a scene that's gone can still be reset.
        if let Err(e) = find_def(key, current)
            && !key.starts_with("scenes.")
        {
            return Err(e);
        }
    }
    config::edit::reset(&paths.user, key)
}

/// `config get`: the effective value (for the current scene), its default
/// and where it came from.
pub fn get(cfg: &Config, key: &str, current: SceneRef) -> Result<Json, String> {
    let def = find_def(key, current)?;
    let (value, source) = cfg.value(&def, current.map(|(id, _)| id));
    Ok(json!({
        "key": def.key,
        "value": config::to_json(&value),
        "default": config::to_json(&def.default),
        "source": source,
        "label": def.label,
    }))
}

/// `config schema --json`: every option with its current value.
pub fn schema_json(cfg: &Config, scene: SceneRef) -> Json {
    let sch = config::schema(scene);
    let entries = config::schema_entries(cfg, &sch, scene.map(|(id, _)| id));
    serde_json::to_value(entries).unwrap_or(Json::Null)
}

/// One option as a line of text, for `config schema` without `--json`.
pub fn describe(entry: &Json) -> String {
    let key = entry["key"].as_str().unwrap_or("");
    let value = short(&entry["value"]);
    let mut range = match entry["type"].as_str() {
        Some("float") => format!(
            "{}..{}{}",
            short(&entry["min"]),
            short(&entry["max"]),
            entry["unit"]
                .as_str()
                .map(|u| format!(" {u}"))
                .unwrap_or_default()
        ),
        Some("choice") => entry["options"]
            .as_array()
            .map(|o| o.iter().map(short).collect::<Vec<_>>().join("|"))
            .unwrap_or_default(),
        Some(t) => t.to_string(),
        None => String::new(),
    };
    if !entry["source"].is_null() {
        range += &format!(", from {}", short(&entry["source"]));
    }
    format!(
        "{key} = {value}    # {} ({range})",
        entry["label"].as_str().unwrap_or("")
    )
}

/// A JSON value as the user would type it.
pub fn short(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Array(a) => a.iter().map(short).collect::<Vec<_>>().join(","),
        Json::Null => "-".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukiyo_core::config::Kind;

    fn manifest() -> Manifest {
        Manifest::parse(
            "[scene]\nname = \"Gate\"\n[params.rain]\ntype = \"float\"\nlabel = \"Rain\"\ndefault = 0.5\nmin = 0\nmax = 2\n",
        )
        .unwrap()
    }

    #[test]
    fn finds_global_override_and_param() {
        let m = manifest();
        let cur = Some(("gate", &m));
        assert_eq!(find_def("intensity", cur).unwrap().key, "intensity");
        let d = find_def("scenes.gate.performance.fps", cur).unwrap();
        assert_eq!(d.key, "scenes.gate.performance.fps");
        assert!(matches!(d.kind, Kind::Float { .. }));
        assert_eq!(find_def("scenes.gate.rain", cur).unwrap().label, "Rain");
        assert!(
            find_def("scenes.gate.snow", cur)
                .unwrap_err()
                .contains("no option")
        );
        assert!(
            find_def("bogus", cur)
                .unwrap_err()
                .contains("unknown option")
        );
        assert!(find_def("scenes.gate.scene", cur).is_err());
        assert!(find_def("scenes.nope-not-installed.rain", None).is_err());
    }

    #[test]
    fn parses_cli_text_and_json() {
        let m = manifest();
        let cur = Some(("gate", &m));
        let d = find_def("performance.motion", cur).unwrap();
        assert_eq!(parse_value(&d, &json!("off")), Ok(Value::Boolean(false)));
        assert_eq!(parse_value(&d, &json!(true)), Ok(Value::Boolean(true)));
        let d = find_def("scenes.gate.rain", cur).unwrap();
        assert_eq!(
            parse_value(&d, &json!("5")),
            Ok(Value::Float(2.0)),
            "clamped"
        );
        assert_eq!(parse_value(&d, &json!(1)), Ok(Value::Float(1.0)));
        assert!(
            parse_value(&d, &json!("lots"))
                .unwrap_err()
                .contains("rain")
        );
        let d = find_def("signals.build.commands", cur).unwrap();
        assert_eq!(
            parse_value(&d, &json!("cargo, make")),
            Ok(Value::Array(vec!["cargo".into(), "make".into()]))
        );
    }

    #[test]
    fn set_get_reset_round_trip() {
        let dir = std::env::temp_dir().join(format!("ukiyo-confops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths::in_dir(&dir);
        let m = manifest();
        let cur = Some(("gate", &m));
        set(&paths, "intensity", &json!("0.25"), cur).unwrap();
        set(&paths, "scenes.gate.rain", &json!("1.5"), cur).unwrap();
        assert!(set(&paths, "intensity", &json!("x"), cur).is_err());
        let cfg = Config::load(&paths).0;
        assert_eq!(get(&cfg, "intensity", cur).unwrap()["value"], 0.25);
        let rain = get(&cfg, "scenes.gate.rain", cur).unwrap();
        assert_eq!(rain["value"], 1.5);
        assert_eq!(rain["source"], "user");
        assert_eq!(rain["default"], 0.5);
        let sch = schema_json(&cfg, cur);
        assert!(
            sch.as_array()
                .unwrap()
                .iter()
                .any(|e| e["key"] == "scenes.gate.rain" && e["value"] == 1.5)
        );
        assert!(reset(&paths, "scenes.gate", cur).unwrap());
        assert!(reset(&paths, "bogus", cur).is_err());
        assert!(reset(&paths, "intensity", cur).unwrap());
        assert!(!reset(&paths, "intensity", cur).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn describe_lines() {
        let e = json!({"key": "performance.fps", "type": "float", "min": 5.0, "max": 144.0,
            "unit": "fps", "value": 60.0, "label": "Frame rate cap", "source": null});
        assert_eq!(
            describe(&e),
            "performance.fps = 60.0    # Frame rate cap (5.0..144.0 fps)"
        );
    }
}
