use super::*;
#[cfg(unix)]
use crate::provider::provider_cancellation;

#[test]
fn manifest_and_requirements_are_source_bound() {
    assert_eq!(
        REQUIREMENTS_INPUT,
        "jupyter_client==8.6.3\nipykernel==6.30.1\n"
    );
    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(REQUIREMENTS.as_bytes()) {
        write!(&mut digest, "{byte:02x}").expect("digest should encode");
    }
    assert_eq!(digest, REQUIREMENTS_SHA256);
    let mut assets = UV_ASSETS.lines();
    assert_eq!(assets.next(), Some("version 0.12.9"));
    let assets = assets
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>();
    assert_eq!(assets.len(), 6);
    assert!(assets.iter().all(|line| {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        fields.len() == 4
            && fields[1..=2].iter().all(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            && matches!(fields[3], "tar.gz" | "zip")
    }));
    assert!(expected_uv_sha256().is_some());
    let manifest = runtime_manifest();
    assert!(manifest.contains("uv=0.12.9"));
    assert!(manifest.contains("python=3.11.15"));
    assert!(manifest.contains("jupyter_client=8.6.3"));
    assert!(manifest.contains("ipykernel=6.30.1"));
}

#[cfg(unix)]
#[test]
fn managed_runtime_is_locked_atomic_cached_and_rebuildable() {
    let root = test_root("managed-runtime");
    let log = root.join("uv.log");
    let uv = root.join("fake-uv");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf 'call\n' >> '{}'
mode=''
install_dir=''
last=''
previous=''
for argument in "$@"; do
  if [ "$previous" = "--install-dir" ]; then install_dir=$argument; fi
  if [ "$argument" = "install" ] && [ "$previous" = "python" ]; then mode=python; fi
  if [ "$argument" = "venv" ]; then mode=venv; fi
  if [ "$argument" = "pip" ]; then mode=pip; fi
  previous=$argument
  last=$argument
done
case "$mode" in
  python)
    mkdir -p "$install_dir/cpython-3.11.15-test/bin"
    cat > "$install_dir/cpython-3.11.15-test/bin/python3.11" <<'PY'
#!/bin/sh
exit 0
PY
    chmod 700 "$install_dir/cpython-3.11.15-test/bin/python3.11"
    ;;
  venv)
    mkdir -p "$last/bin"
    cat > "$last/bin/python" <<'PY'
#!/bin/sh
exit 0
PY
    chmod 700 "$last/bin/python"
    ;;
  pip) ;;
  *) exit 2 ;;
esac
"#,
        log.display()
    );
    fs::write(&uv, script).expect("fake uv should be written");
    fs::set_permissions(&uv, fs::Permissions::from_mode(0o700))
        .expect("fake uv should be executable");
    let runtime = ManagedPythonRuntime::with_uv(root.join("python"), uv.clone());
    let (_, cancellation) = provider_cancellation();
    let executable = runtime
        .ensure(&cancellation)
        .expect("runtime should prepare");
    assert!(PathBuf::from(&executable).exists());
    assert_eq!(
        fs::read_to_string(&log)
            .expect("log should read")
            .lines()
            .count(),
        3
    );
    assert_eq!(
        runtime.ensure(&cancellation).expect("runtime should cache"),
        executable
    );
    assert_eq!(
        fs::read_to_string(&log)
            .expect("log should read")
            .lines()
            .count(),
        3
    );

    fs::write(
        root.join("python")
            .join(RUNTIME_DIRECTORY)
            .join(MANIFEST_FILE),
        "stale\n",
    )
    .expect("manifest should become stale");
    let rebuilt = ManagedPythonRuntime::with_uv(root.join("python"), uv);
    rebuilt
        .ensure(&cancellation)
        .expect("stale runtime should rebuild");
    assert_eq!(
        fs::read_to_string(&log)
            .expect("log should read")
            .lines()
            .count(),
        6
    );
    fs::remove_dir_all(root).expect("test root should be removed");
}

#[cfg(unix)]
#[test]
fn managed_bootstrap_cancellation_stops_before_publication() {
    let root = test_root("cancelled-runtime");
    let uv = root.join("fake-uv");
    fs::write(&uv, "#!/bin/sh\nsleep 60\n").expect("fake uv should be written");
    fs::set_permissions(&uv, fs::Permissions::from_mode(0o700))
        .expect("fake uv should be executable");
    let runtime_root = root.join("python");
    let runtime = ManagedPythonRuntime::with_uv(runtime_root.clone(), uv);
    let (handle, cancellation) = provider_cancellation();
    let cancellation_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        handle.cancel();
    });
    assert_eq!(
        runtime
            .ensure(&cancellation)
            .expect_err("bootstrap should cancel"),
        ToolErrorKind::Cancelled
    );
    cancellation_thread
        .join()
        .expect("cancellation thread should join");
    assert!(!runtime_root.join(RUNTIME_DIRECTORY).exists());
    fs::remove_dir_all(root).expect("test root should be removed");
}

#[cfg(unix)]
#[test]
fn failed_bootstrap_never_publishes_a_runtime() {
    let root = test_root("failed-runtime");
    let uv = root.join("fake-uv");
    fs::write(&uv, "#!/bin/sh\nexit 1\n").expect("fake uv should be written");
    fs::set_permissions(&uv, fs::Permissions::from_mode(0o700))
        .expect("fake uv should be executable");
    let runtime_root = root.join("python");
    let runtime = ManagedPythonRuntime::with_uv(runtime_root.clone(), uv);
    let (_, cancellation) = provider_cancellation();
    assert_eq!(
        runtime
            .ensure(&cancellation)
            .expect_err("bootstrap should fail"),
        ToolErrorKind::KernelUnavailable
    );
    assert!(!runtime_root.join(RUNTIME_DIRECTORY).exists());
    fs::remove_dir_all(root).expect("test root should be removed");
}

#[cfg(unix)]
fn test_root(label: &str) -> PathBuf {
    let mut encoded = [0_u8; 8];
    getrandom::fill(&mut encoded).expect("test randomness should be available");
    let root = env::temp_dir().join(format!(
        "morons-{label}-{}-{}",
        std::process::id(),
        u64::from_be_bytes(encoded)
    ));
    fs::create_dir(&root).expect("test root should be created");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("test root should be private");
    root
}
