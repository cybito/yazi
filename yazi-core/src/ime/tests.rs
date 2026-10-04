use std::{
	ffi::OsString,
	fs,
	io::{BufRead, BufReader, Write},
	os::unix::{
		fs::{DirBuilderExt, PermissionsExt, symlink},
		net::{UnixListener, UnixStream},
	},
	path::PathBuf,
	sync::{
		atomic::{AtomicU64, Ordering},
		mpsc,
	},
	thread,
	time::Duration,
};

use anyhow::Result;
use serde_json::{Value, json};

use super::{
	Ime,
	transport::{Connection, Transport},
};

fn reporter(client: UnixStream) -> Ime {
	Ime {
		enabled: true,
		running: true,
		focused: false,
		suspended: false,
		state: None,
		transport: None,
		connection: Some(Connection::from_stream(client, false)),
	}
}

fn assert_disabled(ime: &mut Ime) -> Result<()> {
	assert!(!ime.enabled);
	assert!(ime.connection.is_none());
	assert!(ime.transport.is_none());
	assert!(ime.state.is_none(), "ordinary input is not an applied or recorded ACK");
	assert!(!ime.focused);
	assert!(ime.command_ready(), "disabled reporter must dispatch the current command key");
	ime.sync(true)?;
	ime.focus_in(true)?;
	ime.input(false)?;
	ime.input(true)?;
	ime.focus_out()?;
	ime.stop()?;
	assert!(!ime.running);
	ime.resume()?;
	assert!(ime.running);
	ime.input(false)?;
	ime.quit()?;
	assert!(ime.state.is_none());
	assert!(ime.connection.is_none());
	assert!(!ime.enabled, "no lifecycle event may reconnect or replay a failed reporter");
	Ok(())
}

#[test]
fn missing_service_disables_reporter_without_blocking_input_or_lifecycle() -> Result<()> {
	let fixture = Fixture::new()?;
	let mut ime = Ime {
		enabled: true,
		running: true,
		focused: false,
		suspended: false,
		state: None,
		transport: Some(Transport::Direct(fixture.root.join("missing"))),
		connection: None,
	};
	ime.focus_in(false)?;
	assert_disabled(&mut ime)?;
	// A subsequently available socket cannot revive the reporter in this process.
	let service = UnixListener::bind(fixture.root.join("missing"))?;
	service.set_nonblocking(true)?;
	ime.resume()?;
	ime.input(false)?;
	assert_disabled(&mut ime)?;
	assert_eq!(service.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
	Ok(())
}

fn receive(peer: &mut BufReader<UnixStream>, op: &str, state: Option<&str>) {
	let mut frame = String::new();
	peer.read_line(&mut frame).unwrap();
	let request: Value = serde_json::from_str(&frame).unwrap();
	assert_eq!(request["op"], op);
	assert_eq!(request.get("state").and_then(Value::as_str), state);
	if op == "activate" {
		assert_eq!(request["policy"], "mode");
	}
}

fn acknowledge(peer: &mut BufReader<UnixStream>, generation: u64, scope: &str) {
	writeln!(
		peer.get_mut(),
		"{}",
		json!({ "ok": true, "generation": generation, "session": "test-lease", "scope": scope })
	)
	.unwrap();
}

#[test]
fn reports_mode_focus_and_lifetime_without_background_reactivation() -> Result<()> {
	let (client, server) = UnixStream::pair()?;
	let mut ime = reporter(client);
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(server);
		for (op, state, scope) in [
			("activate", Some("command"), "applied"),
			("resume", Some("command"), "applied"),
			("state", Some("text"), "applied"),
			("blur", None, "pending"),
			("activate", Some("text"), "applied"),
			("suspend", None, "applied"),
			("resume", Some("command"), "applied"),
			("close", None, "applied"),
		] {
			receive(&mut peer, op, state);
			acknowledge(&mut peer, 1, scope);
		}
	});

	ime.sync(false)?;
	assert!(!ime.command_ready());
	ime.focus_in(false)?;
	assert!(ime.command_ready());
	ime.sync(false)?;
	ime.input(false)?;
	assert!(ime.command_ready());
	ime.sync(true)?;
	assert!(!ime.command_ready());
	ime.focus_out()?;
	assert!(!ime.command_ready());
	ime.sync(false)?;
	ime.focus_in(true)?;
	ime.stop()?;
	ime.sync(false)?;
	ime.input(false)?;
	assert!(!ime.command_ready());
	ime.resume()?;
	assert!(!ime.command_ready());
	ime.sync(true)?;
	ime.input(false)?;
	assert!(ime.command_ready());
	ime.quit()?;
	assert!(!ime.command_ready());
	service.join().unwrap();
	Ok(())
}

