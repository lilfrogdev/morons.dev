use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use rustix::{
    io::Errno,
    mount::{MountFlags, MountPropagationFlags},
    process::{Gid, Resource, Rlimit, Signal, Uid, set_parent_process_death_signal},
    thread::{
        CapabilitiesSecureBits, CapabilitySet, CapabilitySets, UnshareFlags, capabilities,
        capability_is_in_bounding_set, clear_ambient_capability_set, no_new_privs,
        remove_capability_from_bounding_set, set_capabilities, set_capabilities_secure_bits,
        set_no_new_privs, set_thread_res_gid, set_thread_res_uid,
    },
};

use crate::{SandboxRequest, runner::PreparedRequest};

const FAILED_BYTES: &[u8] = b"failed\n";
const ADDRESS_SPACE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const FILE_BYTES: u64 = 512 * 1024 * 1024;
const OPEN_FILES: u64 = 256;
const PROCESS_LIMIT: u64 = 256;
const PENDING_SIGNALS: u64 = 256;
const STACK_BYTES: u64 = 64 * 1024 * 1024;

pub(super) struct Layout {
    pub(super) root: PathBuf,
    pub(super) ready: PathBuf,
    pub(super) outcome: PathBuf,
    pub(super) failure: PathBuf,
    home: PathBuf,
    temporary: PathBuf,
    cargo_home: PathBuf,
}

impl Layout {
    pub(super) fn prepare(request: &PreparedRequest) -> Result<Self, ()> {
        let identifier = hexadecimal(&request.operation_id);
        let root = request
            .scratch_root
            .join(format!(".morons-linux-{identifier}"));
        create_private_directory(&root, true)?;
        let prepared = (|| {
            let ready = root.join(".morons-ready");
            let outcome = root.join(".morons-outcome");
            let failure = root.join(".morons-failure");
            let home = request.scratch_root.join("home");
            let temporary = request.scratch_root.join("tmp");
            let cargo_home = request.scratch_root.join("cargo-home");
            for path in [&home, &temporary, &cargo_home] {
                create_private_directory(path, false)?;
            }
            crate::runner::seed_cargo_home(&request.image_root, &cargo_home)?;
            if ready.exists() || outcome.exists() || failure.exists() {
                return Err(());
            }
            Ok(Self {
                root: root.clone(),
                ready,
                outcome,
                failure,
                home,
                temporary,
                cargo_home,
            })
        })();
        if prepared.is_err() {
            let _ = fs::remove_dir_all(&root);
        }
        prepared
    }

    pub(super) fn existing(request: &PreparedRequest) -> Result<Self, ()> {
        let identifier = hexadecimal(&request.operation_id);
        let root = request
            .scratch_root
            .join(format!(".morons-linux-{identifier}"));
        let layout = Self {
            ready: root.join(".morons-ready"),
            outcome: root.join(".morons-outcome"),
            failure: root.join(".morons-failure"),
            home: request.scratch_root.join("home"),
            temporary: request.scratch_root.join("tmp"),
            cargo_home: request.scratch_root.join("cargo-home"),
            root,
        };
        for path in [
            &layout.root,
            &layout.home,
            &layout.temporary,
            &layout.cargo_home,
        ] {
            validate_private_directory(path)?;
        }
        Ok(layout)
    }

    pub(super) fn cleanup(&self) -> Result<(), ()> {
        fs::remove_dir_all(&self.root).map_err(|_| ())
    }
}

pub(super) fn prepared_command_matches(
    prepared: &PreparedRequest,
    request: &SandboxRequest,
) -> bool {
    let expected_executable = prepared.image_root.join(&request.executable);
    let expected_working_directory = if request.working_directory == "." {
        prepared.candidate_root.clone()
    } else {
        prepared.candidate_root.join(&request.working_directory)
    };
    prepared.executable == expected_executable
        && prepared.arguments == request.arguments
        && prepared.working_directory == expected_working_directory
}

