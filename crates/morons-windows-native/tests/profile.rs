#![cfg(windows)]

use std::{fs, io::Read, path::PathBuf, time::Duration};

use morons_windows_native::{
    CommandCompletion, CommandLaunch, CommandLimits, OperationPaths, OperationProfile,
};

#[test]
fn operation_profile_grants_only_typed_private_paths() {
    let mut operation_id = [0u8; 16];
    getrandom::fill(&mut operation_id).expect("operation ID entropy should be available");
    let root = private_test_root(operation_id);
    let candidate = root.join("candidate");
    let runtime = root.join("runtime");
    let image = root.join("image");
    let image_bin = image.join("bin");
    let executable = image_bin.join("fixture.exe");
    let temporary = runtime.join("tmp");
    let home = runtime.join("home");
    let local = runtime.join("local-app-data");
    let roaming = runtime.join("app-data");
    let public = runtime.join("public");
    let cargo = runtime.join("cargo-home");
    for directory in [
        &candidate, &temporary, &home, &local, &roaming, &public, &cargo, &image, &image_bin,
    ] {
        fs::create_dir_all(directory).expect("private test directory should be created");
    }
    fs::copy(
        std::env::current_exe().expect("test executable path should be available"),
        &executable,
    )
    .expect("private executable copy should be created");

    let profile = OperationProfile::create(operation_id)
        .expect("operation AppContainer profile should be created");
    profile
        .grant_operation(OperationPaths {
            candidate: &candidate,
            runtime: &runtime,
            image: &image,
        })
        .expect("operation-private ACL grants should be installed");
    let arguments = vec!["--morons-native-child".to_owned()];
    let mut process = profile
        .launch_command(CommandLaunch {
            executable: &executable,
            arguments: &arguments,
            candidate: &candidate,
            working_directory: &candidate,
            runtime: &runtime,
            image: &image,
            limits: CommandLimits {
                memory_bytes: 512 * 1024 * 1024,
                process_count: 8,
            },
        })
        .expect("command should launch suspended, join its Job, and resume");
    let mut stdout = process
        .take_stdout()
        .expect("stdout endpoint should be available exactly once");
    let mut stderr = process
        .take_stderr()
        .expect("stderr endpoint should be available exactly once");
    assert!(matches!(
        process
            .complete_and_verify(Duration::from_secs(10))
            .expect("command tree completion should be verified"),
        CommandCompletion::Clean { .. }
    ));
    let mut output = Vec::new();
    stdout
        .read_to_end(&mut output)
        .expect("stdout should close after command completion");
    stderr
        .read_to_end(&mut output)
        .expect("stderr should close after command completion");
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