#[test]
fn command_key_cannot_be_authorized_before_service_ack() -> Result<()> {
	let (client, server) = UnixStream::pair()?;
	let (done, verdict) = mpsc::channel();
	let worker = thread::spawn(move || {
		let mut ime = reporter(client);
		assert!(!ime.command_ready());
		done.send(ime.input(false).is_ok() && ime.command_ready()).unwrap();
	});
	let mut peer = BufReader::new(server);
	receive(&mut peer, "activate", Some("command"));
	assert!(verdict.recv_timeout(Duration::from_millis(50)).is_err());
	acknowledge(&mut peer, 1, "applied");
	assert!(verdict.recv_timeout(Duration::from_secs(1))?);
	worker.join().unwrap();
	Ok(())
}

#[test]
fn inactive_cancels_command_until_real_input_samples_current_editing() -> Result<()> {
	let (client, server) = UnixStream::pair()?;
	let mut ime = reporter(client);
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(server);
		for (op, state, scope) in [
			("activate", Some("command"), "inactive"),
			("activate", Some("text"), "applied"),
			("state", Some("command"), "inactive"),
			("activate", Some("command"), "applied"),
			("close", None, "applied"),
		] {
			receive(&mut peer, op, state);
			acknowledge(&mut peer, 1, scope);
		}
	});
	ime.focus_in(false)?;
	assert!(!ime.command_ready());
	ime.sync(false)?;
	ime.sync(true)?;
	ime.input(true)?;
	assert!(!ime.command_ready());
	ime.sync(false)?;
	assert!(!ime.command_ready());
	ime.sync(true)?;
	ime.input(false)?;
	assert!(ime.command_ready());
	ime.quit()?;
	service.join().unwrap();
	Ok(())
}

#[test]
fn rejected_malformed_or_incomplete_ack_revokes_protection_without_retry() -> Result<()> {
	let mut oversized = vec![b'x'; 4096];
	oversized.push(b'\n');
	for reply in [
		b"{\"ok\":false,\"generation\":2,\"error\":\"BACKEND_UNAVAILABLE\"}\n".as_slice(),
		b"{\"ok\":false,\"generation\":2,\"error\":\"FOCUS_UNVERIFIED\"}\n".as_slice(),
		b"{\"ok\":true,\"generation\":3}\n".as_slice(),
		b"{\"ok\":true,\"generation\":1,\"session\":\"test-lease\"}\n".as_slice(),
		b"{\"ok\":true,\"generation\":1,\"session\":\"test-lease\",\"scope\":\"recorded\"}\n".as_slice(),
		b"{\"ok\":true,\"generation\":1,\"session\":\"test-lease\",\"scope\":\"pending\"}\n".as_slice(),
		b"{\"ok\":true,\"generation\":true,\"session\":\"test-lease\",\"scope\":\"applied\"}\n".as_slice(),
		b"{\"ok\":true,\"generation\":1,\"session\":\"test-lease\",\"scope\":\"applied\",\"error\":\"UNKNOWN\"}\n".as_slice(),
		b"{\"ok\":true,\"generation\":1,\"session\":\"test-lease\",\"scope\":\"applied\",\"extra\":true}\n".as_slice(),
		b"{\"ok\":true,\"generation\":1,\"session\":\"test-lease\",\"scope\":\"applied\",\"error\":null}\n".as_slice(),
		b"{\"ok\":true".as_slice(),
		oversized.as_slice(),
	] {
		let (client, server) = UnixStream::pair()?;
		let mut ime = reporter(client);
		let reply = reply.to_owned();
		let service = thread::spawn(move || {
			let mut peer = BufReader::new(server);
			receive(&mut peer, "activate", Some("command"));
			peer.get_mut().write_all(&reply).unwrap();
		});
		ime.input(false)?;
		assert_disabled(&mut ime)?;
		service.join().unwrap();
	}
	Ok(())
}

