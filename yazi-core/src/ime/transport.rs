use std::{ffi::OsString, fs, io::{self, BufRead, BufReader, Write}, mem, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::{ffi::OsStrExt, fs::{FileTypeExt, MetadataExt}, net::UnixStream}}, path::{Component, Path, PathBuf}, time::{Duration, Instant}};

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

const MAX_FRAME: usize = 4096;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const RPC_TIMEOUT: Duration = Duration::from_secs(4);
const LABEL: &str = "imekit.intent";

pub(super) enum Transport {
	Direct(PathBuf),
	Hc { path: PathBuf, terminal: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AckScope {
	Applied,
	Recorded,
	Inactive,
	Pending,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response<'a> {
	ok:         bool,
	generation: u64,
	#[serde(default, borrow, deserialize_with = "optional")]
	session:    Option<&'a str>,
	#[serde(default, borrow, deserialize_with = "optional")]
	scope:      Option<&'a str>,
	#[serde(default, borrow, deserialize_with = "optional")]
	error:      Option<&'a str>,
}

#[derive(Deserialize)]
struct Lifecycle<'a> {
	op:    &'a str,
	#[serde(default, borrow)]
	state: Option<&'a str>,
}

/// hc transport state: the reporter's intent is a herdr-core terminal label.
struct Hc {
	terminal: String,
	pgid:     libc::pid_t,
	held:     bool,
	rpc:      u64,
}

// Optional fields may be absent, but an explicit null is not a protocol value.
fn optional<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
	deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
	T::deserialize(deserializer).map(Some)
}

impl Transport {
	pub(super) fn select(
		lookup: impl Fn(&str) -> Option<OsString>,
		local_session: impl FnOnce() -> bool,
	) -> Result<Option<Self>> {
		let terminal = lookup("HC_TERMINAL_ID").filter(|value| !value.is_empty());
		let socket = lookup("HC_TERMINAL_SOCKET").filter(|value| !value.is_empty());
		match (terminal, socket) {
			(Some(terminal), Some(socket)) => {
				let terminal = terminal
					.into_string()
					.map_err(|_| anyhow::anyhow!("HC_IME_INVALID_ENV: invalid terminal identity"))?;
				let path = PathBuf::from(socket);
				validate_socket(&path, false)?;
				return Ok(Some(Self::Hc { path, terminal }));
			}
			(None, None) => {}
			_ => bail!("HC_IME_INVALID_ENV: HC_TERMINAL_ID and HC_TERMINAL_SOCKET must both be set"),
		}
		if !local_session() {
			return Ok(None);
		}
		let home = lookup("HOME")
			.filter(|value| !value.is_empty())
			.ok_or_else(|| anyhow::anyhow!("HOME is unavailable for the IME control socket"))?;
		let home = PathBuf::from(home);
		if !home.is_absolute() {
			bail!("HOME is not absolute for the IME control socket");
		}
		Ok(Some(Self::Direct(home.join(".local/state/infra-as-code/ime-control/run/control.sock"))))
	}

	fn path(&self) -> &Path {
		match self {
			Self::Direct(path) | Self::Hc { path, .. } => path,
		}
	}
}

pub(super) struct Connection {
	stream:     BufReader<UnixStream>,
	hc:         Option<Hc>,
	session:    Option<String>,
	generation: Option<u64>,
}

impl Connection {
	pub(super) fn connect(transport: &Transport) -> Result<Self> {
		let hc = matches!(transport, Transport::Hc { .. });
		let identity = validate_socket(transport.path(), !hc)?;
		let stream = connect(transport.path(), Instant::now() + CONNECT_TIMEOUT)?;
		let hc = match transport {
			Transport::Direct(_) => None,
			Transport::Hc { path, terminal } => {
				// The connected socket must still be the one whose trust was checked.
				if validate_socket(path, false)? != identity {
					bail!("IME_INVALID_PATH: socket was replaced during connect");
				}
				Some(Hc {
					terminal: terminal.clone(),
					pgid:     unsafe { libc::getpgrp() },
					held:     false,
					rpc:      0,
				})
			}
		};
		Ok(Self { stream: BufReader::new(stream), hc, session: None, generation: None })
	}

