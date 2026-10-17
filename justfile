# List the recipes.
default:
    @just --list

test:
    cargo test --workspace

# Open the winit preview on a scene.
preview scene="scenes/kabukicho-gate":
    cargo run --release -p ukiyo-preview -- {{scene}}

# Offscreen frame-time benchmark.
bench scene="scenes/kabukicho-gate" frames="300":
    cargo run --release -p ukiyo-preview -- {{scene}} --bench {{frames}}

# Render the scripted demo video to out/demo.mp4.
demo:
    mkdir -p out
    cargo run --release -p ukiyo-preview -- scenes/kabukicho-gate --script demos/kabukicho-gate.toml --record out/demo.mp4 --hud

fmt:
    cargo fmt --all

lint:
    cargo clippy --workspace --all-targets -- -D warnings