pub(super) fn create_namespaces(
    before_user: u64,
    before_mount: u64,
    before_network: u64,
) -> Result<(), ()> {
    let host_uid = rustix::process::geteuid().as_raw();
    let host_gid = rustix::process::getegid().as_raw();
    unshare_single_threaded(UnshareFlags::NEWUSER)?;
    fs::write("/proc/self/setgroups", b"deny\n").map_err(|_| ())?;
    fs::write("/proc/self/uid_map", format!("0 {host_uid} 1\n")).map_err(|_| ())?;
    fs::write("/proc/self/gid_map", format!("0 {host_gid} 1\n")).map_err(|_| ())?;
    set_thread_res_gid(Gid::ROOT, Gid::ROOT, Gid::ROOT).map_err(|_| ())?;
    set_thread_res_uid(Uid::ROOT, Uid::ROOT, Uid::ROOT).map_err(|_| ())?;
    if !rustix::process::geteuid().is_root()
        || !rustix::process::getegid().is_root()
        || namespace_identity("user")? == before_user
    {
        return Err(());
    }

    unshare_single_threaded(
        UnshareFlags::NEWNS
            | UnshareFlags::NEWPID
            | UnshareFlags::NEWNET
            | UnshareFlags::NEWIPC
            | UnshareFlags::NEWUTS,
    )?;
    rustix::system::sethostname(b"morons-sandbox").map_err(|_| ())?;
    rustix::system::setdomainname(b"morons-sandbox").map_err(|_| ())?;
    if namespace_identity("mnt")? == before_mount
        || namespace_identity("net")? == before_network
        || !network_namespace_is_empty()?
    {
        return Err(());
    }
    Ok(())
}

#[allow(deprecated)]
fn unshare_single_threaded(flags: UnshareFlags) -> Result<(), ()> {
    // This stage is entered before it creates threads, satisfying rustix's descriptor-table concern.
    rustix::thread::unshare(flags).map_err(|_| ())
}

pub(super) fn bind_to_parent(expected_parent: u32) -> Result<(), ()> {
    let current_parent = rustix::process::getppid()
        .and_then(|pid| u32::try_from(pid.as_raw_nonzero().get()).ok())
        .ok_or(())?;
    if current_parent != expected_parent {
        return Err(());
    }
    set_parent_process_death_signal(Some(Signal::KILL)).map_err(|_| ())?;
    let current_parent = rustix::process::getppid()
        .and_then(|pid| u32::try_from(pid.as_raw_nonzero().get()).ok())
        .ok_or(())?;
    if current_parent != expected_parent
        || rustix::process::parent_process_death_signal() != Ok(Some(Signal::KILL))
    {
        return Err(());
    }
    Ok(())
}

pub(super) fn namespace_identity(name: &str) -> Result<u64, ()> {
    let target = fs::read_link(format!("/proc/self/ns/{name}")).map_err(|_| ())?;
    let target = target.to_str().ok_or(())?;
    let value = target
        .strip_prefix(&format!("{name}:["))
        .and_then(|value| value.strip_suffix(']'))
        .ok_or(())?;
    value.parse().map_err(|_| ())
}

fn network_namespace_is_empty() -> Result<bool, ()> {
    let devices = fs::read_to_string("/proc/net/dev").map_err(|_| ())?;
    let interfaces = devices
        .lines()
        .skip(2)
        .map(|line| line.split_once(':').map(|(name, _)| name.trim()))
        .collect::<Option<Vec<_>>>()
        .ok_or(())?;
    Ok(interfaces == ["lo"])
}

