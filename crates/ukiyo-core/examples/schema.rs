//! Print the config schema (as `ukiyo config schema --json` does) for a
//! scene directory, with the user's config applied:
//!
//!     cargo run -p ukiyo-core --example schema -- scenes/kabukicho-gate

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "scenes/kabukicho-gate".into());
    let scene = ukiyo_core::Scene::load(&dir).expect("scene");
    let (cfg, warnings) = ukiyo_core::config::Config::load(&ukiyo_core::config::Paths::from_env());
    for w in warnings {
        eprintln!("warning: {w}");
    }
    let schema = ukiyo_core::config::schema(Some((&scene.id, &scene.manifest)));
    let entries = ukiyo_core::config::schema_entries(&cfg, &schema, Some(&scene.id));
    println!("{}", serde_json::to_string_pretty(&entries).expect("json"));
}
