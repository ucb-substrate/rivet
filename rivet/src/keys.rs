//! Keys typed at the live display.
//!
//! [`crate::progress`] owns what the terminal shows while a flow runs; this
//! owns what is typed at it for exactly as long, so a step can be picked out of
//! the ones running and asked about. Nothing here is on by default anywhere
//! else: with no display there is no cursor to move, and so nothing to read.
//!
//! # It is the terminal's mode that is borrowed, not just a file
//!
//! A key can only be seen as it is typed if the terminal stops collecting a
//! line before handing it over, and stops echoing what it collects. Both are
//! settings on the terminal device itself, which the flow's tools share, so
//! only the two that are strictly needed are changed:
//!
//! * `ICANON`, so a keystroke arrives without waiting for a newline, and
//! * `ECHO`, so `j` does not appear in the middle of the display.
//!
//! What a full raw mode would also take away is left alone on purpose. `ISIG`
//! stays, so `^C` and `^Z` keep meaning what they always did rather than
//! arriving here as bytes to be reimplemented. The output flags stay, so a
//! newline still returns the carriage and every line the display prints does
//! not staircase down the screen.
//!
//! The settings are put back when the run ends, when the process is
//! backgrounded, and around [`crate::progress::suspend`] — and, because a
//! signal ends a run where it stands, from a handler as well.
//!
//! # Backgrounding
//!
//! A process that is not the terminal's foreground job is stopped by the kernel
//! if it reads from it (`SIGTTIN`) or changes its mode (`SIGTTOU`). A run put in
//! the background with `^Z bg` therefore has to be left alone: the reader
//! notices, drops the terminal, and waits for the run to be brought back.

use std::time::Duration;

/// How long a read waits before giving the handler a [`Event::Tick`] instead.
///
/// Also the longest [`crate::progress::suspend`] waits to take the terminal
/// back, and the longest [`Keyboard::stop`] waits for the reader to notice.
const TICK: Duration = Duration::from_millis(100);

/// A key the display does something with. Everything else is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Enter,
}

/// What the reader hands to its handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    Key(Key),
    /// Nothing was typed for [`TICK`]. Anything the display shows for a while
    /// and then takes down is timed off these.
    Tick,
}

/// Whether the handler wants to keep reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    Continue,
    Stop,
}

/// Decode whatever keys are complete at the front of `buf`, leaving a partial
/// escape sequence behind for the next read.
///
/// `j`/`k` and the arrow keys move; `enter` acts. A terminal sends an arrow as
/// `ESC [ A` or, once an application has switched the cursor keys over, as
/// `ESC O A`, so both are read.
fn decode(buf: &mut Vec<u8>) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut taken = 0;
    while taken < buf.len() {
        match buf[taken] {
            b'j' => {
                keys.push(Key::Down);
                taken += 1;
            }
            b'k' => {
                keys.push(Key::Up);
                taken += 1;
            }
            b'\r' | b'\n' => {
                keys.push(Key::Enter);
                taken += 1;
            }
            0x1b => match escape(&buf[taken..]) {
                // The rest of the sequence has not arrived yet. It is not a key
                // until it has, so it stays in the buffer.
                Escape::Partial => break,
                Escape::Known(key, len) => {
                    keys.push(key);
                    taken += len;
                }
                Escape::Other(len) => taken += len,
            },
            _ => taken += 1,
        }
    }
    buf.drain(..taken);
    keys
}

/// What was found at the start of a byte string beginning with `ESC`.
enum Escape {
    Known(Key, usize),
    /// A sequence this does not read, and how long it was.
    Other(usize),
    /// The start of a sequence whose end has not been read yet.
    Partial,
}

fn escape(bytes: &[u8]) -> Escape {
    match bytes.get(1) {
        None => Escape::Partial,
        // A CSI sequence runs until its final byte, which is the part that
        // names it; the parameters in between are skipped whatever they are.
        Some(b'[') => {
            for (offset, &byte) in bytes.iter().enumerate().skip(2) {
                if (0x40..=0x7e).contains(&byte) {
                    // The final byte is what names the sequence, so an arrow
                    // held down with something else is still that arrow.
                    return match byte {
                        b'A' => Escape::Known(Key::Up, offset + 1),
                        b'B' => Escape::Known(Key::Down, offset + 1),
                        _ => Escape::Other(offset + 1),
                    };
                }
            }
            Escape::Partial
        }
        Some(b'O') => match bytes.get(2) {
            None => Escape::Partial,
            Some(b'A') => Escape::Known(Key::Up, 3),
            Some(b'B') => Escape::Known(Key::Down, 3),
            Some(_) => Escape::Other(3),
        },
        // A bare escape, or a key chord that is not one. Both bytes go, so that
        // the second is not then read as a key on its own.
        Some(_) => Escape::Other(2),
    }
}