#[test]
fn stable_session_and_monotonic_generation_are_required_across_modes() -> Result<()> {
	for reply in [
		json!({ "ok": true, "generation": 2, "session": "different", "scope": "applied" }),
		json!({ "ok": true, "generation": 1, "session": "test-lease", "scope": "applied" }),
		json!({ "ok": true, "generation": -1, "session": "test-lease", "scope": "applied" }),
	] {
		let (client, server) = UnixStream::pair()?;
		let mut ime = reporter(client);
		let service = thread::spawn(move || {
			let mut peer = BufReader::new(server);
			receive(&mut peer, "activate", Some("command"));
			acknowledge(&mut peer, 2, "applied");
			receive(&mut peer, "state", Some("text"));
			writeln!(peer.get_mut(), "{reply}").unwrap();
		});
		ime.focus_in(false)?;
		assert!(ime.command_ready());
		ime.sync(true)?;
		assert_disabled(&mut ime)?;
		service.join().unwrap();
	}
	Ok(())
}

#[test]
fn failed_suspend_allows_handoff_and_never_replays_uncertain_source_state() -> Result<()> {
	for reply in [
		json!({ "ok": false, "generation": 2, "error": "RESTORE_FAILED" }),
		json!({ "ok": true, "generation": 2, "session": "test-lease", "scope": "inactive" }),
	] {
		let (client, server) = UnixStream::pair()?;
		let mut ime = reporter(client);
		let service = thread::spawn(move || {
			let mut peer = BufReader::new(server);
			receive(&mut peer, "activate", Some("command"));
			acknowledge(&mut peer, 1, "applied");
			receive(&mut peer, "suspend", None);
			writeln!(peer.get_mut(), "{reply}").unwrap();
		});
		ime.focus_in(false)?;
		ime.stop()?;
		assert!(!ime.running, "optional reporter must not cancel terminal handoff");
		assert_disabled(&mut ime)?;
		service.join().unwrap();
	}
	Ok(())
}

struct Fixture {
	root: PathBuf,
	path: PathBuf,
	listener: UnixListener,
}

impl Fixture {
	fn new() -> Result<Self> {
		static NEXT: AtomicU64 = AtomicU64::new(0);
		let root = std::env::temp_dir().canonicalize()?.join(format!(
			"yi-{}-{}",
			std::process::id(),
			NEXT.fetch_add(1, Ordering::Relaxed)
		));
		fs::DirBuilder::new().mode(0o700).create(&root)?;
		let path = root.join("s");
		let listener = UnixListener::bind(&path)?;
		fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
		Ok(Self { root, path, listener })
	}

	fn select(&self, identity: (&str, &str)) -> Result<Transport> {
		let values = [
			("HERDR_IME_INTENT", OsString::from("1")),
			("HERDR_ENV", OsString::from("1")),
			("HERDR_SOCKET_PATH", self.path.clone().into_os_string()),
			(identity.0, OsString::from(identity.1)),
		];
		Ok(
			Transport::select(
				|key| values.iter().find(|(name, _)| *name == key).map(|(_, value)| value.clone()),
				|| panic!("Herdr must be selected before ordinary SSH/GUI eligibility"),
			)?
			.unwrap(),
		)
	}

	fn reporter(&self, identity: (&str, &str)) -> Result<Ime> {
		Ok(Ime {
			enabled: true,
			running: true,
			focused: false,
			suspended: false,
			state: None,
			transport: Some(self.select(identity)?),
			connection: None,
		})
	}
}

impl Drop for Fixture {
	fn drop(&mut self) {
		fs::remove_dir_all(&self.root).unwrap();
	}
}

