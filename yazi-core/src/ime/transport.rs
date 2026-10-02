use std::{ffi::OsString, fs, io::{self, BufRead, BufReader, Write}, mem, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::{ffi::OsStrExt, fs::{FileTypeExt, MetadataExt}, net::UnixStream}}, path::{Component, Path, PathBuf}, time::{Duration, Instant}};

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

const MAX_FRAME: usize = 4096;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const RPC_TIMEOUT: Duration = Duration::from_secs(4);

pub(super) enum Transport {
	Direct(PathBuf),
	Herdr { path: PathBuf, params: Value },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AckScope { Applied, Recorded, Inactive, Pending }

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
#[serde(deny_unknown_fields)]
struct OpenResponse<'a> {
	id:     &'a str,
	#[serde(default, borrow, deserialize_with = "optional")]
	result: Option<OpenResult<'a>>,
	#[serde(default, borrow, deserialize_with = "optional")]
	error:  Option<OpenError<'a>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenResult<'a> {
	#[serde(rename = "type")]
	kind:       &'a str,
	session:    &'a str,
	generation: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenError<'a> {
	code:    &'a str,
	message: &'a str,
}

#[derive(Serialize)]
struct OpenRequest<'a> {
	id:     &'static str,
	method: &'static str,
	params: &'a Value,
}

// Optional fields may be absent, but an explicit null is not a protocol value.
fn optional<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> std::result::Result<Option<T>, D::Error> {
	T::deserialize(deserializer).map(Some)
}

impl Transport {
	pub(super) fn select(
		lookup: impl Fn(&str) -> Option<OsString>,
		local_session: impl FnOnce() -> bool,
	) -> Result<Option<Self>> {
		if let Some(marker) = lookup("HERDR_IME_INTENT") {
			if marker != "1" { bail!("HERDR_IME_INVALID_ENV: unsupported intent marker"); }
			if lookup("HERDR_ENV").as_deref() != Some(std::ffi::OsStr::new("1")) {
				bail!("HERDR_IME_INVALID_ENV: HERDR_ENV must be 1");
			}
			let pane = lookup("HERDR_PANE_ID").filter(|value| !value.is_empty());
			let popup = lookup("HERDR_IME_POPUP_TERMINAL_ID").filter(|value| !value.is_empty());
			let params = match (pane, popup) {
				(Some(pane), None) => json!({ "pane_id": pane.to_str().ok_or_else(|| anyhow::anyhow!("HERDR_IME_INVALID_ENV: invalid pane identity"))? }),
				(None, Some(popup)) => json!({ "popup_terminal_id": popup.to_str().ok_or_else(|| anyhow::anyhow!("HERDR_IME_INVALID_ENV: invalid popup identity"))? }),
				_ => bail!("HERDR_IME_INVALID_ENV: exactly one pane or popup identity is required"),
			};
			let path = lookup("HERDR_SOCKET_PATH").filter(|value| !value.is_empty())
				.ok_or_else(|| anyhow::anyhow!("HERDR_IME_INVALID_ENV: HERDR_SOCKET_PATH is required"))?;
			let path = PathBuf::from(path);
			validate_socket(&path)?;
			return Ok(Some(Self::Herdr { path, params }));
		}
		if !local_session() { return Ok(None); }
		let home = lookup("HOME").filter(|value| !value.is_empty())
			.ok_or_else(|| anyhow::anyhow!("HOME is unavailable for the IME control socket"))?;
		let home = PathBuf::from(home);
		if !home.is_absolute() { bail!("HOME is not absolute for the IME control socket"); }
		Ok(Some(Self::Direct(home.join(".local/state/infra-as-code/ime-control/run/control.sock"))))
	}

	fn path(&self) -> &Path {
		match self { Self::Direct(path) | Self::Herdr { path, .. } => path }
	}
}

pub(super) struct Connection {
	stream:     BufReader<UnixStream>,
	herdr:      bool,
	session:    Option<String>,
	generation: Option<u64>,
}

impl Connection {
	pub(super) fn connect(transport: &Transport) -> Result<Self> {
		validate_socket(transport.path())?;
		let stream = connect(transport.path(), Instant::now() + CONNECT_TIMEOUT)?;
		let mut connection = Self {
			stream: BufReader::new(stream), herdr: matches!(transport, Transport::Herdr { .. }),
			session: None, generation: None,
		};
		if let Transport::Herdr { params, .. } = transport {
			connection.open(params)?;
		}
		Ok(connection)
	}