// ---------------------------------------------------------------------------
// Unix
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod unix {
    use std::fs::{File, OpenOptions};
    use std::io::Read;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, RawFd};
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use std::{mem, ptr};

    use super::{decode, Event, Flow, TICK};

    /// The terminal, in the mode keys are read in.
    ///
    /// Opened as `/dev/tty` rather than as stdin: the display already knows it
    /// has a terminal, and this way a run whose stdin is a file still has a
    /// cursor.
    struct Tty {
        file: File,
        /// How the terminal was set up before any of this, and how it is put
        /// back afterwards.
        original: libc::termios,
    }

    impl Tty {
        fn open() -> Option<Self> {
            let file = OpenOptions::new().read(true).open("/dev/tty").ok()?;
            if unsafe { libc::isatty(file.as_raw_fd()) } != 1 {
                return None;
            }

            let mut original = MaybeUninit::<libc::termios>::uninit();
            if unsafe { libc::tcgetattr(file.as_raw_fd(), original.as_mut_ptr()) } != 0 {
                return None;
            }
            let tty = Self {
                file,
                original: unsafe { original.assume_init() },
            };

            // A background job may not touch the terminal at all, so there is
            // no point starting.
            tty.is_foreground().then_some(tty)
        }

        fn fd(&self) -> RawFd {
            self.file.as_raw_fd()
        }

        /// Whether this process is the terminal's foreground job, and so
        /// allowed to read from it and change its mode.
        fn is_foreground(&self) -> bool {
            let group = unsafe { libc::tcgetpgrp(self.fd()) };
            group != -1 && group == unsafe { libc::getpgrp() }
        }

        /// Hand keys over as they are typed, without echoing them.
        ///
        /// Only the two flags that stand in the way are cleared; see the module
        /// docs for what is deliberately left alone.
        fn take(&self) -> bool {
            let mut mode = self.original;
            mode.c_lflag &= !(libc::ICANON | libc::ECHO);
            self.set(&mode)
        }

        /// Put the terminal back the way it was found.
        fn give_back(&self) -> bool {
            self.set(&self.original)
        }

        fn set(&self, mode: &libc::termios) -> bool {
            // Changing the mode from the background raises SIGTTOU, which stops
            // the whole run.
            self.is_foreground() && unsafe { libc::tcsetattr(self.fd(), libc::TCSANOW, mode) } == 0
        }

        /// Wait up to `timeout` for something to be typed.
        fn wait(&self, timeout: Duration) -> bool {
            let mut poll = libc::pollfd {
                fd: self.fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, timeout.as_millis() as libc::c_int) };
            ready > 0 && poll.revents & libc::POLLIN != 0
        }

        fn read(&self, buf: &mut [u8]) -> usize {
            (&self.file).read(buf).unwrap_or(0)
        }
    }

    /// The terminal to put back, and how, if the run is killed.
    ///
    /// Statics because a signal handler may only reach what is already there:
    /// no locking, no allocation, nothing that could be halfway through
    /// something on the thread the signal interrupted.
    static DYING_FD: AtomicI32 = AtomicI32::new(-1);
    static DYING_MODE: AtomicPtr<libc::termios> = AtomicPtr::new(ptr::null_mut());

    /// Signals that end a run, and so leave nobody to put the terminal back.
    const FATAL: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];

    /// Put the terminal back, then die of the signal as usual.
    ///
    /// `tcsetattr` and `raise` are both on the short list of calls a signal
    /// handler is allowed to make. The handler is installed with
    /// `SA_RESETHAND`, so by the time it re-raises, the signal means what it
    /// always did: end the run.
    extern "C" fn die(signal: libc::c_int) {
        let fd = DYING_FD.load(Ordering::Relaxed);
        let mode = DYING_MODE.load(Ordering::Relaxed);
        if fd >= 0 && !mode.is_null() {
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, mode) };
        }
        unsafe { libc::raise(signal) };
    }

    /// Arrange for the terminal to be put back if the run is killed.
    ///
    /// Only signals nothing else has taken an interest in are taken over: an
    /// application that handles its own `^C` is doing something with it, and
    /// this must not be what stops that from happening.
    fn restore_when_killed(fd: libc::c_int, original: &libc::termios) {
        DYING_MODE.store(Box::into_raw(Box::new(*original)), Ordering::Relaxed);
        DYING_FD.store(fd, Ordering::Relaxed);

        for signal in FATAL {
            let mut current: libc::sigaction = unsafe { mem::zeroed() };
            let known = unsafe { libc::sigaction(signal, ptr::null(), &mut current) } == 0;
            if !known || current.sa_sigaction != libc::SIG_DFL {
                continue;
            }
            let mut action: libc::sigaction = unsafe { mem::zeroed() };
            action.sa_sigaction = die as *const () as usize;
            action.sa_flags = libc::SA_RESETHAND;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            unsafe { libc::sigaction(signal, &action, ptr::null_mut()) };
        }
    }

    /// Stop putting the terminal back: the run is doing it itself.
    fn restore_when_killed_done() {
        // The handlers go first, so that what they read cannot be freed while
        // one of them is still able to run.
        for signal in FATAL {
            let mut current: libc::sigaction = unsafe { mem::zeroed() };
            let ours = unsafe { libc::sigaction(signal, ptr::null(), &mut current) } == 0
                && current.sa_sigaction == die as *const () as usize;
            if ours {
                let mut action: libc::sigaction = unsafe { mem::zeroed() };
                action.sa_sigaction = libc::SIG_DFL;
                unsafe { libc::sigemptyset(&mut action.sa_mask) };
                unsafe { libc::sigaction(signal, &action, ptr::null_mut()) };
            }
        }
        DYING_FD.store(-1, Ordering::Relaxed);
        let mode = DYING_MODE.swap(ptr::null_mut(), Ordering::Relaxed);
        if !mode.is_null() {
            drop(unsafe { Box::from_raw(mode) });
        }
    }

    /// Reads keys from the terminal on a thread of its own until it is stopped.
    pub(crate) struct Keyboard {
        tty: Arc<Tty>,
        /// Whose turn it is with the terminal. The reader holds it only while
        /// it is actually waiting on a key, so [`Keyboard::cooked`] can take it
        /// away for as long as it needs.
        turn: Arc<Mutex<()>>,
        stop: Arc<AtomicBool>,
        reader: JoinHandle<()>,
    }

    impl Keyboard {
        /// Start reading, calling `handler` with each key and with a tick
        /// whenever nothing is typed for a moment.
        ///
        /// Returns `None` when there is no terminal to read — no controlling
        /// terminal at all, or a run that has been put in the background — in
        /// which case the display simply has no cursor.
        pub(crate) fn start(
            handler: impl FnMut(Event) -> Flow + Send + 'static,
        ) -> Option<Keyboard> {
            let tty = Arc::new(Tty::open()?);
            restore_when_killed(tty.fd(), &tty.original);
            let turn = Arc::new(Mutex::new(()));
            let stop = Arc::new(AtomicBool::new(false));

            let reader = {
                let (tty, turn, stop) = (Arc::clone(&tty), Arc::clone(&turn), Arc::clone(&stop));
                thread::Builder::new()
                    .name("rivet-keys".into())
                    .spawn(move || watch(&tty, &turn, &stop, handler))
                    .ok()?
            };

            Some(Keyboard {
                tty,
                turn,
                stop,
                reader,
            })
        }

        /// Stop reading and give the terminal back.
        ///
        /// Waits for the reader to notice, which takes at most a [`TICK`], so
        /// that the terminal is its own again before anything else prints.
        pub(crate) fn stop(self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = self.reader.join();
            restore_when_killed_done();
        }

        /// Run `f` with the terminal back in the mode it was found in.
        ///
        /// For something that has to have the terminal for itself: with the
        /// keys still being read as typed, anything that prompts would neither
        /// echo what was typed nor wait for the end of a line.
        pub(crate) fn cooked<R>(&self, f: impl FnOnce() -> R) -> R {
            let _turn = lock(&self.turn);
            let taken = self.tty.give_back();
            let result = f();
            if taken {
                self.tty.take();
            }
            result
        }
    }

    fn lock(turn: &Mutex<()>) -> MutexGuard<'_, ()> {
        // A panic while the terminal was borrowed must not leave it borrowed
        // for the rest of the run.
        turn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn watch(
        tty: &Tty,
        turn: &Mutex<()>,
        stop: &AtomicBool,
        mut handler: impl FnMut(Event) -> Flow,
    ) {
        let mut pending: Vec<u8> = Vec::new();
        // Whether the terminal is currently in our mode. It is not, whenever
        // the run is in the background: the shell has taken it back.
        let mut taken = false;

        while !stop.load(Ordering::Relaxed) {
            let mut chunk = [0u8; 32];
            let read = {
                let _turn = lock(turn);
                if !tty.is_foreground() {
                    taken = false;
                    0
                } else {
                    if !taken {
                        taken = tty.take();
                    }
                    if taken && tty.wait(TICK) {
                        tty.read(&mut chunk)
                    } else {
                        0
                    }
                }
            };

            if read == 0 {
                // Nothing waiting to be read, so an escape that has not grown
                // into a sequence by now was the escape key.
                pending.clear();
                if !taken {
                    thread::sleep(TICK);
                }
                if handler(Event::Tick) == Flow::Stop {
                    break;
                }
                continue;
            }

            pending.extend_from_slice(&chunk[..read]);
            if decode(&mut pending)
                .into_iter()
                .any(|key| handler(Event::Key(key)) == Flow::Stop)
            {
                break;
            }
        }

        let _turn = lock(turn);
        if taken {
            tty.give_back();
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::Keyboard;

// ---------------------------------------------------------------------------
// Everywhere else
// ---------------------------------------------------------------------------

/// A keyboard that never starts, for platforms whose terminals rivet does not
/// know how to read. The display keeps working; it just has no cursor.
#[cfg(not(unix))]
pub(crate) struct Keyboard;

#[cfg(not(unix))]
impl Keyboard {
    pub(crate) fn start(_handler: impl FnMut(Event) -> Flow + Send + 'static) -> Option<Self> {
        None
    }

    pub(crate) fn stop(self) {}

    pub(crate) fn cooked<R>(&self, f: impl FnOnce() -> R) -> R {
        f()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Key> {
        decode(&mut bytes.to_vec())
    }

    #[test]
    fn letters_and_arrows_are_the_same_keys() {
        assert_eq!(keys(b"j"), [Key::Down]);
        assert_eq!(keys(b"k"), [Key::Up]);
        assert_eq!(keys(b"\x1b[B"), [Key::Down]);
        assert_eq!(keys(b"\x1b[A"), [Key::Up]);
        // A terminal whose cursor keys have been switched over.
        assert_eq!(keys(b"\x1bOB"), [Key::Down]);
        assert_eq!(keys(b"\x1bOA"), [Key::Up]);
        // Both of the bytes enter can arrive as.
        assert_eq!(keys(b"\r"), [Key::Enter]);
        assert_eq!(keys(b"\n"), [Key::Enter]);
    }

    #[test]
    fn a_held_key_reads_as_every_press() {
        assert_eq!(
            keys(b"jj\x1b[Bk"),
            [Key::Down, Key::Down, Key::Down, Key::Up]
        );
    }

    #[test]
    fn a_modified_arrow_is_still_an_arrow() {
        // `ESC [ 1;5 A` is ctrl-up. It is the final byte that names a sequence,
        // so holding something down while pressing an arrow still moves.
        assert_eq!(keys(b"\x1b[1;5A"), [Key::Up]);
    }

    #[test]
    fn keys_that_do_nothing_are_dropped() {
        assert_eq!(keys(b"x"), []);
        // Delete, and a mouse report: sequences that are not arrows.
        assert_eq!(keys(b"\x1b[3~"), []);
        assert_eq!(keys(b"\x1b[<0;12;30M"), []);
        // A chord: the letter after the escape must not act on its own.
        assert_eq!(keys(b"\x1bj"), []);
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_one_key() {
        // A terminal usually writes an escape sequence in one go, but nothing
        // says it has to.
        let mut buf = b"\x1b".to_vec();
        assert_eq!(decode(&mut buf), []);
        buf.extend_from_slice(b"[");
        assert_eq!(decode(&mut buf), []);
        buf.extend_from_slice(b"B");
        assert_eq!(decode(&mut buf), [Key::Down]);
        assert!(buf.is_empty());
    }

    #[test]
    fn a_partial_sequence_is_kept_and_the_keys_before_it_are_not() {
        let mut buf = b"j\x1b[".to_vec();
        assert_eq!(decode(&mut buf), [Key::Down]);
        assert_eq!(buf, b"\x1b[");
    }
}
