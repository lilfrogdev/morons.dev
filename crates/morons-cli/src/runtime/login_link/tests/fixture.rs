use super::Command;

pub(super) struct Helper {
    #[cfg(windows)]
    root: std::path::PathBuf,
}
impl Helper {
    pub(super) async fn new() -> Self {
        #[cfg(unix)]
        {
            Self {}
        }
        #[cfg(windows)]
        {
            use std::{fs, process::Stdio, time::Duration};
            use tokio::{io::AsyncWriteExt as _, time};
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).unwrap();
            let name = nonce.iter().map(|v| format!("{v:02x}")).collect::<String>();
            let root = std::env::temp_dir().join(format!("morons-link-fixture-{name}"));
            fs::create_dir(&root).unwrap();
            fence_windows::harden_private_directory(&root).unwrap();
            let helper = Self { root };
            // Compile fixture setup outside the production five-second readiness clock.
            // Cargo's existing stable Rust toolchain is the only compiler dependency.
            let mut compiler =
                Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
            compiler
                .args([
                    "--edition=2024",
                    "--crate-name=morons_link_fixture",
                    "-Dwarnings",
                    "-",
                ])
                .arg("-o")
                .arg(helper.root.join("fixture.exe"))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .creation_flags(0x0800_0000);
            let mut child = compiler.spawn().expect("fixture compiler should start");
            let result = time::timeout(Duration::from_secs(60), async {
                let mut input = child.stdin.take().unwrap();
                input
                    .write_all(include_bytes!("fixture_main.rs"))
                    .await
                    .unwrap();
                drop(input);
                child.wait().await.unwrap()
            })
            .await;
            if result.is_err() {
                let _ = time::timeout(Duration::from_secs(5), child.kill()).await;
            }
            assert!(
                result.expect("fixture compilation timed out").success(),
                "fixture compilation failed"
            );
            helper
        }
    }
    pub(super) fn command(&self, kind: &str) -> Command {
        #[cfg(unix)]
        {
            let script = match kind {
                "ok" => "exit 0",
                "reject" => "exit 1",
                "hold" => "exec sleep 30",
                "copy" => "printf '\\001'; cat >/dev/null",
                "bad_ack" => "printf x; cat >/dev/null",
                _ => panic!("unknown fixture"),
            };
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            command
        }
        #[cfg(windows)]
        {
            assert!(matches!(
                kind,
                "ok" | "reject" | "hold" | "copy" | "bad_ack"
            ));
            let mut command = Command::new(self.root.join("fixture.exe"));
            command.arg(kind);
            command
        }
    }
}
#[cfg(windows)]
impl Drop for Helper {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
