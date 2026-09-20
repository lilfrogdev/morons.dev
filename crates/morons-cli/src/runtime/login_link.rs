mod commands;
#[cfg(test)]
mod tests;

use crate::login_link::{LinkAction, LinkEvent, READY};
use morons_protocol::OpenAiAuthorizationUrl;
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};

const START_TIMEOUT: Duration = Duration::from_secs(5);
const HOLD_TIMEOUT: Duration = Duration::from_secs(680);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct LinkRuntime {
    pub(super) events: mpsc::Receiver<LinkEvent>,
    sender: mpsc::Sender<LinkEvent>,
    browser: Option<Job>,
    clipboard: Option<Job>,
}
struct Job {
    scope: Arc<()>,
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
}
impl Default for LinkRuntime {
    fn default() -> Self {
        let (sender, events) = mpsc::channel(4);
        Self {
            sender,
            events,
            browser: None,
            clipboard: None,
        }
    }
}
impl Drop for LinkRuntime {
    fn drop(&mut self) {
        for job in [&self.browser, &self.clipboard].into_iter().flatten() {
            job.cancel.send_replace(true);
            job.task.abort();
        }
    }
}
impl LinkRuntime {
    pub(super) async fn copy_selection(&mut self, text: String) {
        if let Some(job) = self.clipboard.take() {
            drain(job).await;
        }
        let (cancel, cancellation) = watch::channel(false);
        let scope = Arc::new(());
        let task_scope = Arc::clone(&scope);
        let sender = self.sender.clone();
        let task = tokio::spawn(async move {
            let text = zeroize::Zeroizing::new(text);
            let result = match std::env::current_exe() {
                Ok(executable) => {
                    let mut command = Command::new(executable);
                    command.arg(crate::login_link::SELECTION_HELPER);
                    run_text(command, LinkAction::Copy, &text, cancellation, || {
                        let _ = sender.try_send(LinkEvent {
                            scope: Arc::clone(&task_scope),
                            message: "Copied",
                        });
                    })
                    .await
                }
                Err(_) => Err("Copy failed: cannot locate CLI executable"),
            };
            if let Err(message) = result {
                crate::clipboard_log::record(message);
            }
        });
        self.clipboard = Some(Job {
            scope,
            cancel,
            task,
        });
    }
    pub(super) fn reconcile(&self, scope: Option<&Arc<()>>) {
        for job in [&self.browser, &self.clipboard].into_iter().flatten() {
            if scope.is_none_or(|scope| !Arc::ptr_eq(scope, &job.scope)) {
                job.cancel.send_replace(true);
            }
        }
    }
    pub(super) async fn start(
        &mut self,
        action: LinkAction,
        scope: Arc<()>,
        url: OpenAiAuthorizationUrl,
    ) {
        let command = match action {
            LinkAction::Open => commands::browser(&url),
            LinkAction::Copy => commands::clipboard(),
        };
        self.start_with(action, scope, url, command).await;
    }
    async fn start_with(
        &mut self,
        action: LinkAction,
        scope: Arc<()>,
        url: OpenAiAuthorizationUrl,
        command: Result<Command, ()>,
    ) {
        let slot = match action {
            LinkAction::Open => &mut self.browser,
            LinkAction::Copy => &mut self.clipboard,
        };
        if action == LinkAction::Open
            && slot
                .as_ref()
                .is_some_and(|job| Arc::ptr_eq(&job.scope, &scope) && !job.task.is_finished())
        {
            return;
        }
        if let Some(job) = slot.take() {
            drain(job).await;
        }
        let (cancel, cancellation) = watch::channel(false);
        let sender = self.sender.clone();
        let task_scope = Arc::clone(&scope);
        let task = tokio::spawn(async move {
            let result = match command {
                Ok(command) => {
                    run(command, action, &url, cancellation, || {
                        let _ = sender.try_send(LinkEvent {
                            scope: Arc::clone(&task_scope),
                            message: success(action),
                        });
                    })
                    .await
                }
                Err(()) => Err(()),
            };
            if result.is_err() {
                let _ = sender.try_send(LinkEvent {
                    scope: task_scope,
                    message: failure(action),
                });
            }
        });
        *slot = Some(Job {
            scope,
            cancel,
            task,
        });
    }
    pub(super) async fn shutdown(&mut self) {
        self.reconcile(None);
        if let Some(job) = self.browser.take() {
            drain(job).await;
        }
        if let Some(job) = self.clipboard.take() {
            drain(job).await;
        }
    }
}
async fn drain(job: Job) {
    job.cancel.send_replace(true);
    let mut task = job.task;
    if time::timeout(DRAIN_TIMEOUT, &mut task).await.is_err() {
        task.abort();
        let _ = task.await;
    }
}
fn success(action: LinkAction) -> &'static str {
    match action {
        LinkAction::Open => {
            "Browser request handed off. If no window appeared, use Open browser or Copy link."
        }
        LinkAction::Copy => {
            "Full login link copied. Paste it into your browser; clipboard managers may retain it."
        }
    }
}
fn failure(action: LinkAction) -> &'static str {
    match action {
        LinkAction::Open => {
            "Browser opening failed or is uncertain. Use Open browser or Copy link; login was not retried."
        }
        LinkAction::Copy => {
            "Copy failed or is uncertain; the clipboard may have changed. Use Open browser or try Copy link."
        }
    }
}
async fn run(
    command: Command,
    action: LinkAction,
    url: &OpenAiAuthorizationUrl,
    cancel: watch::Receiver<bool>,
    ready: impl FnOnce(),
) -> Result<(), ()> {
    run_text(command, action, url.as_str(), cancel, ready)
        .await
        .map_err(|_| ())
}

