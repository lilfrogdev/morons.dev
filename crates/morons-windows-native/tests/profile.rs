#![cfg(windows)]

use std::{fs, path::PathBuf, time::Duration};

use morons_windows_native::{BootstrapLaunch, BootstrapLimits, OperationPaths, OperationProfile};

#[test]
fn operation_profile_grants_only_typed_private_paths() {
    let mut operation_id = [0u8; 16];
    getrandom::fill(&mut operation_id).expect("operation ID entropy should be available");
    let root = private_test_root(operation_id);
    let candidate = root.join("candidate");
    let runtime = root.join("runtime");
    let image = root.join("image");
    let bootstrap = image.join("bootstrap.exe");
    let temporary = runtime.join("tmp");
    let control = runtime.join("control");
    for directory in [&candidate, &temporary, &control, &image] {
        fs::create_dir_all(directory).expect("private test directory should be created");
    }
    fs::copy(
        std::env::current_exe().expect("test executable path should be available"),
        &bootstrap,
    )
    .expect("private bootstrap copy should be created");
    let input = control.join("input");
    let output = control.join("output");
    let gate = control.join("gate");
    let done = control.join("done");
    fs::write(&input, b"test").expect("input fixture should be created");
    fs::write(&output, b"").expect("output fixture should be created");

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
    let process = profile
        .launch_bootstrap(BootstrapLaunch {
            executable: &bootstrap,
            working_directory: &candidate,
            runtime: &runtime,
            input: &input,
            output: &output,
            gate: &gate,
            done: &done,
            limits: BootstrapLimits {
                memory_bytes: 512 * 1024 * 1024,
                process_count: 8,
            },
        })
        .expect("bootstrap should launch suspended, join its Job, and resume");
    assert_ne!(process.id(), 0);
    let _ = process
        .wait_root(Duration::from_secs(10))
        .expect("bootstrap wait should remain observable");
    process
        .terminate_and_verify(Duration::from_secs(2))
        .expect("bootstrap Job should terminate with no active members");
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
