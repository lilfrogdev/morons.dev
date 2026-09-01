use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreated, RulesetCreatedAttr, RulesetStatus, make_bitflags,
};

const REQUIRED_ABI: ABI = ABI::V4;

pub(super) fn restrict() -> Result<(), ()> {
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(REQUIRED_ABI))
        .and_then(|ruleset| {
            ruleset.handle_access(make_bitflags!(AccessNet::{ BindTcp | ConnectTcp }))
        })
        .and_then(|ruleset| ruleset.create())
        .map_err(|_| ())?;

    let writable = AccessFs::from_all(REQUIRED_ABI);
    let executable = make_bitflags!(AccessFs::{ Execute | ReadFile | ReadDir });
    let readable = make_bitflags!(AccessFs::{ ReadFile });
    let device = make_bitflags!(AccessFs::{ ReadFile | WriteFile });

    for path in ["/workspace", "/home/morons", "/tmp", "/cargo"] {
        add_rule(&mut ruleset, path, writable)?;
    }
    for path in ["/image", "/usr", "/bin", "/lib", "/lib64", "/sbin"] {
        add_rule(&mut ruleset, path, executable)?;
    }
    if std::path::Path::new("/etc/ld.so.cache").exists() {
        add_rule(&mut ruleset, "/etc/ld.so.cache", readable)?;
    }
    for path in ["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"] {
        add_rule(&mut ruleset, path, device)?;
    }

    let status = ruleset.restrict_self().map_err(|_| ())?;
    if status.ruleset != RulesetStatus::FullyEnforced {
        return Err(());
    }
    Ok(())
}

fn add_rule(
    ruleset: &mut RulesetCreated,
    path: &str,
    access: landlock::BitFlags<AccessFs>,
) -> Result<(), ()> {
    let descriptor = PathFd::new(path).map_err(|_| ())?;
    ruleset
        .add_rule(PathBeneath::new(descriptor, access))
        .map(|_| ())
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_abi_handles_refer_truncate_and_network() {
        let filesystem = AccessFs::from_all(REQUIRED_ABI);
        assert!(filesystem.contains(AccessFs::Refer));
        assert!(filesystem.contains(AccessFs::Truncate));
        assert_eq!(REQUIRED_ABI, ABI::V4);
    }
}
