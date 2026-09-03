mod safety;

use std::{
    io::{self, IsTerminal, Stdout, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use ratatui::Terminal;
use ratatui_crossterm::{
    CrosstermBackend,
    crossterm::{
        cursor::{Hide, Show},
        event::{
            self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent,
            KeyEventKind, KeyModifiers,
        },
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

pub(crate) use safety::is_bidirectional_control;
pub use safety::{CredentialBuffer, MAX_PROMPT_BYTES, PromptBuffer, SafeText};

const TERMINAL_EVENT_QUEUE_CAPACITY: usize = 16;
const TERMINAL_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_PASTE_BYTES: usize = MAX_PROMPT_BYTES;

type AppTerminal = Terminal<CrosstermBackend<Stdout>>;

pub enum TerminalInput {
    Key(KeyEvent),
    Paste(Zeroizing<String>),
    Image(morons_image::NormalizedImage),
    ClipboardUnavailable,
    Resize,
}

impl std::fmt::Debug for TerminalInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Key(_) => formatter.write_str("TerminalInput::Key([REDACTED])"),
            Self::Paste(paste) => formatter
                .debug_struct("TerminalInput::Paste")
                .field("paste_bytes", &paste.len())
                .finish(),
            Self::Image(image) => formatter
                .debug_struct("TerminalInput::Image")
                .field("media_type", &image.media_type)
                .field("width", &image.width)
                .field("height", &image.height)
                .field("bytes", &image.bytes.len())
                .finish(),
            Self::ClipboardUnavailable => {
                formatter.write_str("TerminalInput::ClipboardUnavailable")
            }
            Self::Resize => formatter.write_str("TerminalInput::Resize"),
        }
    }
}

pub fn require_interactive_terminal() -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "morons requires interactive terminal input and output",
        ));
    }
    Ok(())
}

pub struct TerminalSession {
    terminal: AppTerminal,
    restored: bool,
}

impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let mut stdout = io::stdout();
                let _ = execute!(stdout, Show, DisableBracketedPaste, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                return Err(error);
            }
        };
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    pub fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut ratatui::Frame<'_>),
    {
        self.terminal.draw(render).map(|_| ())
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        let mut first_error = None;
        if let Err(error) = self.terminal.show_cursor() {
            first_error = Some(error);
        }
        if let Err(error) = execute!(
            self.terminal.backend_mut(),
            Show,
            DisableBracketedPaste,
            LeaveAlternateScreen
        ) && first_error.is_none()
        {
            first_error = Some(error);
        }
        if let Err(error) = disable_raw_mode()
            && first_error.is_none()
        {
            first_error = Some(error);
        }
        if let Err(error) = self.terminal.backend_mut().flush()
            && first_error.is_none()
        {
            first_error = Some(error);
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub struct TerminalEvents {
    receiver: mpsc::Receiver<io::Result<TerminalInput>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TerminalEvents {
    pub fn start() -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel(TERMINAL_EVENT_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let event_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("morons-terminal-events".to_owned())
            .spawn(move || read_terminal_events(sender, &event_stop))?;
        Ok(Self {
            receiver,
            stop,
            thread: Some(thread),
        })
    }

    pub async fn next(&mut self) -> Option<io::Result<TerminalInput>> {
        self.receiver.recv().await
    }
}

impl Drop for TerminalEvents {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_terminal_events(sender: mpsc::Sender<io::Result<TerminalInput>>, stop: &AtomicBool) {
    while !stop.load(Ordering::Acquire) {
        match event::poll(TERMINAL_POLL_INTERVAL) {
            Ok(false) => {}
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => {
                    let input = if is_clipboard_paste_key(key) {
                        capture_clipboard()
                    } else {
                        TerminalInput::Key(key)
                    };
                    if sender.try_send(Ok(input)).is_err() && sender.is_closed() {
                        return;
                    }
                }
                Ok(Event::Paste(paste)) => {
                    let paste = Zeroizing::new(bounded_utf8_prefix(paste, MAX_PASTE_BYTES));
                    if sender.try_send(Ok(TerminalInput::Paste(paste))).is_err()
                        && sender.is_closed()
                    {
                        return;
                    }
                }
                Ok(Event::Resize(_, _)) => {
                    if sender.try_send(Ok(TerminalInput::Resize)).is_err() && sender.is_closed() {
                        return;
                    }
                }
                Ok(Event::FocusGained | Event::FocusLost | Event::Mouse(_)) => {}
                Err(error) => {
                    let _ = sender.try_send(Err(error));
                    return;
                }
            },
            Err(error) => {
                let _ = sender.try_send(Err(error));
                return;
            }
        }
    }
}

fn is_clipboard_paste_key(key: KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press || key.code != KeyCode::Char('v') {
        return false;
    }
    #[cfg(windows)]
    return key.modifiers.contains(KeyModifiers::ALT);
    #[cfg(not(windows))]
    key.modifiers.contains(KeyModifiers::CONTROL)
}

fn capture_clipboard() -> TerminalInput {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return TerminalInput::ClipboardUnavailable;
    };
    if let Ok(image) = clipboard.get_image() {
        let (Ok(width), Ok(height)) = (u32::try_from(image.width), u32::try_from(image.height))
        else {
            return TerminalInput::ClipboardUnavailable;
        };
        return morons_image::normalize_rgba(width, height, image.bytes.into_owned())
            .map_or(TerminalInput::ClipboardUnavailable, TerminalInput::Image);
    }
    clipboard
        .get_text()
        .map(|text| {
            TerminalInput::Paste(Zeroizing::new(bounded_utf8_prefix(text, MAX_PASTE_BYTES)))
        })
        .unwrap_or(TerminalInput::ClipboardUnavailable)
}

fn bounded_utf8_prefix(mut value: String, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

#[cfg(test)]
mod tests {
    use ratatui_crossterm::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{TerminalInput, bounded_utf8_prefix, is_clipboard_paste_key};

    #[test]
    fn paste_prefix_preserves_utf8_boundaries() {
        assert_eq!(bounded_utf8_prefix("aéz".to_owned(), 3), "aé");
        assert_eq!(bounded_utf8_prefix("aéz".to_owned(), 2), "a");
    }

    #[test]
    fn terminal_input_debug_omits_key_and_paste_content() {
        let input = TerminalInput::Paste(zeroize::Zeroizing::new("sensitive paste".to_owned()));
        let debug = format!("{input:?}");
        assert!(!debug.contains("sensitive paste"));
        assert!(debug.contains("paste_bytes"));
        let image = morons_image::normalize_rgba(1, 1, vec![1, 2, 3, 4])
            .expect("fixture image should normalize");
        let debug = format!("{:?}", TerminalInput::Image(image));
        assert!(!debug.contains("AQIDBA"));
        assert!(debug.contains("width"));
    }

    #[test]
    fn platform_clipboard_shortcut_is_explicit() {
        #[cfg(windows)]
        let modifiers = KeyModifiers::ALT;
        #[cfg(not(windows))]
        let modifiers = KeyModifiers::CONTROL;
        assert!(is_clipboard_paste_key(KeyEvent::new(
            KeyCode::Char('v'),
            modifiers
        )));
        assert!(!is_clipboard_paste_key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::SHIFT
        )));
    }
}