pub(super) fn setup_mounts(
    request: &PreparedRequest,
    layout: &Layout,
    current_executable: &Path,
) -> Result<(), ()> {
    rustix::mount::mount_change(
        "/",
        MountPropagationFlags::PRIVATE | MountPropagationFlags::REC,
    )
    .map_err(|_| ())?;
    prepare_root_layout(&layout.root)?;
    bind_directory(
        &request.candidate_root,
        &layout.root.join("workspace"),
        false,
    )?;
    bind_directory(&request.image_root, &layout.root.join("image"), true)?;
    bind_directory(&layout.home, &layout.root.join("home/morons"), false)?;
    bind_directory(&layout.temporary, &layout.root.join("tmp"), false)?;
    bind_directory(&layout.cargo_home, &layout.root.join("cargo"), false)?;
    for runtime in ["bin", "lib"] {
        let source = Path::new("/usr").join(runtime);
        bind_directory(&source, &layout.root.join("usr").join(runtime), true)?;
        bind_directory(&source, &layout.root.join(runtime), true)?;
    }
    for runtime in ["lib64", "sbin"] {
        let source = Path::new("/usr").join(runtime);
        if source.is_dir() {
            bind_directory(&source, &layout.root.join("usr").join(runtime), true)?;
            bind_directory(&source, &layout.root.join(runtime), true)?;
        }
    }
    let libexec = Path::new("/usr/libexec");
    if libexec.is_dir() {
        bind_directory(libexec, &layout.root.join("usr/libexec"), true)?;
    }
    bind_file(current_executable, &layout.root.join("runner"), true)?;
    if Path::new("/etc/ld.so.cache").is_file() {
        bind_file(
            Path::new("/etc/ld.so.cache"),
            &layout.root.join("etc/ld.so.cache"),
            true,
        )?;
    }
    for device in ["null", "zero", "random", "urandom"] {
        bind_device(
            &Path::new("/dev").join(device),
            &layout.root.join("dev").join(device),
        )?;
    }
    Ok(())
}

pub(super) fn mount_proc(root: &Path) -> Result<(), ()> {
    rustix::mount::mount(
        "proc",
        root.join("proc"),
        "proc",
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
        Some(c"hidepid=2,subset=pid"),
    )
    .map_err(|_| ())
}

fn prepare_root_layout(root: &Path) -> Result<(), ()> {
    for path in [
        root.join("workspace"),
        root.join("image"),
        root.join("home"),
        root.join("home/morons"),
        root.join("tmp"),
        root.join("cargo"),
        root.join("bin"),
        root.join("lib"),
        root.join("lib64"),
        root.join("sbin"),
        root.join("usr"),
        root.join("usr/bin"),
        root.join("usr/lib"),
        root.join("usr/lib64"),
        root.join("usr/libexec"),
        root.join("usr/sbin"),
        root.join("etc"),
        root.join("dev"),
        root.join("proc"),
    ] {
        create_private_directory(&path, true)?;
    }
    for path in [
        root.join("runner"),
        root.join("etc/ld.so.cache"),
        root.join("dev/null"),
        root.join("dev/zero"),
        root.join("dev/random"),
        root.join("dev/urandom"),
    ] {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| ())?;
    }
    Ok(())
}

fn bind_directory(source: &Path, target: &Path, read_only: bool) -> Result<(), ()> {
    rustix::mount::mount_bind(source, target).map_err(|_| ())?;
    let mut flags = MountFlags::BIND | MountFlags::NOSUID | MountFlags::NODEV;
    if read_only {
        flags |= MountFlags::RDONLY;
    }
    rustix::mount::mount_remount(target, flags, "").map_err(|_| ())
}

fn bind_file(source: &Path, target: &Path, read_only: bool) -> Result<(), ()> {
    rustix::mount::mount_bind(source, target).map_err(|_| ())?;
    let mut flags = MountFlags::BIND | MountFlags::NOSUID | MountFlags::NODEV;
    if read_only {
        flags |= MountFlags::RDONLY;
    }
    rustix::mount::mount_remount(target, flags, "").map_err(|_| ())
}

fn bind_device(source: &Path, target: &Path) -> Result<(), ()> {
    rustix::mount::mount_bind(source, target).map_err(|_| ())
}

