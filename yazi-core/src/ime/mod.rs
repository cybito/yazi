//! Reports Yazi's text/command mode through one persistent intent connection.
//! Source ownership and readback belong to ime-control, never to Yazi or a Herdr server.

use anyhow::Result;

#[cfg(unix)]
use std::{env, io::IsTerminal};
#[cfg(unix)]
use transport::{AckScope, Connection, Transport};

#[cfg(unix)]
mod transport;

/// Background sync can update an acknowledged mode, but cannot start a focus
/// episode. A failed transport is terminal: uncertain source operations are not retried.
pub struct Ime {
	enabled:    bool,
	running:    bool,
	focused:    bool,
	suspended:  bool,
	state:      Option<bool>,
	failure:    Option<String>,
	last_error: Option<String>,
	#[cfg(unix)]
	transport:  Option<Transport>,
	#[cfg(unix)]
	connection: Option<Connection>,
}

impl Ime {
	pub fn new() -> Self {
		#[cfg(unix)]
		let selected = Transport::select(|key| env::var_os(key), Self::local_session);
		#[cfg(unix)]
		let (transport, failure) = match selected {
			Ok(transport) => (transport, None),
			Err(error) => (None, Some(error.to_string())),
		};
		#[cfg(unix)]
		let enabled = transport.is_some() || failure.is_some();
		#[cfg(not(unix))]
		let failure = std::env::var_os("HERDR_IME_INTENT").map(|_| "HERDR_IME_UNSUPPORTED_PLATFORM: intent transport requires Unix".to_owned());
		#[cfg(not(unix))]
		let enabled = failure.is_some();
		Self {
			enabled, running: true, focused: false, suspended: false, state: None, failure, last_error: None,
			#[cfg(unix)]
			transport,
			#[cfg(unix)]
			connection: None,
		}
	}