	/// Maps a lifecycle request onto the terminal label and returns the synthetic
	/// `recorded` ACK the reporter state machine expects from a non-direct transport.
	fn label(&mut self, request: &[u8]) -> Result<AckScope> {
		let lifecycle: Lifecycle = serde_json::from_slice(request)?;
		let hc = self.hc.as_mut().expect("hc transport");
		let state = |state: Option<&str>| match state {
			Some(state @ ("command" | "text")) => Ok(state.to_owned()),
			_ => Err(anyhow::anyhow!("IME request has no valid state")),
		};
		let (value, held) = match lifecycle.op {
			"enter" => (Some("command".to_owned()), true),
			"activate" | "resume" => (Some(state(lifecycle.state)?), true),
			"state" => {
				let state = state(lifecycle.state)?;
				(hc.held.then_some(state), hc.held)
			}
			"blur" | "suspend" => (Some("idle".to_owned()), false),
			"close" => (Some(String::new()), false),
			op => bail!("IME request has unsupported op {op}"),
		};
		if let Some(state) = value {
			let value = if state.is_empty() { state } else { format!("1:{}:{state}", hc.pgid) };
			hc.rpc += 1;
			let id = hc.rpc;
			let mut frame = serde_json::to_vec(&json!({
				"jsonrpc": "2.0",
				"id": id,
				"method": "terminal.label",
				"params": { "terminal": hc.terminal, "labels": { LABEL: value } },
			}))?;
			frame.push(b'\n');
			let deadline = Instant::now() + RPC_TIMEOUT;
			self.write(&frame, deadline)?;
			let (reply, length) = self.read(deadline)?;
			let reply: Value = serde_json::from_slice(&reply[..length])
				.map_err(|error| anyhow::anyhow!("IME_BAD_ACK: {error}"))?;
			let labels = reply
				.as_object()
				.filter(|reply| !reply.contains_key("error") && reply.get("id") == Some(&json!(id)))
				.and_then(|reply| reply.get("result")?.as_object()?.get("labels")?.as_object())
				.ok_or_else(|| anyhow::anyhow!("IME_BAD_ACK: invalid terminal.label reply"))?;
			let stored = labels.get(LABEL);
			if (value.is_empty() && stored.is_some())
				|| (!value.is_empty() && stored.and_then(Value::as_str) != Some(value.as_str()))
			{
				bail!("IME_BAD_ACK: terminal.label did not store the intent");
			}
		}
		self.hc.as_mut().expect("hc transport").held = held;
		Ok(AckScope::Recorded)
	}

	pub(super) fn request(&mut self, request: &[u8]) -> Result<AckScope> {
		if self.hc.is_some() {
			return self.label(request);
		}
		let deadline = Instant::now() + RPC_TIMEOUT;
		self.write(request, deadline)?;
		let (frame, length) = self.read(deadline)?;
		let response: Response = serde_json::from_slice(&frame[..length])
			.map_err(|error| anyhow::anyhow!("IME_BAD_ACK: {error}"))?;
		if self.generation.is_some_and(|previous| response.generation < previous) {
			bail!("IME_BAD_ACK: generation regressed");
		}
		if !response.ok {
			if response.session.is_some() || response.scope.is_some() {
				bail!("IME_BAD_ACK: invalid rejection");
			}
			bail!(
				"IME request rejected: {}",
				response
					.error
					.filter(|error| !error.is_empty())
					.ok_or_else(|| anyhow::anyhow!("IME_BAD_ACK: rejection omitted its error"))?
			);
		}
		if response.error.is_some() {
			bail!("IME_BAD_ACK: success contains an error");
		}
		let session = response
			.session
			.filter(|session| !session.is_empty())
			.ok_or_else(|| anyhow::anyhow!("IME_BAD_ACK: omitted session"))?;
		if self.session.as_deref().is_some_and(|previous| previous != session) {
			bail!("IME_BAD_ACK: changed session on an existing connection");
		}
		let scope = match response.scope {
			Some("applied") => AckScope::Applied,
			Some("inactive") => AckScope::Inactive,
			Some("pending") if request == b"{\"op\":\"blur\"}\n" => AckScope::Pending,
			_ => bail!("IME_BAD_ACK: unexpected acknowledgment scope"),
		};
		if self.session.is_none() {
			self.session = Some(session.to_owned());
		}
		self.generation = Some(response.generation);
		Ok(scope)
	}