	fn open(&mut self, params: &Value) -> Result<()> {
		let mut request = serde_json::to_vec(&OpenRequest { id: "ime:open", method: "pane.input_intent.stream", params })?;
		request.push(b'\n');
		let deadline = Instant::now() + RPC_TIMEOUT;
		self.write(&request, deadline)?;
		let (frame, length) = self.read(deadline)?;
		let response: OpenResponse = serde_json::from_slice(&frame[..length])
			.map_err(|error| anyhow::anyhow!("HERDR_IME_BAD_ACK: {error}"))?;
		if response.id != "ime:open" { bail!("HERDR_IME_BAD_ACK: wrong stream open request identity"); }
		let result = match (response.result, response.error) {
			(Some(result), None) => result,
			(None, Some(error)) if !error.code.is_empty() => bail!("Herdr IME stream rejected: {}: {}", error.code, error.message),
			_ => bail!("HERDR_IME_BAD_ACK: invalid stream open result"),
		};
		if result.kind != "pane_input_intent_stream_opened" || result.session.is_empty() || result.generation == 0 {
			bail!("HERDR_IME_BAD_ACK: invalid stream open identity");
		}
		self.session = Some(result.session.to_owned());
		self.generation = Some(result.generation);
		Ok(())
	}

	pub(super) fn request(&mut self, request: &[u8]) -> Result<AckScope> {
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
			bail!("IME request rejected: {}", response.error.filter(|error| !error.is_empty())
				.ok_or_else(|| anyhow::anyhow!("IME_BAD_ACK: rejection omitted its error"))?);
		}
		if response.error.is_some() { bail!("IME_BAD_ACK: success contains an error"); }
		let session = response.session.filter(|session| !session.is_empty())
			.ok_or_else(|| anyhow::anyhow!("IME_BAD_ACK: omitted session"))?;
		if self.session.as_deref().is_some_and(|previous| previous != session) {
			bail!("IME_BAD_ACK: changed session on an existing connection");
		}
		let scope = match (self.herdr, response.scope) {
			(true, Some("recorded")) => AckScope::Recorded,
			(false, Some("applied")) => AckScope::Applied,
			(false, Some("inactive")) => AckScope::Inactive,
			(false, Some("pending")) if request == b"{\"op\":\"blur\"}\n" => AckScope::Pending,
			_ => bail!("IME_BAD_ACK: unexpected acknowledgment scope"),
		};
		if self.session.is_none() { self.session = Some(session.to_owned()); }
		self.generation = Some(response.generation);
		Ok(scope)
	}

	fn write(&mut self, request: &[u8], deadline: Instant) -> Result<()> {
		if request.len() > MAX_FRAME || request.last() != Some(&b'\n') {
			bail!("IME request exceeds the frame limit or is incomplete");
		}
		let mut remaining = request;
		while !remaining.is_empty() {
			self.stream.get_mut().set_write_timeout(Some(time_left(deadline)?))?;
			let count = self.stream.get_mut().write(remaining)?;
			if count == 0 { bail!("IME disconnected while writing request"); }
			remaining = &remaining[count..];
		}
		Ok(())
	}

	fn read(&mut self, deadline: Instant) -> Result<([u8; MAX_FRAME], usize)> {
		let mut frame = [0u8; MAX_FRAME];
		let mut length = 0;
		loop {
			self.stream.get_mut().set_read_timeout(Some(time_left(deadline)?))?;
			let available = self.stream.fill_buf()?;
			if available.is_empty() { bail!("IME disconnected before ACK"); }
			let count = available.iter().position(|&byte| byte == b'\n').map_or(available.len(), |end| end + 1);
			if length + count > MAX_FRAME { bail!("IME ACK exceeds 4096 bytes"); }
			frame[length..length + count].copy_from_slice(&available[..count]);
			length += count;
			self.stream.consume(count);
			if frame[length - 1] == b'\n' { return Ok((frame, length)); }
		}
	}

	/// Cached authorization is unusable once its lease socket has reached EOF.
	pub(super) fn alive(&self) -> Result<()> {
		if !self.stream.buffer().is_empty() { bail!("IME_BAD_ACK: unsolicited response"); }
		let deadline = Instant::now() + CONNECT_TIMEOUT;
		loop {
			time_left(deadline)?;
			let mut byte = 0u8;
			let count = unsafe {
				libc::recv(self.stream.get_ref().as_raw_fd(), (&raw mut byte).cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT)
			};
			if count == 0 { bail!("IME disconnected; cached command acknowledgment is no longer valid"); }
			if count > 0 { bail!("IME_BAD_ACK: unsolicited response"); }
			let error = io::Error::last_os_error();
			if error.kind() == io::ErrorKind::WouldBlock { return Ok(()); }
			if error.kind() != io::ErrorKind::Interrupted { return Err(error.into()); }
		}
	}

	#[cfg(test)]
	pub(super) fn from_stream(stream: UnixStream, herdr: bool) -> Self {
		Self { stream: BufReader::new(stream), herdr, session: None, generation: None }
	}
}

fn time_left(deadline: Instant) -> Result<Duration> {
	deadline.checked_duration_since(Instant::now()).filter(|remaining| !remaining.is_zero())
		.ok_or_else(|| anyhow::anyhow!("IME_ACK_TIMEOUT: complete request deadline expired"))
}

