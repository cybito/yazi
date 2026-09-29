mod fcitx;
mod macos;

use std::{io::Read, process::{Command, Stdio}, sync::mpsc, thread, time::{Duration, Instant}};

use anyhow::{Result, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Source {
	Mac(String),
	Fcitx5 { method: String, active: u8 },
	Fcitx4(u8),
}

pub(super) struct Output {
	pub code: i32,
	pub text: String,
}

pub(super) trait Runner {
	fn run(&mut self, binary: &str, args: &[&str]) -> Result<Output>;
}

struct ProcessRunner;

impl ProcessRunner {
	fn run_with_timeout(binary: &str, args: &[&str], timeout: Duration) -> Result<Output> {
		let mut child = Command::new(binary).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
		let stdout = child.stdout.take().expect("piped stdout");
		let (tx, rx) = mpsc::sync_channel(1);
		thread::spawn(move || {
			let mut bytes = Vec::new();
			let result = stdout.take(4096).read_to_end(&mut bytes).map(|_| bytes);
			let _ = tx.send(result);
		});
		let deadline = Instant::now() + timeout;
		let status = loop {
			if let Some(status) = child.try_wait()? {
				break status;
			}
			if Instant::now() >= deadline {
				child.kill().ok();
				child.wait().ok();
				bail!("IME command {binary} timed out");
			}
			thread::sleep(Duration::from_millis(10));
		};
		let bytes = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
			.map_err(|_| anyhow::anyhow!("IME command {binary} output timed out"))??;
		let text = String::from_utf8(bytes)?;
		Ok(Output { code: status.code().unwrap_or(-1), text })
	}
}
impl Runner for ProcessRunner {
	fn run(&mut self, binary: &str, args: &[&str]) -> Result<Output> {
		Self::run_with_timeout(binary, args, Duration::from_secs(2))
	}
}


#[derive(Clone, Copy)]
enum Platform {
	Mac,
	Fcitx,
	Unsupported,
}

/// The snapshot belongs to the current command-mode lease, never to the application lifetime.
/// Only a successful query grants a lease; failures keep an existing lease for a later retry.
pub struct Ime {
	runner:        Box<dyn Runner>,
	platform:      Platform,
	snapshot:      Option<Source>,
	forced:        bool,
	running:       bool,
	focused:       bool,
	handoff_until: Option<Instant>,
	last_error:    Option<String>,
}

impl Ime {
	pub fn new() -> Self {
		let platform = if cfg!(target_os = "macos") {
			Platform::Mac
		} else if cfg!(target_os = "linux") {
			Platform::Fcitx
		} else {
			Platform::Unsupported
		};
		Self {
			runner: Box::new(ProcessRunner), platform, snapshot: None, forced: false,
			running: true, focused: true, handoff_until: None, last_error: None,
		}
	}
	pub fn disabled() -> Self {
		let mut ime = Self::new();
		ime.running = false;
		ime
	}


	fn query(&mut self) -> Result<Source> {
		match self.platform {
			Platform::Mac => macos::query(&mut *self.runner),
			Platform::Fcitx => fcitx::query(&mut *self.runner),
			Platform::Unsupported => bail!("IME switching is unsupported on this platform"),
		}
	}

	fn is_english(&self, source: &Source) -> bool {
		match self.platform {
			Platform::Mac => macos::is_english(source),
			Platform::Fcitx => fcitx::is_english(source),
			Platform::Unsupported => false,
		}
	}

	fn english(&mut self, current: &Source) -> Result<()> {
		match self.platform {
			Platform::Mac => {
				self.handoff_until = Some(Instant::now() + Duration::from_millis(500));
				macos::english(&mut *self.runner, current)
			}
			Platform::Fcitx => fcitx::english(&mut *self.runner, current),
			Platform::Unsupported => bail!("IME switching is unsupported on this platform"),
		}
	}

	fn restore(&mut self, saved: &Source) -> Result<()> {
		match self.platform {
			Platform::Mac => {
				self.handoff_until = Some(Instant::now() + Duration::from_millis(500));
				macos::restore(&mut *self.runner, saved)
			}
			Platform::Fcitx => fcitx::restore(&mut *self.runner, saved),
			Platform::Unsupported => bail!("IME switching is unsupported on this platform"),
		}
	}

	fn acquire(&mut self) -> Result<()> {
		let current = self.query()?;
		if self.snapshot.is_none() || (!self.is_english(&current) && self.snapshot.as_ref() != Some(&current)) {
			self.snapshot = Some(current.clone());
		}
		if !self.is_english(&current) {
			self.english(&current)?;
			let verified = self.query()?;
			if !self.is_english(&verified) {
				bail!("IME did not switch to English");
			}
		}
		self.forced = true;
		Ok(())
	}

	fn release(&mut self) -> Result<()> {
		let Some(saved) = self.snapshot.clone() else { return Ok(()) };
		let current = self.query()?;
		if !self.is_english(&current) && current != saved {
			// A user's manual selection supersedes our old snapshot.
			self.snapshot = None;
			self.forced = false;
			return Ok(());
		}
		if current != saved {
			self.restore(&saved)?;
			if self.query()? != saved {
				bail!("IME did not restore the saved source");
			}
		}
		self.snapshot = None;
		self.forced = false;
		Ok(())
	}

	pub fn sync(&mut self, editing: bool) -> Result<()> {
		if !self.running { return self.release(); }
		if !self.focused {
			return if self.handoff_until.is_some_and(|until| Instant::now() < until) { Ok(()) } else { self.release() };
		}
		if editing { self.release() } else if self.forced { Ok(()) } else { self.acquire() }
	}

	pub fn poll(&mut self, editing: bool) -> Result<()> {
		if let Some(until) = self.handoff_until
			&& !self.focused && Instant::now() >= until
		{
			self.handoff_until = None;
			return self.release();
		}
		if !self.running || !self.focused || editing {
			return Ok(());
		}
		if !self.forced {
			return self.acquire();
		}
		let current = self.query()?;
		if !self.is_english(&current) {
			self.snapshot = Some(current.clone());
			self.english(&current)?;
			let verified = self.query()?;
			if !self.is_english(&verified) {
				bail!("IME did not switch back to English");
			}
		}
		Ok(())
	}

	pub fn focus_out(&mut self) -> Result<()> {
		self.focused = false;
		if self.handoff_until.is_some_and(|until| Instant::now() < until) {
			Ok(())
		} else {
			self.release()
		}
	}

	pub fn focus_in(&mut self, editing: bool) -> Result<()> {
		self.focused = true;
		self.handoff_until = None;
		self.sync(editing)
	}

	pub fn stop(&mut self) -> Result<()> {
		self.running = false;
		self.handoff_until = None;
		self.release()
	}

	pub fn resume(&mut self, editing: bool) -> Result<()> {
		self.running = true;
		self.focused = true;
		self.sync(editing)
	}

	pub fn quit(&mut self) -> Result<()> { self.stop() }

	/// Return a notification only when the error changes; a successful operation resets it.
	pub fn report(&mut self, result: Result<()>) -> Option<String> {
		match result {
			Ok(()) => { self.last_error = None; None }
			Err(error) => {
				let message = error.to_string();
				if self.last_error.as_deref() == Some(&message) { None }
				else { self.last_error = Some(message.clone()); Some(message) }
			}
		}
	}
}

impl Default for Ime {
	fn default() -> Self { Self::new() }
}

impl Drop for Ime {
	fn drop(&mut self) { let _ = self.release(); }
}

#[cfg(test)]
mod tests;
