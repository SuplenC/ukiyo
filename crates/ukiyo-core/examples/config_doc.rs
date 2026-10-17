//! Print the option tables of docs/CONFIG.md from the schema:
//!
//!     cargo run -p ukiyo-core --example config_doc -- scenes/kabukicho-gate

use ukiyo_core::config::{self, Kind, OptDef};

fn row(d: &OptDef) -> String {
    let kind = match &d.kind {
        Kind::Bool => "bool".to_string(),
        Kind::Float { min, max, unit, .. } => format!(
            "{min} to {max}{}",
            if unit.is_empty() {
                String::new()
            } else {
                format!(" {unit}")
            }
        ),
        Kind::Choice { options } => options.join(" \\| "),
        Kind::Color => "colour".into(),
        Kind::String => "string".into(),
        Kind::List => "list of strings".into(),
    };
    let default = match &d.default {
        toml::Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        toml::Value::String(s) if s.is_empty() => "\"\"".into(),
        v => v.to_string().trim_matches('"').to_string(),
    };
    let help = if d.help.is_empty() {
        d.label.clone()
    } else {
        format!("{}. {}", d.label, d.help)
    };
    format!(
        "| `{}` | {} | `{}` | {} |",
        d.key,
        kind,
        default,
        help.replace("..", ".")
    )
}

fn main() {
    let dir = std::env::args().nth(1);
    let scene = dir.map(|d| ukiyo_core::Scene::load(d).expect("scene"));
    let schema = config::schema(scene.as_ref().map(|s| (s.id.as_str(), &s.manifest)));
    let mut groups: Vec<&str> = Vec::new();
    for d in &schema {
        if !groups.contains(&d.group.as_str()) {
            groups.push(&d.group);
        }
    }
    for g in groups {
        println!("### {g}\n\n| Key | Type | Default | What |\n|---|---|---|---|");
        for d in schema.iter().filter(|d| d.group == g) {
            println!("{}", row(d));
        }
        println!();
    }
}