async fn run_text(
    mut command: Command,
    action: LinkAction,
    text: &str,
    mut cancel: watch::Receiver<bool>,
    ready: impl FnOnce(),
) -> Result<(), &'static str> {
    if *cancel.borrow() {
        return Ok(());
    }
    command.kill_on_drop(true).stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW for owned desktop helpers.
    match action {
        LinkAction::Open => {
            command.stdin(Stdio::null()).stdout(Stdio::null());
        }
        LinkAction::Copy => {
            command.stdin(Stdio::piped()).stdout(Stdio::piped());
        }
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Copy failed: helper could not start")?;
    let started = tokio::select! {
        biased;
        _ = cancel.changed() => {stop(&mut child).await;return Ok(());}
        result = time::timeout(START_TIMEOUT, async {
            match action {
                LinkAction::Open => child.wait().await.map_err(|_| "Browser wait failed").and_then(|s| s.success().then_some(()).ok_or("Browser exited unsuccessfully")),
                LinkAction::Copy => {
                    let input = child.stdin.as_mut().ok_or("Copy failed: helper input unavailable")?;
                    input.write_all(&(text.len() as u32).to_be_bytes()).await.map_err(|_| "Copy uncertain: helper frame write failed")?;
                    input.write_all(text.as_bytes()).await.map_err(|_| "Copy uncertain: helper text write failed")?;
                    input.flush().await.map_err(|_| "Copy uncertain: helper input flush failed")?;
                    let mut ack = [0];
                    child.stdout.as_mut().ok_or("Copy failed: helper output unavailable")?.read_exact(&mut ack).await.map_err(|_| "Copy uncertain: helper closed before acknowledgment")?;
                    copy_acknowledgment(ack[0])
                }
            }
        }) => result.map_err(|_| "Copy uncertain: helper startup timed out").and_then(|result| result),
    };
    if let Err(error) = started {
        stop(&mut child).await;
        return Err(error);
    }
    if *cancel.borrow() {
        stop(&mut child).await;
        return Ok(());
    }
    ready();
    if action == LinkAction::Copy {
        // Child::wait closes its stdin; retain the ownership pipe separately while waiting.
        let ownership = child.stdin.take();
        tokio::select! {
            biased;
            _ = cancel.changed() => {}
            _ = time::sleep(HOLD_TIMEOUT) => {}
            _ = child.wait() => return Err("Clipboard ownership ended: helper exited after copy"),
        }
        drop(ownership);
        stop(&mut child).await;
    }
    Ok(())
}
fn copy_acknowledgment(code: u8) -> Result<(), &'static str> {
    match code {
        READY => Ok(()),
        crate::login_link::INVALID_SELECTION => Err("Copy failed: helper rejected selection"),
        crate::login_link::CLIPBOARD_CONNECT_FAILED => {
            Err("Copy failed: clipboard connection failed")
        }
        crate::login_link::CLIPBOARD_WRITE_FAILED => Err("Copy uncertain: clipboard write failed"),
        _ => Err("Copy uncertain: invalid helper acknowledgment"),
    }
}

async fn stop(child: &mut Child) {
    let _ = child.start_kill();
    let _ = time::timeout(DRAIN_TIMEOUT, child.wait()).await;
}
