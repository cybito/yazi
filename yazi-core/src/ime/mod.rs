//! Reports Yazi's text/command mode through one optional persistent intent connection.
//! Source ownership and readback belong to ime-control, never to Yazi or a Herdr server.

use anyhow::Result;

#[cfg(unix)]
use std::{env, io::IsTerminal};
#[cfg(unix)]
use transport::{AckScope, Connection, Transport};

#[cfg(unix)]
mod transport;

/// Background sync cannot start a focus episode. Any reporter failure disables
/// it until the next process start; ordinary input and terminal handoff continue.
pub struct Ime {
	enabled: bool,
	running: bool,
	focused: bool,
	suspended: bool,
	state: Option<bool>,
	#[cfg(unix)]
	transport: Option<Transport>,
	#[cfg(unix)]
	connection: Option<Connection>,
}

impl Ime {
	pub fn new() -> Self {
		#[cfg(unix)]
		let transport = Transport::select(|key| env::var_os(key), Self::local_session).ok().flatten();
		#[cfg(unix)]
		let enabled = transport.is_some();
		#[cfg(not(unix))]
		let enabled = false;
		Self {
			enabled,
			running: true,
			focused: false,
			suspended: false,
			state: None,
			#[cfg(unix)]
			transport,
			#[cfg(unix)]
			connection: None,
		}
	}

	#[cfg(unix)]
	fn local_session() -> bool {
		if !std::io::stdin().is_terminal() {
			return false;
		}
		if ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"].into_iter().any(|key| env::var_os(key).is_some())
		{
			return false;
		}
		if cfg!(target_os = "linux") {
			return ["DISPLAY", "WAYLAND_DISPLAY"]
				.into_iter()
				.any(|key| env::var_os(key).is_some_and(|value| !value.is_empty()));
		}
		cfg!(target_os = "macos")
	}

	#[cfg(unix)]
	fn request(&mut self, request: &'static [u8]) -> Option<AckScope> {
		if !self.enabled {
			return None;
		}
		let result = (|| {
			if self.connection.is_none() {
				self.connection = Some(Connection::connect(
					self.transport.as_ref().ok_or_else(|| anyhow::anyhow!("IME transport is unavailable"))?,
				)?);
			}
			self.connection.as_mut().expect("IME connection established").request(request)
		})();
		match result {
			Ok(scope) => Some(scope),
			Err(_) => {
				self.disable();
				None
			}
		}
	}

	#[cfg(unix)]
	fn disable(&mut self) {
		// EOF releases only this reporter's intent; no key replays its stale mode.
		self.connection = None;
		self.transport = None;
		self.enabled = false;
		self.state = None;
		self.focused = false;
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
		#[cfg(unix)]
		if let Some(scope) = self.request(if editing {
			b"{\"op\":\"activate\",\"state\":\"text\",\"policy\":\"mode\"}\n"
		} else {
			b"{\"op\":\"activate\",\"state\":\"command\",\"policy\":\"mode\"}\n"
		}) {
			self.acknowledge(editing, scope);
		}
		Ok(())
	}

	/// Called after dispatch/render. Never acquires a lease or restores a lost episode.
	pub fn sync(&mut self, editing: bool) -> Result<()> {
		if !self.enabled || !self.running || !self.focused {
			return Ok(());
		}
		#[cfg(unix)]
		if self.state.is_some_and(|current| current != editing)
			&& let Some(scope) = self.request(if editing {
				b"{\"op\":\"state\",\"state\":\"text\"}\n"
			} else {
				b"{\"op\":\"state\",\"state\":\"command\"}\n"
			}) {
			self.acknowledge(editing, scope);
		}
		Ok(())
	}

	/// Genuine terminal input samples the current editing classifier, not a cached mode.
	pub fn input(&mut self, editing: bool) -> Result<()> {
		if !self.enabled || !self.running {
			return Ok(());
		}
		if !self.focused || self.state.is_none() {
			return self.focus_in(editing);
		}
		#[cfg(unix)]
		if !matches!(self.transport.as_ref(), Some(Transport::Herdr { .. })) {
			// Only genuine input may resume a direct owner without a TUI focus event.
			return self.resume_intent(editing);
		}
		#[cfg(unix)]
		if let Some(connection) = &self.connection
			&& connection.alive().is_err()
		{
			self.disable();
			return Ok(());
		}
		self.sync(editing)
	}

	/// Disabled means ordinary CLI dispatch, not an applied IME guarantee.
	/// Direct protection requires applied; Herdr recorded only authorizes reporting.
	pub fn command_ready(&self) -> bool {
		!self.enabled || (self.running && self.focused && self.state == Some(false))
	}

	pub fn focus_out(&mut self) -> Result<()> {
		self.focused = false;
		self.state = None;
		#[cfg(unix)]
		if self.connection.is_some() {
			self.request(b"{\"op\":\"blur\"}\n");
		}
		Ok(())
	}

	pub fn focus_in(&mut self, editing: bool) -> Result<()> {
		if !self.enabled || !self.running {
			return Ok(());
		}
		self.focused = true;
		if self.suspended {
			return self.resume_intent(editing);
		}
		self.activate(editing)
	}

	pub fn stop(&mut self) -> Result<()> {
		#[cfg(unix)]
		if self.connection.is_some()
			&& let Some(scope) = self.request(b"{\"op\":\"suspend\"}\n")
			&& !matches!(scope, AckScope::Applied | AckScope::Recorded)
		{
			self.disable();
		}
		self.running = false;
		self.focused = false;
		self.state = None;
		self.suspended = true;
		Ok(())
	}

	/// Terminal/UI return is not evidence of foreground input or focus.
	pub fn resume(&mut self) -> Result<()> {
		self.running = true;
		Ok(())
	}

	fn resume_intent(&mut self, editing: bool) -> Result<()> {
		self.running = true;
		self.focused = true;
		self.suspended = false;
		if !self.enabled {
			return Ok(());
		}
		#[cfg(unix)]
		if self.connection.is_some() {
			if let Some(scope) = self.request(if editing {
				b"{\"op\":\"resume\",\"state\":\"text\"}\n"
			} else {
				b"{\"op\":\"resume\",\"state\":\"command\"}\n"
			}) {
				self.acknowledge(editing, scope);
			}
			return Ok(());
		}
		self.activate(editing)
	}

	pub fn quit(&mut self) -> Result<()> {
		self.running = false;
		self.focused = false;
		self.state = None;
		#[cfg(unix)]
		{
			if self.connection.is_some()
				&& let Some(scope) = self.request(b"{\"op\":\"close\"}\n")
				&& !matches!(scope, AckScope::Applied | AckScope::Recorded)
			{
				self.disable();
			}
			self.connection = None;
		}
		Ok(())
	}
}

impl Default for Ime {
	fn default() -> Self {
		Self::new()
	}
}

impl Drop for Ime {
	fn drop(&mut self) {
		#[cfg(unix)]
		{
			self.connection = None;
		}
	}
}

#[cfg(all(test, unix))]
mod tests;
