use std::path::Path;

const REPOSITORY_ONLY_TEST_MODULES: [&str; 8] = [
    "src/server/app_admission_tests.rs",
    "src/server/game_data_tests.rs",
    "src/server/message_coordinator_tests.rs",
    "src/server/message_router_tests.rs",
    "src/server/ready_state_tests.rs",
    "src/server/room_service_tests.rs",
    "src/server/session_policy_tests.rs",
    "src/server/signaling_tests.rs",
];

fn main() {
    println!("cargo::rustc-check-cfg=cfg(signal_fish_repository_tests)");
    println!("cargo::rerun-if-env-changed=SIGNAL_FISH_CARGO_CHEF_SKELETON");

    let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        eprintln!("Cargo did not provide CARGO_MANIFEST_DIR to the build script");
        std::process::exit(1);
    };
    let manifest_dir = Path::new(&manifest_dir);
    let package_markers = ["Cargo.toml.orig", ".cargo_vcs_info.json"];
    let generated_package = package_markers.iter().all(|path| {
        println!("cargo::rerun-if-changed={path}");
        manifest_dir.join(path).is_file()
    });
    let cargo_chef_skeleton = std::env::var_os("SIGNAL_FISH_CARGO_CHEF_SKELETON")
        .is_some_and(|value| value == "1")
        && std::env::var_os("CARGO_PKG_VERSION").is_some_and(|value| value == "0.0.1");
    let present = REPOSITORY_ONLY_TEST_MODULES
        .iter()
        .filter(|path| {
            println!("cargo::rerun-if-changed={path}");
            manifest_dir.join(path).is_file()
        })
        .count();

    match (present, generated_package, cargo_chef_skeleton) {
        (0, true, _) | (0, false, true) => {}
        (present, false, false) if present == REPOSITORY_ONLY_TEST_MODULES.len() => {
            println!("cargo::rustc-cfg=signal_fish_repository_tests");
        }
        (0, false, false) => {
            eprintln!("repository-only test modules are missing outside a Cargo-generated package");
            std::process::exit(1);
        }
        (present, _, _) => {
            eprintln!(
                "repository-only test modules must be either all present or all omitted; \
                 found {present} of {}",
                REPOSITORY_ONLY_TEST_MODULES.len()
            );
            std::process::exit(1);
        }
    }
}
