fn main() {
    let generated =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("build output directory"));
    let protocol = "protocols/presentation-time.xml";
    for (operation, output) in [
        ("client-header", "presentation-time-client-protocol.h"),
        ("private-code", "presentation-time-protocol.c"),
    ] {
        assert!(
            std::process::Command::new("wayland-scanner")
                .arg(operation)
                .arg(protocol)
                .arg(generated.join(output))
                .status()
                .expect("wayland-scanner")
                .success()
        );
    }
    let mut builder = cc::Build::new();
    for package in ["sdl2", "epoxy", "egl", "wayland-client"] {
        let library =
            pkg_config::probe_library(package).expect("host display development libraries");
        builder.includes(library.include_paths);
    }
    builder
        .include(&generated)
        .file(generated.join("presentation-time-protocol.c"))
        .file("presentation.c")
        .file("frame_tags.c")
        .file("stage_profile.c")
        .file("viewer.c")
        .warnings_into_errors(true)
        .compile("legion_dbus_viewer");
    for source in [
        "viewer.c",
        "presentation.c",
        "presentation.h",
        "frame_tags.c",
        "frame_tags.h",
        "stage_profile.c",
        "stage_profile.h",
        protocol,
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
}