fn validate_socket(path: &Path) -> Result<()> {
	let uid = unsafe { libc::geteuid() };
	if !path.is_absolute() || path.as_os_str().as_bytes().split(|&byte| byte == b'/').any(|part| part == b"." || part == b"..") {
		bail!("HERDR_IME_INVALID_PATH: socket path must be absolute and normalized");
	}
	let mut ancestor = PathBuf::new();
	for component in path.components() {
		if !matches!(component, Component::RootDir | Component::Normal(_)) {
			bail!("HERDR_IME_INVALID_PATH: invalid socket path component");
		}
		ancestor.push(component.as_os_str());
		let info = fs::symlink_metadata(&ancestor)?;
		if info.file_type().is_symlink() { bail!("HERDR_IME_INVALID_PATH: symlink traversal is forbidden"); }
		if ancestor == path {
			if !info.file_type().is_socket() || info.uid() != uid || info.mode() & 0o077 != 0 {
				bail!("HERDR_IME_INVALID_PATH: socket must be private and owned by the current UID");
			}
		} else {
			if !info.is_dir() || (info.uid() != uid && info.uid() != 0)
				|| (info.mode() & 0o022 != 0 && !(info.uid() == 0 && info.mode() & 0o1000 != 0))
			{
				bail!("HERDR_IME_INVALID_PATH: unsafe socket ancestor");
			}
			if Some(&*ancestor) == path.parent() && (info.uid() != uid || info.mode() & 0o077 != 0) {
				bail!("HERDR_IME_INVALID_PATH: socket parent must be private and owned by the current UID");
			}
		}
	}
	Ok(())
}

fn connect(path: &Path, deadline: Instant) -> Result<UnixStream> {
	let mut address: libc::sockaddr_un = unsafe { mem::zeroed() };
	let bytes = path.as_os_str().as_bytes();
	if bytes.contains(&0) || bytes.len() >= address.sun_path.len() { bail!("IME socket path exceeds the Unix address limit"); }
	address.sun_family = libc::AF_UNIX as _;
	for (destination, &byte) in address.sun_path.iter_mut().zip(bytes) { *destination = byte as _; }
	let length = (mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
	#[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"))]
	{ address.sun_len = length as _; }
	let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
	if raw < 0 { return Err(io::Error::last_os_error().into()); }
	let socket = unsafe { OwnedFd::from_raw_fd(raw) };
	let flags = unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_GETFL) };
	if flags < 0 || unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
		|| unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
	{
		return Err(io::Error::last_os_error().into());
	}
	if unsafe { libc::connect(socket.as_raw_fd(), (&raw const address).cast(), length) } < 0 {
		let error = io::Error::last_os_error();
		if !matches!(error.raw_os_error(), Some(libc::EINPROGRESS) | Some(libc::EINTR)) { return Err(error.into()); }
		loop {
			let mut pollfd = libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
			let timeout = time_left(deadline)?.as_millis().max(1).min(i32::MAX as u128) as i32;
			let ready = unsafe { libc::poll(&raw mut pollfd, 1, timeout) };
			if ready == 0 { bail!("IME_CONNECT_TIMEOUT: Unix socket connect deadline expired"); }
			if ready < 0 {
				let error = io::Error::last_os_error();
				if error.kind() == io::ErrorKind::Interrupted { continue; }
				return Err(error.into());
			}
			let mut error: libc::c_int = 0;
			let mut size = mem::size_of_val(&error) as libc::socklen_t;
			if unsafe { libc::getsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_ERROR, (&raw mut error).cast(), &raw mut size) } < 0 {
				return Err(io::Error::last_os_error().into());
			}
			if error != 0 { return Err(io::Error::from_raw_os_error(error).into()); }
			break;
		}
	}
	if unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFL, flags) } < 0 { return Err(io::Error::last_os_error().into()); }
	Ok(UnixStream::from(socket))
}

#[cfg(test)]
mod tests {
	use std::{io::Write, os::unix::net::UnixStream, sync::mpsc, thread, time::{Duration, Instant}};

	use anyhow::Result;

	use super::Connection;

	#[test]
	fn fragmented_ack_does_not_extend_the_complete_frame_deadline() -> Result<()> {
		let (client, mut peer) = UnixStream::pair()?;
		let (started, ready) = mpsc::channel();
		let service = thread::spawn(move || {
			peer.write_all(b"{").unwrap();
			started.send(()).unwrap();
			for _ in 0..100 {
				thread::sleep(Duration::from_millis(10));
				if peer.write_all(b" ").is_err() { return; }
			}
			peer.write_all(b"}\n").ok();
		});
		ready.recv()?;
		let mut connection = Connection::from_stream(client, false);
		assert!(connection.read(Instant::now() + Duration::from_millis(80)).is_err());
		drop(connection);
		service.join().unwrap();
		Ok(())
	}
}
