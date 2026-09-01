use std::collections::BTreeMap;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

pub(super) struct Filter {
    programs: [BpfProgram; 2],
}

impl Filter {
    pub(super) fn build() -> Result<Self, ()> {
        let architecture = architecture()?;
        let denied = compile(
            denied_syscalls()?,
            SeccompAction::Errno(libc::EPERM as u32),
            architecture,
        )?;
        let clone3 = compile(
            BTreeMap::from([(libc::SYS_clone3, Vec::new())]),
            SeccompAction::Errno(libc::ENOSYS as u32),
            architecture,
        )?;
        Ok(Self {
            programs: [denied, clone3],
        })
    }

    pub(super) fn apply(self) -> Result<(), ()> {
        for program in &self.programs {
            seccompiler::apply_filter(program).map_err(|_| ())?;
        }
        if rustix::thread::secure_computing_mode()
            != Ok(rustix::thread::SecureComputingMode::Filter)
        {
            return Err(());
        }
        Ok(())
    }
}

fn compile(
    rules: BTreeMap<i64, Vec<SeccompRule>>,
    matched: SeccompAction,
    architecture: TargetArch,
) -> Result<BpfProgram, ()> {
    SeccompFilter::new(rules, SeccompAction::Allow, matched, architecture)
        .map_err(|_| ())?
        .try_into()
        .map_err(|_| ())
}

fn architecture() -> Result<TargetArch, ()> {
    #[cfg(target_arch = "x86_64")]
    return Ok(TargetArch::x86_64);

    #[cfg(target_arch = "aarch64")]
    return Ok(TargetArch::aarch64);

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    Err(())
}

fn denied_syscalls() -> Result<BTreeMap<i64, Vec<SeccompRule>>, ()> {
    let mut rules = BTreeMap::new();
    rules.insert(libc::SYS_clone, namespace_clone_rules()?);
    for syscall in BLOCKED_SYSCALLS {
        rules.insert(*syscall, Vec::new());
    }
    #[cfg(target_arch = "x86_64")]
    rules.insert(libc::SYS_mknod, Vec::new());
    Ok(rules)
}

fn namespace_clone_rules() -> Result<Vec<SeccompRule>, ()> {
    [
        libc::CLONE_NEWCGROUP,
        libc::CLONE_NEWIPC,
        libc::CLONE_NEWNET,
        libc::CLONE_NEWNS,
        libc::CLONE_NEWPID,
        libc::CLONE_NEWTIME,
        libc::CLONE_NEWUSER,
        libc::CLONE_NEWUTS,
        libc::CLONE_PARENT,
    ]
    .into_iter()
    .map(|flag| {
        let flag = flag as u64;
        let condition = SeccompCondition::new(
            0,
            SeccompCmpArgLen::Qword,
            SeccompCmpOp::MaskedEq(flag),
            flag,
        )
        .map_err(|_| ())?;
        SeccompRule::new(vec![condition]).map_err(|_| ())
    })
    .collect()
}

const BLOCKED_SYSCALLS: &[i64] = &[
    libc::SYS_socket,
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_process_madvise,
    libc::SYS_kcmp,
    libc::SYS_pidfd_open,
    libc::SYS_pidfd_getfd,
    libc::SYS_pidfd_send_signal,
    libc::SYS_setpgid,
    libc::SYS_setsid,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_open_tree,
    libc::SYS_move_mount,
    libc::SYS_fsopen,
    libc::SYS_fsconfig,
    libc::SYS_fsmount,
    libc::SYS_mount_setattr,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_open_by_handle_at,
    libc::SYS_name_to_handle_at,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_reboot,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_setuid,
    libc::SYS_setgid,
    libc::SYS_setreuid,
    libc::SYS_setregid,
    libc::SYS_setresuid,
    libc::SYS_setresgid,
    libc::SYS_setgroups,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_keyctl,
    libc::SYS_bpf,
    libc::SYS_userfaultfd,
    libc::SYS_perf_event_open,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
    libc::SYS_mknodat,
    libc::SYS_memfd_secret,
    libc::SYS_msgget,
    libc::SYS_msgsnd,
    libc::SYS_msgrcv,
    libc::SYS_msgctl,
    libc::SYS_semget,
    libc::SYS_semop,
    libc::SYS_semtimedop,
    libc::SYS_semctl,
    libc::SYS_shmget,
    libc::SYS_shmat,
    libc::SYS_shmdt,
    libc::SYS_shmctl,
    libc::SYS_mq_open,
    libc::SYS_mq_unlink,
    libc::SYS_mq_timedsend,
    libc::SYS_mq_timedreceive,
    libc::SYS_mq_notify,
    libc::SYS_mq_getsetattr,
    libc::SYS_sethostname,
    libc::SYS_setdomainname,
    libc::SYS_personality,
    libc::SYS_syslog,
    libc::SYS_acct,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_quotactl,
    libc::SYS_settimeofday,
    libc::SYS_clock_settime,
    libc::SYS_adjtimex,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_compile_for_the_native_architecture() {
        let filter = Filter::build().expect("builds filters");
        assert!(filter.programs.iter().all(|program| !program.is_empty()));
    }

    #[test]
    fn namespaces_sockets_mounts_and_io_uring_are_blocked() {
        let rules = denied_syscalls().expect("builds rules");
        for syscall in [
            libc::SYS_socket,
            libc::SYS_unshare,
            libc::SYS_setns,
            libc::SYS_mount,
            libc::SYS_io_uring_setup,
        ] {
            assert!(rules.contains_key(&syscall));
        }
        assert!(!rules[&libc::SYS_clone].is_empty());
        assert!(!rules.contains_key(&libc::SYS_socketpair));
    }
}
