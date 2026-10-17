//! The scenes shipped in `scenes/` parse, bind only known signals, and
//! (when a GPU, even a software one, is around) compile and render.

use std::path::PathBuf;

use ukiyo_core::config::{self, Config};
use ukiyo_core::render::{Renderer, SceneGpu, open_device};
use ukiyo_core::{Engine, Scene, catalog};

fn scenes_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scenes")
}

#[test]
fn every_bundled_scene_loads_and_binds_known_signals() {
    let scenes = catalog::list(&[scenes_dir()]);
    assert!(
        !scenes.is_empty(),
        "no scenes found in {}",
        scenes_dir().display()
    );
    for entry in scenes {
        let scene = Scene::load(&entry.dir).unwrap();
        assert!(
            scene.poster_path().is_some_and(|p| p.is_file()),
            "{}: poster missing",
            entry.id
        );
        let engine = Engine::new(scene.clone(), &Config::default());
        assert!(
            engine.unknown_names.is_empty(),
            "{}: {:?}",
            entry.id,
            engine.unknown_names
        );
        // Every binding input is a signal ukiyo knows (or a pulse).
        for (_, spec) in &scene.manifest.bindings {
            for name in ukiyo_core::expr::Expr::parse(spec.expr()).unwrap().names() {
                let known = name.starts_with("param.")
                    || name.starts_with("pulse.")
                    || scene.manifest.bindings.iter().any(|(b, _)| *b == name)
                    || ukiyo_core::signals::KNOWN.iter().any(|(k, _)| *k == name);
                assert!(known, "{}: binding reads unknown `{name}`", entry.id);
            }
        }
        // Its options all have valid defaults.
        for d in config::scene_schema(&entry.id, &scene.manifest) {
            assert!(d.coerce(&d.default).is_ok(), "{}", d.key);
        }
    }
}

#[test]
fn every_bundled_scene_renders() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok((_, device, queue)) = pollster::block_on(open_device(&instance, None)) else {
        eprintln!("no GPU adapter: skipping the render test");
        return;
    };
    for entry in catalog::list(&[scenes_dir()]) {
        let scene = Scene::load(&entry.dir).unwrap();
        let mut engine = Engine::new(scene, &Config::default());
        let gpu = SceneGpu::load(&device, &queue, &engine.scene, &engine.layout).unwrap();
        let mut r = Renderer::new(
            &device,
            gpu,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            (320, 200),
            1.0,
        )
        .unwrap();
        let frame = engine.tick(1.0);
        let img = r.capture(&device, &queue, &frame).unwrap();
        let mean: f64 = img
            .pixels()
            .map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]))
            .sum::<f64>()
            / (img.width() * img.height() * 3) as f64;
        assert!(mean > 5.0, "{}: rendered black ({mean})", entry.id);
    }
}

#[test]
fn every_option_is_documented() {
    let doc = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/CONFIG.md"),
    )
    .unwrap();
    let mut keys: Vec<String> = config::global_schema().into_iter().map(|d| d.key).collect();
    for entry in catalog::list(&[scenes_dir()]) {
        let scene = Scene::load(&entry.dir).unwrap();
        keys.extend(
            config::scene_schema(&entry.id, &scene.manifest)
                .into_iter()
                .map(|d| d.key),
        );
    }
    for k in keys {
        assert!(
            doc.contains(&format!("`{k}`")),
            "docs/CONFIG.md doesn't mention `{k}`"
        );
    }
}