fn open(peer: &mut BufReader<UnixStream>, params: Value) {
	let mut frame = String::new();
	peer.read_line(&mut frame).unwrap();
	let request: Value = serde_json::from_str(&frame).unwrap();
	assert_eq!(request["id"], "ime:open");
	assert_eq!(request["method"], "pane.input_intent.stream");
	assert_eq!(request["params"], params);
}

fn opened(peer: &mut BufReader<UnixStream>) {
	writeln!(peer.get_mut(), "{}", json!({ "id": "ime:open", "result": { "type": "pane_input_intent_stream_opened", "session": "test-lease", "generation": 1 } })).unwrap();
}

#[test]
fn herdr_pane_and_popup_streams_use_recorded_only_and_preserve_parent_handoff() -> Result<()> {
	for (identity, params) in [
		(("HERDR_PANE_ID", "pane-7"), json!({ "pane_id": "pane-7" })),
		(("HERDR_IME_POPUP_TERMINAL_ID", "42"), json!({ "popup_terminal_id": "42" })),
	] {
		let fixture = Fixture::new()?;
		let mut ime = fixture.reporter(identity)?;
		let listener = fixture.listener.try_clone()?;
		let service = thread::spawn(move || {
			let mut peer = BufReader::new(listener.accept().unwrap().0);
			open(&mut peer, params);
			opened(&mut peer);
			for (generation, (op, state)) in [
				("activate", Some("command")),
				("state", Some("text")),
				("suspend", None),
				("resume", Some("command")),
				("blur", None),
				("activate", Some("text")),
				("close", None),
			]
			.into_iter()
			.enumerate()
			{
				receive(&mut peer, op, state);
				acknowledge(&mut peer, generation as u64 + 1, "recorded");
			}
		});
		ime.focus_in(false)?;
		assert!(ime.command_ready());
		ime.sync(true)?;
		ime.stop()?;
		ime.sync(false)?;
		assert!(!ime.command_ready());
		ime.resume()?;
		ime.sync(true)?;
		assert!(!ime.command_ready());
		ime.input(false)?;
		assert!(ime.command_ready());
		ime.focus_out()?;
		ime.sync(false)?;
		assert!(!ime.command_ready());
		ime.input(true)?;
		ime.quit()?;
		service.join().unwrap();
	}
	Ok(())
}

#[test]
fn child_return_keeps_parent_suspended_until_genuine_input_or_focus() -> Result<()> {
	for focus in [false, true] {
		for editing in [false, true] {
			let fixture = Fixture::new()?;
			let mut ime = fixture.reporter(("HERDR_PANE_ID", "pane-7"))?;
			let listener = fixture.listener.try_clone()?;
			let (returned, wait_return) = mpsc::channel();
			let (checked, wait_checked) = mpsc::channel();
			let service = thread::spawn(move || {
				let mut peer = BufReader::new(listener.accept().unwrap().0);
				open(&mut peer, json!({ "pane_id": "pane-7" }));
				opened(&mut peer);
				receive(&mut peer, "activate", Some("command"));
				acknowledge(&mut peer, 1, "recorded");
				receive(&mut peer, "suspend", None);
				acknowledge(&mut peer, 2, "recorded");
				let mut child = BufReader::new(listener.accept().unwrap().0);
				open(&mut child, json!({ "pane_id": "pane-7" }));
				opened(&mut child);
				receive(&mut child, "activate", Some("command"));
				acknowledge(&mut child, 1, "recorded");
				let mut eof = String::new();
				assert_eq!(child.read_line(&mut eof).unwrap(), 0);
				wait_return.recv().unwrap();
				peer.get_mut().set_nonblocking(true).unwrap();
				let mut byte = [0];
				assert_eq!(
					std::io::Read::read(peer.get_mut(), &mut byte).unwrap_err().kind(),
					std::io::ErrorKind::WouldBlock,
					"child completion must neither send a mode nor close the retained parent stream"
				);
				peer.get_mut().set_nonblocking(false).unwrap();
				checked.send(()).unwrap();
				receive(&mut peer, "resume", Some(if editing { "text" } else { "command" }));
				acknowledge(&mut peer, 3, "recorded");
				receive(&mut peer, "close", None);
				acknowledge(&mut peer, 4, "recorded");
			});
			ime.focus_in(false)?;
			ime.stop()?;
			let mut child = fixture.reporter(("HERDR_PANE_ID", "pane-7"))?;
			child.focus_in(false)?;
			drop(child);
			ime.resume()?;
			ime.sync(!editing)?;
			ime.sync(editing)?;
			assert!(!ime.command_ready());
			returned.send(())?;
			wait_checked.recv_timeout(Duration::from_secs(1))?;
			if focus {
				ime.focus_in(editing)?;
			} else {
				ime.input(editing)?;
			}
			assert_eq!(ime.command_ready(), !editing);
			ime.quit()?;
			service.join().unwrap();
		}
	}
	Ok(())
}