	#[cfg(unix)]
	fn local_session() -> bool {
		if !std::io::stdin().is_terminal() { return false; }
		if ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"].into_iter().any(|key| env::var_os(key).is_some()) {
			return false;
		}
		if cfg!(target_os = "linux") {
			return ["DISPLAY", "WAYLAND_DISPLAY"].into_iter().any(|key| env::var_os(key).is_some_and(|value| !value.is_empty()));
		}
		cfg!(target_os = "macos")
	}

	fn check_failure(&self) -> Result<()> {
		if let Some(error) = &self.failure { anyhow::bail!("{error}"); }
		Ok(())
	}

	#[cfg(unix)]
	fn request(&mut self, request: &'static [u8]) -> Result<AckScope> {
		self.check_failure()?;
		let result = (|| {
			if self.connection.is_none() {
				self.connection = Some(Connection::connect(self.transport.as_ref()
					.ok_or_else(|| anyhow::anyhow!("IME transport is unavailable"))?)?);
			}
			self.connection.as_mut().expect("IME connection established").request(request)
		})();
		if let Err(error) = &result { self.fail(error); }
		result
	}

	#[cfg(unix)]
	fn fail(&mut self, error: &anyhow::Error) {
		// EOF releases only this connection; no render or key replays its stale mode.
		self.connection = None;
		self.state = None;
		self.focused = false;
		self.failure = Some(error.to_string());
	}

	#[cfg(unix)]
	fn acknowledge(&mut self, editing: bool, scope: AckScope) {
		if matches!(scope, AckScope::Applied | AckScope::Recorded) {
			self.state = Some(editing);
		} else {
			self.focused = false;
			self.state = None;
		}
	}

	fn activate(&mut self, editing: bool) -> Result<()> {
		self.check_failure()?;
		#[cfg(unix)]
		{
			let scope = self.request(if editing { b"{\"op\":\"activate\",\"state\":\"text\",\"policy\":\"mode\"}\n" }
				else { b"{\"op\":\"activate\",\"state\":\"command\",\"policy\":\"mode\"}\n" })?;
			self.acknowledge(editing, scope);
		}
		Ok(())
	}

	/// Called after dispatch/render. Never acquires a lease or restores a lost episode.
	pub fn sync(&mut self, editing: bool) -> Result<()> {
		self.check_failure()?;
		if !self.enabled || !self.running || !self.focused { return Ok(()); }
		#[cfg(unix)]
		if self.state.is_some_and(|current| current != editing) {
			let scope = self.request(if editing { b"{\"op\":\"state\",\"state\":\"text\"}\n" }
				else { b"{\"op\":\"state\",\"state\":\"command\"}\n" })?;
			self.acknowledge(editing, scope);
		}
		Ok(())
	}

	/// Genuine terminal input samples the current editing classifier, not a cached mode.
	pub fn input(&mut self, editing: bool) -> Result<()> {
		self.check_failure()?;
		if !self.enabled || !self.running { return Ok(()); }
		if !self.focused || self.state.is_none() { return self.focus_in(editing); }
		#[cfg(unix)]
		if !matches!(self.transport.as_ref(), Some(Transport::Herdr { .. })) {
			// The compositor may have released a direct owner without a TUI focus event.
			// Only this genuine input may explicitly resume its freshly sampled mode.
			return self.resume_intent(editing);
		}
		#[cfg(unix)]
		if let Some(connection) = &self.connection
			&& let Err(error) = connection.alive()
		{
			self.fail(&error);
			return Err(error);
		}
		self.sync(editing)
	}

	/// Direct protection requires applied; Herdr recorded authorizes only reporting,
	/// with source gating performed by the attached client before it sends this key.
	pub fn command_ready(&self) -> bool {
		!self.enabled || (self.failure.is_none() && self.running && self.focused && self.state == Some(false))
	}

	pub fn focus_out(&mut self) -> Result<()> {
		self.focused = false;
		self.state = None;
		self.check_failure()?;
		if !self.enabled { return Ok(()); }
		#[cfg(unix)]
		if self.connection.is_some() { self.request(b"{\"op\":\"blur\"}\n")?; }
		Ok(())
	}

	pub fn focus_in(&mut self, editing: bool) -> Result<()> {
		self.check_failure()?;
		self.focused = true;
		if self.suspended && self.running { return self.resume_intent(editing); }
		if !self.enabled || !self.running { return Ok(()); }
		self.activate(editing)
	}

	pub fn stop(&mut self) -> Result<()> {
		self.check_failure()?;
		#[cfg(unix)]
		if self.enabled && self.connection.is_some() {
			let scope = self.request(b"{\"op\":\"suspend\"}\n")?;
			if !matches!(scope, AckScope::Applied | AckScope::Recorded) {
				let error = anyhow::anyhow!("IME_BAD_ACK: terminal handoff release was not acknowledged");
				self.fail(&error);
				return Err(error);
			}
		}
		self.running = false;
		self.focused = false;
		self.state = None;
		self.suspended = true;
		Ok(())
	}

	/// Terminal/UI return is not evidence of foreground input or focus.
	pub fn resume(&mut self) -> Result<()> {
		self.check_failure()?;
		self.running = true;
		Ok(())
	}

	fn resume_intent(&mut self, editing: bool) -> Result<()> {
		self.check_failure()?;
		self.running = true;
		self.focused = true;
		self.suspended = false;
		if !self.enabled { return Ok(()); }
		#[cfg(unix)]
		if self.connection.is_some() {
			let scope = self.request(if editing { b"{\"op\":\"resume\",\"state\":\"text\"}\n" }
				else { b"{\"op\":\"resume\",\"state\":\"command\"}\n" })?;
			self.acknowledge(editing, scope);
			return Ok(());
		}
		self.activate(editing)
	}

	pub fn quit(&mut self) -> Result<()> {
		self.running = false;
		self.focused = false;
		self.state = None;
		let mut result = self.check_failure();
		#[cfg(unix)]
		{
			if result.is_ok() && self.enabled && self.connection.is_some() {
				result = self.request(b"{\"op\":\"close\"}\n").and_then(|scope| {
					if matches!(scope, AckScope::Applied | AckScope::Recorded) { Ok(()) }
					else { anyhow::bail!("IME_BAD_ACK: close release was not acknowledged") }
				});
			}
			self.connection = None;
		}
		result
	}

	/// Notify once per distinct error. Inactive is normal background state, not an error.
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
		#[cfg(unix)]
		{ self.connection = None; }
	}
}

#[cfg(all(test, unix))]
mod tests;