	fn write(&mut self, request: &[u8], deadline: Instant) -> Result<()> {
		if request.len() > MAX_FRAME || request.last() != Some(&b'\n') {
			bail!("IME request exceeds the frame limit or is incomplete");
		}
		let mut remaining = request;
		while !remaining.is_empty() {
			time_left(deadline)?;
			let count = match self.stream.get_mut().write(remaining) {
				Ok(count) => count,
				Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
				Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
					wait_ready(self.stream.get_ref().as_raw_fd(), libc::POLLOUT, deadline)?;
					continue;
				}
				Err(error) => return Err(error.into()),
			};
			if count == 0 {
				bail!("IME disconnected while writing request");
			}
			remaining = &remaining[count..];
		}
		Ok(())
	}

	fn read(&mut self, deadline: Instant) -> Result<([u8; MAX_FRAME], usize)> {
		let mut frame = [0u8; MAX_FRAME];
		let mut length = 0;
		loop {
			time_left(deadline)?;
			let available = match self.stream.fill_buf() {
				Ok(available) => available,
				Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
				Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
					wait_ready(self.stream.get_ref().as_raw_fd(), libc::POLLIN, deadline)?;
					continue;
				}
				Err(error) => return Err(error.into()),
			};
			if available.is_empty() {
				bail!("IME disconnected before ACK");
			}
			let count =
				available.iter().position(|&byte| byte == b'\n').map_or(available.len(), |end| end + 1);
			if length + count > MAX_FRAME {
				bail!("IME ACK exceeds 4096 bytes");
			}
			frame[length..length + count].copy_from_slice(&available[..count]);
			length += count;
			self.stream.consume(count);
			if frame[length - 1] == b'\n' {
				return Ok((frame, length));
			}
		}
	}

	/// Cached authorization is unusable once its lease socket has reached EOF.
	pub(super) fn alive(&self) -> Result<()> {
		if !self.stream.buffer().is_empty() {
			bail!("IME_BAD_ACK: unsolicited response");
		}
		let deadline = Instant::now() + CONNECT_TIMEOUT;
		loop {
			time_left(deadline)?;
			let mut byte = 0u8;
			let count = unsafe {
				libc::recv(
					self.stream.get_ref().as_raw_fd(),
					(&raw mut byte).cast(),
					1,
					libc::MSG_PEEK | libc::MSG_DONTWAIT,
				)
			};
			if count == 0 {
				bail!("IME disconnected; cached command acknowledgment is no longer valid");
			}
			if count > 0 {
				bail!("IME_BAD_ACK: unsolicited response");
			}
			let error = io::Error::last_os_error();
			if error.kind() == io::ErrorKind::WouldBlock {
				return Ok(());
			}
			if error.kind() != io::ErrorKind::Interrupted {
				return Err(error.into());
			}
		}
	}

	#[cfg(test)]
	pub(super) fn from_stream(stream: UnixStream) -> Self {
		stream.set_nonblocking(true).expect("IME test stream must support nonblocking I/O");
		Self {
			stream:     BufReader::new(stream),
			hc:         None,
			session:    None,
			generation: None,
		}
	}
}

fn time_left(deadline: Instant) -> Result<Duration> {
	deadline
		.checked_duration_since(Instant::now())
		.filter(|remaining| !remaining.is_zero())
		.ok_or_else(|| anyhow::anyhow!("IME_ACK_TIMEOUT: complete request deadline expired"))
}

fn wait_ready(fd: libc::c_int, events: libc::c_short, deadline: Instant) -> Result<()> {
	loop {
		let timeout = time_left(deadline)?.as_millis().max(1).min(i32::MAX as u128) as i32;
		let mut descriptor = libc::pollfd { fd, events, revents: 0 };
		let ready = unsafe { libc::poll(&raw mut descriptor, 1, timeout) };
		if ready == 0 {
			continue;
		}
		if ready < 0 {
			let error = io::Error::last_os_error();
			if error.kind() == io::ErrorKind::Interrupted {
				continue;
			}
			return Err(error.into());
		}
		if descriptor.revents & libc::POLLNVAL != 0 {
			return Err(io::Error::from_raw_os_error(libc::EBADF).into());
		}
		return Ok(());
	}
}

/// Returns the socket's `(st_dev, st_ino)`. `private` additionally requires
/// mode 0600-or-stricter on the socket itself; hc relies on its 0700 parent.
fn validate_socket(path: &Path, private: bool) -> Result<(u64, u64)> {
	let uid = unsafe { libc::geteuid() };
	if !path.is_absolute()
		|| path
			.as_os_str()
			.as_bytes()
			.split(|&byte| byte == b'/')
			.any(|part| part == b"." || part == b"..")
	{
		bail!("IME_INVALID_PATH: socket path must be absolute and normalized");
	}
	let mut ancestor = PathBuf::new();
	let mut identity = None;
	for component in path.components() {
		if !matches!(component, Component::RootDir | Component::Normal(_)) {
			bail!("IME_INVALID_PATH: invalid socket path component");
		}
		ancestor.push(component.as_os_str());
		let info = fs::symlink_metadata(&ancestor)?;
		if info.file_type().is_symlink() {
			bail!("IME_INVALID_PATH: symlink traversal is forbidden");
		}
		if ancestor == path {
			if !info.file_type().is_socket() || info.uid() != uid || (private && info.mode() & 0o077 != 0)
			{
				bail!("IME_INVALID_PATH: socket must be owned by the current UID");
			}
			identity = Some((info.dev() as u64, info.ino()));
		} else {
			if !info.is_dir()
				|| (info.uid() != uid && info.uid() != 0)
				|| (info.mode() & 0o022 != 0 && !(info.uid() == 0 && info.mode() & 0o1000 != 0))
			{
				bail!("IME_INVALID_PATH: unsafe socket ancestor");
			}
			if Some(&*ancestor) == path.parent() && (info.uid() != uid || info.mode() & 0o077 != 0) {
				bail!("IME_INVALID_PATH: socket parent must be private and owned by the current UID");
			}
		}
	}
	identity.ok_or_else(|| anyhow::anyhow!("IME_INVALID_PATH: socket path has no target"))
}