#[test]
fn herdr_never_accepts_applied_scope_or_wrong_open_session_generation() -> Result<()> {
	for reply in [
		json!({ "ok": true, "generation": 2, "session": "test-lease", "scope": "applied" }),
		json!({ "ok": true, "generation": 2, "session": "other", "scope": "recorded" }),
		json!({ "ok": true, "generation": 0, "session": "test-lease", "scope": "recorded" }),
	] {
		let fixture = Fixture::new()?;
		let mut ime = fixture.reporter(("HERDR_PANE_ID", "pane-7"))?;
		let listener = fixture.listener.try_clone()?;
		let service = thread::spawn(move || {
			let mut peer = BufReader::new(listener.accept().unwrap().0);
			open(&mut peer, json!({ "pane_id": "pane-7" }));
			opened(&mut peer);
			receive(&mut peer, "activate", Some("command"));
			writeln!(peer.get_mut(), "{reply}").unwrap();
		});
		ime.focus_in(false)?;
		assert_disabled(&mut ime)?;
		service.join().unwrap();
	}
	Ok(())
}

#[test]
fn malformed_open_is_refused_before_any_mode_can_be_recorded() -> Result<()> {
	for reply in [
		json!({ "id": "ime:open", "result": { "type": "pane_input_intent_stream_opened", "session": "test-lease", "generation": 1 }, "error": null }),
		json!({ "id": "other", "result": { "type": "pane_input_intent_stream_opened", "session": "test-lease", "generation": 1 } }),
		json!({ "id": "ime:open", "result": { "type": "other", "session": "test-lease", "generation": 1 } }),
		json!({ "id": "ime:open", "result": { "type": "pane_input_intent_stream_opened", "session": "", "generation": 1 } }),
		json!({ "id": "ime:open", "result": { "type": "pane_input_intent_stream_opened", "session": "test-lease", "generation": 0 } }),
		json!({ "id": "ime:open", "result": { "type": "pane_input_intent_stream_opened", "session": "test-lease", "generation": true } }),
		json!({ "id": "ime:open", "error": { "code": "TARGET_NOT_FOUND", "message": "No target" } }),
	] {
		let fixture = Fixture::new()?;
		let mut ime = fixture.reporter(("HERDR_PANE_ID", "pane-7"))?;
		let listener = fixture.listener.try_clone()?;
		let service = thread::spawn(move || {
			let mut peer = BufReader::new(listener.accept().unwrap().0);
			open(&mut peer, json!({ "pane_id": "pane-7" }));
			writeln!(peer.get_mut(), "{reply}").unwrap();
			let mut unexpected = String::new();
			assert_eq!(peer.read_line(&mut unexpected).unwrap(), 0);
		});
		ime.focus_in(false)?;
		assert_disabled(&mut ime)?;
		service.join().unwrap();
	}
	Ok(())
}