pub(super) fn drop_capabilities() -> Result<(), ()> {
    clear_ambient_capability_set().map_err(|_| ())?;
    set_capabilities_secure_bits(
        CapabilitiesSecureBits::NO_ROOT
            | CapabilitiesSecureBits::NO_ROOT_LOCKED
            | CapabilitiesSecureBits::NO_SETUID_FIXUP
            | CapabilitiesSecureBits::NO_SETUID_FIXUP_LOCKED
            | CapabilitiesSecureBits::NO_CAP_AMBIENT_RAISE
            | CapabilitiesSecureBits::NO_CAP_AMBIENT_RAISE_LOCKED,
    )
    .map_err(|_| ())?;
    for capability in all_capabilities() {
        match remove_capability_from_bounding_set(capability) {
            Ok(()) | Err(Errno::INVAL) => {}
            Err(_) => return Err(()),
        }
    }
    set_capabilities(
        None,
        CapabilitySets {
            effective: CapabilitySet::empty(),
            permitted: CapabilitySet::empty(),
            inheritable: CapabilitySet::empty(),
        },
    )
    .map_err(|_| ())?;
    set_no_new_privs(true).map_err(|_| ())?;
    verify_capabilities_dropped()
}

pub(super) fn verify_capabilities_dropped() -> Result<(), ()> {
    let sets = capabilities(None).map_err(|_| ())?;
    if !sets.effective.is_empty() || !sets.permitted.is_empty() || !sets.inheritable.is_empty() {
        return Err(());
    }
    for capability in all_capabilities() {
        match capability_is_in_bounding_set(capability) {
            Ok(false) | Err(Errno::INVAL) => {}
            Ok(true) | Err(_) => return Err(()),
        }
    }
    if no_new_privs() != Ok(true) {
        return Err(());
    }
    Ok(())
}

pub(super) fn apply_limits(wall_time_milliseconds: u64) -> Result<(), ()> {
    let cpu_seconds = wall_time_milliseconds.div_ceil(1_000).max(1);
    for (resource, value) in [
        (Resource::Cpu, cpu_seconds),
        (Resource::As, ADDRESS_SPACE_BYTES),
        (Resource::Fsize, FILE_BYTES),
        (Resource::Nofile, OPEN_FILES),
        (Resource::Nproc, PROCESS_LIMIT),
        (Resource::Sigpending, PENDING_SIGNALS),
        (Resource::Stack, STACK_BYTES),
        (Resource::Core, 0),
        (Resource::Memlock, 0),
        (Resource::Msgqueue, 0),
    ] {
        rustix::process::setrlimit(
            resource,
            Rlimit {
                current: Some(value),
                maximum: Some(value),
            },
        )
        .map_err(|_| ())?;
    }
    Ok(())
}

pub(super) fn validate_synthetic_target(path: &Path, directory: bool) -> Result<(), ()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.file_type().is_symlink()
        || directory && !metadata.file_type().is_dir()
        || !directory
            && (!metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0)
    {
        return Err(());
    }
    Ok(())
}

pub(super) fn write_failed_marker(marker: &mut File) -> Result<(), ()> {
    marker.seek(SeekFrom::Start(0)).map_err(|_| ())?;
    marker.set_len(0).map_err(|_| ())?;
    marker.write_all(FAILED_BYTES).map_err(|_| ())?;
    marker.sync_all().map_err(|_| ())
}

pub(super) fn trusted_current_executable() -> Result<PathBuf, ()> {
    let executable = fs::canonicalize(std::env::current_exe().map_err(|_| ())?).map_err(|_| ())?;
    let metadata = fs::symlink_metadata(&executable).map_err(|_| ())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(());
    }
    Ok(executable)
}

fn create_private_directory(path: &Path, exclusive: bool) -> Result<(), ()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if !exclusive && error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(()),
    }
    validate_private_directory(path)
}

fn validate_private_directory(path: &Path) -> Result<(), ()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(());
    }
    Ok(())
}

fn hexadecimal(identifier: &[u8; 16]) -> String {
    identifier
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn all_capabilities() -> impl Iterator<Item = CapabilitySet> {
    (0..u64::BITS).map(|bit| CapabilitySet::from_bits_retain(1_u64 << bit))
}
