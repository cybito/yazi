//! Reports Yazi's text/command mode to the local ime-control service.
//! Input-source ownership, readback, and supervision belong exclusively to the service.

#[cfg(unix)]
use std::env;

use anyhow::Result;
#[cfg(unix)]
use anyhow::bail;

#[cfg(unix)]
use std::{io::{BufRead, BufReader, IsTerminal, Write}, os::unix::net::UnixStream, path::PathBuf, time::Duration};
#[cfg(unix)]
use serde::Deserialize;

#[cfg(unix)]
const MAX_FRAME: usize = 4096;
#[cfg(unix)]
const TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(unix)]
#[derive(Deserialize)]
struct Response<'a> {
	ok:         bool,
	generation: u64,
	session:    Option<&'a str>,
	error:      Option<&'a str>,
}

/// A persistent connection is one service lease. `state` is only the last ACKed
/// mode; an error never authorizes a command key or suppresses a later retry.
pub struct Ime {
	enabled:    bool,
	running:    bool,
	focused:    bool,
	state:      Option<bool>,
	last_error: Option<String>,
	#[cfg(unix)]
	connection: Option<BufReader<UnixStream>>,
	#[cfg(unix)]
	session:    Option<String>,
}

impl Ime {
	pub fn new() -> Self {
		Self {
			enabled: Self::local_session(), running: true, focused: true, state: None, last_error: None,
			#[cfg(unix)]
			connection: None,
			#[cfg(unix)]
			session: None,
		}
	}