#[test]
fn invalid_marker_or_identity_never_falls_back_to_the_host_daemon() -> Result<()> {
	for values in [
		vec![("HERDR_IME_INTENT", "unsupported")],
		vec![("HERDR_IME_INTENT", "")],
		vec![("HERDR_IME_INTENT", "1")],
		vec![("HERDR_IME_INTENT", "1"), ("HERDR_ENV", "1")],
		vec![
			("HERDR_IME_INTENT", "1"),
			("HERDR_ENV", "1"),
			("HERDR_PANE_ID", "p"),
			("HERDR_IME_POPUP_TERMINAL_ID", "t"),
		],
		vec![("HERDR_IME_INTENT", "1"), ("HERDR_ENV", "1"), ("HERDR_PANE_ID", "p")],
		vec![
			("HERDR_IME_INTENT", "1"),
			("HERDR_ENV", "1"),
			("HERDR_PANE_ID", "p"),
			("HERDR_SOCKET_PATH", "tcp://host:123"),
		],
	] {
		assert!(
			Transport::select(
				|key| values.iter().find(|(name, _)| *name == key).map(|(_, value)| OsString::from(value)),
				|| panic!("invalid marker must never reach ordinary local eligibility")
			)
			.is_err()
		);
	}
	assert!(Transport::select(|_| None, || false)?.is_none());
	Ok(())
}

#[test]
fn herdr_refuses_nonprivate_socket_or_ancestor_and_symlink_traversal() -> Result<()> {
	let fixture = Fixture::new()?;
	fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o660))?;
	assert!(fixture.select(("HERDR_PANE_ID", "p")).is_err());
	fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o600))?;
	fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o755))?;
	assert!(fixture.select(("HERDR_PANE_ID", "p")).is_err());
	fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700))?;
	let alias = fixture.root.join("alias");
	symlink(&fixture.path, &alias)?;
	let values = [("HERDR_IME_INTENT", "1"), ("HERDR_ENV", "1"), ("HERDR_PANE_ID", "p")];
	assert!(
		Transport::select(
			|key| {
				if key == "HERDR_SOCKET_PATH" {
					Some(alias.clone().into_os_string())
				} else {
					values.iter().find(|(name, _)| *name == key).map(|(_, value)| OsString::from(value))
				}
			},
			|| panic!("Herdr cannot fall back after unsafe socket refusal")
		)
		.is_err()
	);
	Ok(())
}

#[test]
fn eof_after_command_ack_does_not_reconnect_or_replay_the_old_mode() -> Result<()> {
	let (client, server) = UnixStream::pair()?;
	let mut ime = reporter(client);
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(server);
		receive(&mut peer, "activate", Some("command"));
		acknowledge(&mut peer, 1, "applied");
		receive(&mut peer, "state", Some("text"));
	});
	ime.focus_in(false)?;
	assert!(ime.command_ready());
	ime.sync(true)?;
	assert_disabled(&mut ime)?;
	service.join().unwrap();
	Ok(())
}

#[test]
fn same_mode_key_cannot_use_a_cached_ack_after_socket_eof() -> Result<()> {
	let (client, server) = UnixStream::pair()?;
	let mut ime = reporter(client);
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(server);
		receive(&mut peer, "activate", Some("command"));
		acknowledge(&mut peer, 1, "applied");
	});
	ime.focus_in(false)?;
	service.join().unwrap();
	ime.input(false)?;
	assert_disabled(&mut ime)?;
	Ok(())
}

#[test]
fn real_key_checks_direct_focus_even_when_no_focus_lost_event_was_reported() -> Result<()> {
	let (client, server) = UnixStream::pair()?;
	let mut ime = reporter(client);
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(server);
		receive(&mut peer, "activate", Some("command"));
		acknowledge(&mut peer, 1, "applied");
		receive(&mut peer, "resume", Some("command"));
		acknowledge(&mut peer, 2, "inactive");
		receive(&mut peer, "activate", Some("text"));
		acknowledge(&mut peer, 3, "applied");
		receive(&mut peer, "close", None);
		acknowledge(&mut peer, 4, "applied");
	});
	ime.focus_in(false)?;
	assert!(ime.command_ready());
	ime.input(false)?;
	assert!(!ime.command_ready());
	ime.sync(false)?;
	ime.input(true)?;
	assert!(!ime.command_ready());
	ime.quit()?;
	service.join().unwrap();
	Ok(())
}
