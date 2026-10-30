//! Terminal-only progress. Pipes stay readable; cursor ownership is scoped to each task.
use std::{
    io::{self, Read, Write},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(super) fn motion_allowed() -> bool {
    if std::env::var_os("HORIZON_OFFLINE_NO_ANIMATION").is_some() {
        return false;
    }
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn SystemParametersInfoW(action: u32, param: u32, value: *mut i32, flags: u32) -> i32;
        }
        let mut enabled = 1;
        // SAFETY: Windows writes one BOOL into a valid local variable.
        if unsafe { SystemParametersInfoW(0x1042, 0, &mut enabled, 0) } != 0 && enabled == 0 {
            return false;
        }
    }
    true
}

fn enabled() -> bool {
    super::terminal_output() && std::env::var_os("NO_COLOR").is_none() && super::enable_vt()
}

pub(super) fn intro(style: &super::Style) {
    if !style.color || !motion_allowed() {
        return;
    }
    super::clear();
    struct Cursor;
    impl Drop for Cursor {
        fn drop(&mut self) {
            print!("\x1b[?25h");
            let _ = io::stdout().flush();
        }
    }
    let _cursor = Cursor;
    print!("\x1b[?25l");
    let start = Instant::now();
    // A single 240 ms linear light sweep. Menu navigation never replays it.
    while start.elapsed() < Duration::from_millis(240) {
        let beam = start.elapsed().as_secs_f32() / 0.24 * 54.0 - 6.0;
        println!("\x1b[H");
        for (row, text) in super::logo_rows().iter().enumerate() {
            print!("  ");
            for (column, pixel) in text.chars().enumerate() {
                let light = (1.0 - (column as f32 - beam).abs() / 6.0).max(0.0);
                let base = super::lerp(super::VIOLET, super::DEEP, row as f32 / 7.0);
                print!(
                    "{}",
                    style.fg(super::lerp(base, super::WHITE, light), &pixel.to_string())
                );
            }
            println!();
        }
        print!(
            "\n  {}\n  {} {}\n",
            style.bold(super::WHITE, "HORIZON OFFLINE"),
            style.fg(super::WHITE, "by"),
            style.bold(super::VIOLET, "RRM Network")
        );
        let _ = io::stdout().flush();
        thread::sleep(Duration::from_millis(30));
    }
    super::clear();
    println!("\n{}", super::banner(style));
}

#[derive(Default)]
struct State {
    fraction: Option<f32>,
    note: String,
}

pub(super) struct Task {
    label: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    renderer: Option<JoinHandle<()>>,
    complete: bool,
}

fn clean(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}

impl Task {
    pub(super) fn new(label: &str) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        println!("\n  ◇ {}", clean(label, 65));
        let renderer = if enabled() {
            let (state, stop) = (Arc::clone(&state), Arc::clone(&stop));
            let animate = motion_allowed();
            Some(thread::spawn(move || {
                let start = Instant::now();
                let mut frame = 0usize;
                print!("\x1b[?25l");
                while !stop.load(Ordering::Relaxed) {
                    let state = state.lock().unwrap_or_else(|e| e.into_inner());
                    let spinner = if animate {
                        ['◜', '◝', '◞', '◟'][frame % 4]
                    } else {
                        '◇'
                    };
                    let bar: String = (0..20)
                        .map(|i| {
                            if let Some(f) = state.fraction {
                                if (i as f32) < f * 20.0 { '━' } else { '·' }
                            } else if animate && (i + 20 - frame % 20) % 20 < 4 {
                                '━'
                            } else {
                                '·'
                            }
                        })
                        .collect();
                    let progress = state
                        .fraction
                        .map(|f| format!("{:>3}%", (f * 100.0) as u32))
                        .unwrap_or_else(|| "    ".into());
                    let seconds = start.elapsed().as_secs();
                    print!(
                        "\r\x1b[2K  \x1b[38;2;196;181;253m{spinner} [{bar}]\x1b[0m {progress} {:02}:{:02} {}",
                        seconds / 60,
                        seconds % 60,
                        clean(&state.note, 25)
                    );
                    let _ = io::stdout().flush();
                    drop(state);
                    frame += 1;
                    thread::sleep(Duration::from_millis(80));
                }
                print!("\r\x1b[2K\x1b[?25h");
                let _ = io::stdout().flush();
            }))
        } else {
            None
        };
        Self {
            label: label.into(),
            state,
            stop,
            renderer,
            complete: false,
        }
    }

    pub(super) fn progress(&self, bytes: u64, total: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.fraction = (total > 0).then(|| (bytes as f32 / total as f32).clamp(0.0, 1.0));
        state.note = format!(
            "{:.1} / {:.1} MB",
            bytes as f64 / 1048576.0,
            total as f64 / 1048576.0
        );
    }

    pub(super) fn note(&self, text: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.note = clean(text, 100);
        if self.label.contains("ресурс") {
            state.fraction = None;
            return;
        }
        // Official download progress ends in `NN%`. Do not infer progress for resource preparation.
        state.fraction = text
            .trim()
            .strip_suffix('%')
            .and_then(|s| s.split_whitespace().last())
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|n| *n <= 100)
            .map(|n| n as f32 / 100.0);
    }

    fn join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(renderer) = self.renderer.take() {
            let _ = renderer.join();
        }
    }

    pub(super) fn finish(mut self) {
        self.join();
        println!("  ✓ {}", self.label);
        self.complete = true;
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.join();
        if !self.complete {
            println!("  ! {} · не завершено", self.label);
        }
    }
}

/// Drain both pipes concurrently: quiet stages still show elapsed time, and large logs cannot
/// deadlock the child. Keep a bounded diagnostic tail for a failed stage.
pub(super) fn run_command(command: &mut Command, label: &str) -> Result<ExitStatus, String> {
    let task = Arc::new(Task::new(label));
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let output = child.stdout.take().ok_or("missing stdout")?;
    let errors = child.stderr.take().ok_or("missing stderr")?;
    let read = |mut pipe: Box<dyn Read + Send>, task: Arc<Task>| {
        thread::spawn(move || {
            let mut tail = Vec::new();
            let mut line = Vec::new();
            let mut buffer = [0u8; 2048];
            while let Ok(count) = pipe.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                for byte in &buffer[..count] {
                    if *byte == b'\r' || *byte == b'\n' {
                        if !line.is_empty() {
                            task.note(&String::from_utf8_lossy(&line));
                            line.clear();
                        }
                    } else if line.len() < 4096 {
                        line.push(*byte);
                    }
                }
                tail.extend_from_slice(&buffer[..count]);
                if tail.len() > 65536 {
                    tail.drain(..tail.len() - 65536);
                }
            }
            tail
        })
    };
    let stdout = read(Box::new(output), Arc::clone(&task));
    let stderr = read(Box::new(errors), Arc::clone(&task));
    let status = child.wait().map_err(|e| e.to_string())?;
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "{label} ({status}): {} {}",
            String::from_utf8_lossy(&out).trim(),
            String::from_utf8_lossy(&err).trim()
        ));
    }
    if let Ok(task) = Arc::try_unwrap(task) {
        task.finish();
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_characters_cannot_escape_a_progress_line() {
        assert_eq!(clean("hello\r\n\x1b[2J", 5), "hello");
    }
}
