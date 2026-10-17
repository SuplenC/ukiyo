# ukiyo: the daemon + CLI (`ukiyo`), the typing helper (`ukiyo-activityd`),
# the bundled scenes in share/ukiyo/scenes and the service files in
# share/ukiyo/dist. ukiyo-preview (the egui desktop previewer) is left out:
# it's a scene author's tool and doubles the build.
{
  lib,
  rustPlatform,
  pkg-config,
  addDriverRunpath,
  patchelf,
  wayland,
  libxkbcommon,
  vulkan-loader,
  libGL,
}:
let
  workspace = (lib.importTOML ../Cargo.toml).workspace.package;
  # wgpu and the Wayland EGL glue dlopen these at run time.
  runtimeLibs = [
    vulkan-loader
    libGL
    wayland
    libxkbcommon
  ];
in
rustPlatform.buildRustPackage {
  pname = "ukiyo";
  inherit (workspace) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../scenes
      ../docs/CONFIG.md
      (lib.fileset.maybeMissing ../dist)
      (lib.fileset.maybeMissing ../LICENSE-MIT)
      (lib.fileset.maybeMissing ../LICENSE-APACHE)
      (lib.fileset.maybeMissing ../CREDITS.md)
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    addDriverRunpath
    patchelf
  ];
  buildInputs = [
    wayland
    libxkbcommon
  ];

  cargoBuildFlags = [
    "-p"
    "ukiyo"
    "-p"
    "ukiyo-activityd"
  ];
  # Core and CLI tests. The renderer tests skip themselves without a GPU
  # adapter, which the build sandbox doesn't have.
  cargoTestFlags = [
    "-p"
    "ukiyo-core"
    "-p"
    "ukiyo"
  ];

  postInstall = ''
    for f in LICENSE-MIT LICENSE-APACHE CREDITS.md; do
      if [ -e "$f" ]; then install -Dm644 "$f" -t $out/share/doc/ukiyo; fi
    done
    install -d $out/share/ukiyo/scenes
    cp -r scenes/. $out/share/ukiyo/scenes/
    if [ -d dist ]; then
      install -d $out/share/ukiyo/dist
      cp -r dist/. $out/share/ukiyo/dist/
      # systemd units, where systemd looks for them: the typing helper is a
      # system service, anything else a user one.
      for unit in dist/*.service dist/*.socket dist/systemd/*.service dist/systemd/*.socket; do
        [ -e "$unit" ] || continue
        case "$unit" in
          *activityd*) dir=$out/lib/systemd/system ;;
          *) dir=$out/lib/systemd/user ;;
        esac
        install -Dm644 "$unit" "$dir/$(basename "$unit")"
        substituteInPlace "$dir/$(basename "$unit")" \
          --replace-quiet /usr/bin/ "$out/bin/" \
          --replace-quiet /usr/local/bin/ "$out/bin/"
      done
    fi
  '';

  # After fixup's --shrink-rpath, which would drop the dlopen'd libraries
  # again: the Nix libraries, then /run/opengl-driver/lib for the system's
  # Mesa (NixOS, or non-NixOS with home-manager's non-nixos-gpu-setup).
  postFixup = ''
    for bin in $out/bin/*; do
      patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} "$bin"
      addDriverRunpath "$bin"
    done
  '';

  passthru.scenes = "share/ukiyo/scenes";

  meta = {
    description = "Reactive live wallpapers for Wayland";
    homepage = workspace.repository;
    # The code; the bundled scenes carry their own (scene.toml, `license`).
    license = with lib.licenses; [
      mit
      asl20
      cc-by-sa-40
    ];
    mainProgram = "ukiyo";
    platforms = lib.platforms.linux;
  };
}