fn connect(path: &Path, deadline: Instant) -> Result<UnixStream> {
	let mut address: libc::sockaddr_un = unsafe { mem::zeroed() };
	let bytes = path.as_os_str().as_bytes();
	if bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
		bail!("IME socket path exceeds the Unix address limit");
	}
	address.sun_family = libc::AF_UNIX as _;
	for (destination, &byte) in address.sun_path.iter_mut().zip(bytes) {
		*destination = byte as _;
	}
	let length = (mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
	#[cfg(any(
		target_os = "macos",
		target_os = "freebsd",
		target_os = "openbsd",
		target_os = "netbsd",
		target_os = "dragonfly"
	))]
	{
		address.sun_len = length as _;
	}
	let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
	if raw < 0 {
		return Err(io::Error::last_os_error().into());
	}
	let socket = unsafe { OwnedFd::from_raw_fd(raw) };
	let flags = unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_GETFL) };
	if flags < 0
		|| unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
		|| unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
	{
		return Err(io::Error::last_os_error().into());
	}
	if unsafe { libc::connect(socket.as_raw_fd(), (&raw const address).cast(), length) } < 0 {
		let error = io::Error::last_os_error();
		if !matches!(error.raw_os_error(), Some(libc::EINPROGRESS) | Some(libc::EINTR)) {
			return Err(error.into());
		}
		loop {
			let mut pollfd =
				libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
			let timeout = time_left(deadline)?.as_millis().max(1).min(i32::MAX as u128) as i32;
			let ready = unsafe { libc::poll(&raw mut pollfd, 1, timeout) };
			if ready == 0 {
				bail!("IME_CONNECT_TIMEOUT: Unix socket connect deadline expired");
			}
			if ready < 0 {
				let error = io::Error::last_os_error();
				if error.kind() == io::ErrorKind::Interrupted {
					continue;
				}
				return Err(error.into());
			}
			let mut error: libc::c_int = 0;
			let mut size = mem::size_of_val(&error) as libc::socklen_t;
			if unsafe {
				libc::getsockopt(
					socket.as_raw_fd(),
					libc::SOL_SOCKET,
					libc::SO_ERROR,
					(&raw mut error).cast(),
					&raw mut size,
				)
			} < 0
			{
				return Err(io::Error::last_os_error().into());
			}
			if error != 0 {
				return Err(io::Error::from_raw_os_error(error).into());
			}
			break;
		}
	}
	Ok(UnixStream::from(socket))
}

#[cfg(test)]
mod tests {
	use std::{io::Write, os::unix::net::UnixStream, sync::mpsc, thread, time::{Duration, Instant}};

	use anyhow::Result;

	use super::Connection;

	#[test]
	fn complete_ack_remains_readable_after_peer_closes() -> Result<()> {
		let (client, mut peer) = UnixStream::pair()?;
		peer.write_all(
			b"{\"ok\":true,\"generation\":1,\"session\":\"final-lease\",\"scope\":\"applied\"}\n",
		)?;
		drop(peer);
		let mut connection = Connection::from_stream(client);
		let (frame, length) = connection.read(Instant::now() + Duration::from_millis(80))?;
		let response: serde_json::Value = serde_json::from_slice(&frame[..length])?;
		assert_eq!(response["session"], "final-lease");
		assert_eq!(response["scope"], "applied");
		assert!(connection.alive().is_err());
		Ok(())
	}

	#[test]
	fn fragmented_ack_does_not_extend_the_complete_frame_deadline() -> Result<()> {
		let (client, mut peer) = UnixStream::pair()?;
		let (started, ready) = mpsc::channel();
		let service = thread::spawn(move || {
			peer.write_all(b"{").unwrap();
			started.send(()).unwrap();
			for _ in 0..100 {
				thread::sleep(Duration::from_millis(10));
				if peer.write_all(b" ").is_err() {
					return;
				}
			}
			peer.write_all(b"}\n").ok();
		});
		ready.recv()?;
		let mut connection = Connection::from_stream(client);
		assert!(connection.read(Instant::now() + Duration::from_millis(80)).is_err());
		drop(connection);
		service.join().unwrap();
		Ok(())
	}
}
