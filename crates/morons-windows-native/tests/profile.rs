#![cfg(windows)]

use std::{fs, path::PathBuf};

use morons_windows_native::{OperationPaths, OperationProfile};

#[test]
fn operation_profile_grants_only_typed_private_paths() {
    let mut operation_id = [0u8; 16];
    getrandom::fill(&mut operation_id).expect("operation ID entropy should be available");
    let root = private_test_root(operation_id);
    let candidate = root.join("candidate");
    let runtime = root.join("runtime");
    let image = root.join("image");
    let bootstrap = root.join("bootstrap.exe");
    for directory in [&candidate, &runtime, &image] {
        fs::create_dir_all(directory).expect("private test directory should be created");
    }
    fs::write(&bootstrap, b"not executable").expect("bootstrap fixture should be created");

    let profile = OperationProfile::create(operation_id)
        .expect("operation AppContainer profile should be created");
    profile
        .grant_operation(OperationPaths {
            candidate: &candidate,
            runtime: &runtime,
            image: &image,
            bootstrap: &bootstrap,
        })
        .expect("operation-private ACL grants should be installed");
    profile
        .delete()
        .expect("operation AppContainer profile should be deleted");
    fs::remove_dir_all(root).expect("private test staging should be removed");
}

fn private_test_root(operation_id: [u8; 16]) -> PathBuf {
    let suffix = operation_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    std::env::temp_dir().join(format!("morons-windows-native-{suffix}"))
}