	fn local_session() -> bool {
		#[cfg(unix)]
		{
			// Only an interactive local terminal can acquire a GUI input-source lease.
			if !std::io::stdin().is_terminal() { return false; }
			// A remote SSH pane must never touch its local host's input source.
			if ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"].into_iter().any(|key| env::var_os(key).is_some()) {
				return false;
			}
			if cfg!(target_os = "linux") {
				return ["DISPLAY", "WAYLAND_DISPLAY"].into_iter().any(|key| env::var_os(key).is_some_and(|v| !v.is_empty()));
			}
			cfg!(target_os = "macos")
		}
		#[cfg(not(unix))]
		{ false }
	}

	#[cfg(unix)]
	fn socket_path() -> Result<PathBuf> {
		let home = env::var_os("HOME").filter(|home| !home.is_empty())
			.ok_or_else(|| anyhow::anyhow!("HOME is unavailable for the IME control socket"))?;
		let home = PathBuf::from(home);
		if !home.is_absolute() { bail!("HOME is not absolute for the IME control socket"); }
		Ok(home.join(".local/state/infra-as-code/ime-control/run/control.sock"))
	}

	#[cfg(unix)]
	fn request(&mut self, request: &'static [u8]) -> Result<()> {
		let result = (|| {
			if self.connection.is_none() {
				let stream = UnixStream::connect(Self::socket_path()?)
					.map_err(|error| anyhow::anyhow!("IME control socket unavailable: {error}"))?;
				stream.set_read_timeout(Some(TIMEOUT))?;
				stream.set_write_timeout(Some(TIMEOUT))?;
				self.connection = Some(BufReader::new(stream));
				self.session = None;
			}
			let connection = self.connection.as_mut().expect("IME connection established");
			connection.get_mut().write_all(request)?;

			// Preserve framing across reads; reject oversized, incomplete and malformed ACKs.
			let mut frame = [0u8; MAX_FRAME];
			let mut length = 0;
			loop {
				let available = connection.fill_buf()?;
				if available.is_empty() { bail!("IME control disconnected before ACK"); }
				let count = available.iter().position(|&byte| byte == b'\n').map_or(available.len(), |end| end + 1);
				if length + count > MAX_FRAME { bail!("IME control ACK exceeds 4096 bytes"); }
				frame[length..length + count].copy_from_slice(&available[..count]);
				length += count;
				connection.consume(count);
				if frame[length - 1] == b'\n' { break; }
			}
			let response: Response = serde_json::from_slice(&frame[..length])?;
			let _generation = response.generation;
			if !response.ok {
				bail!("IME control rejected request: {}", response.error.unwrap_or("unknown error"));
			}
			let session = response.session.filter(|session| !session.is_empty())
				.ok_or_else(|| anyhow::anyhow!("IME control ACK omitted its session"))?;
			if let Some(previous) = &self.session {
				if previous != session { bail!("IME control changed session on an existing connection"); }
			} else {
				self.session = Some(session.to_owned());
			}
			Ok(())
		})();
		if result.is_err() {
			// Releasing the socket lets the daemon release an uncertain lease.
			self.connection = None;
			self.session = None;
			self.state = None;
		}
		result
	}

	#[cfg(not(unix))]
	fn request(&mut self, _: &'static [u8]) -> Result<()> { unreachable!("non-Unix IME reporter is disabled") }

	fn activate(&mut self, editing: bool) -> Result<()> {
		self.request(if editing { b"{\"op\":\"activate\",\"state\":\"text\",\"policy\":\"mode\"}\n" }
			else { b"{\"op\":\"activate\",\"state\":\"command\",\"policy\":\"mode\"}\n" })?;
		self.state = Some(editing);
		Ok(())
	}

	pub fn sync(&mut self, editing: bool) -> Result<()> {
		if !self.enabled || !self.running || !self.focused { return Ok(()); }
		match self.state {
			None => self.activate(editing),
			Some(current) if current != editing => {
				self.request(if editing { b"{\"op\":\"state\",\"state\":\"text\"}\n" }
					else { b"{\"op\":\"state\",\"state\":\"command\"}\n" })?;
				self.state = Some(editing);
				Ok(())
			}
			_ => Ok(()),
		}
	}

	/// A blur or suspend has no command ACK, even if a prior mode was command.
	pub fn command_ready(&self) -> bool {
		!self.enabled || (self.running && self.focused && self.state == Some(false))
	}

	pub fn focus_out(&mut self) -> Result<()> {
		self.focused = false;
		self.state = None;
		if !self.enabled { return Ok(()); }
		#[cfg(unix)]
		if self.connection.is_none() { return Ok(()); }
		self.request(b"{\"op\":\"blur\"}\n")
	}

	pub fn focus_in(&mut self, editing: bool) -> Result<()> {
		self.focused = true;
		self.sync(editing)
	}

	pub fn stop(&mut self) -> Result<()> {
		if !self.enabled {
			self.running = false;
			self.state = None;
			return Ok(());
		}
		#[cfg(unix)]
		if self.connection.is_none() {
			self.running = false;
			self.state = None;
			return Ok(());
		}
		self.request(b"{\"op\":\"suspend\"}\n")?;
		self.running = false;
		self.state = None;
		Ok(())
	}

	pub fn resume(&mut self, editing: bool) -> Result<()> {
		self.running = true;
		self.focused = true;
		if !self.enabled { return Ok(()); }
		#[cfg(unix)]
		if self.connection.is_some() {
			self.request(if editing { b"{\"op\":\"resume\",\"state\":\"text\"}\n" }
				else { b"{\"op\":\"resume\",\"state\":\"command\"}\n" })?;
			self.state = Some(editing);
			return Ok(());
		}
		self.activate(editing)
	}

	pub fn quit(&mut self) -> Result<()> {
		self.running = false;
		self.state = None;
		if !self.enabled { return Ok(()); }
		#[cfg(unix)]
		if self.connection.is_none() { return Ok(()); }
		let result = self.request(b"{\"op\":\"close\"}\n");
		#[cfg(unix)]
		{
			self.connection = None;
			self.session = None;
		}
		result
	}

	/// Notify once per distinct error. An unsuccessful operation never counts as protection.
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
	fn drop(&mut self) {
		// Closing the connection releases the lease even if Yazi exits unexpectedly.
		#[cfg(unix)]
		{ self.connection = None; }
	}
}

#[cfg(all(test, unix))]
mod tests;
