use std::{ffi::OsString, fs, io::{BufRead, BufReader, Write}, os::unix::{fs::{DirBuilderExt, PermissionsExt, symlink}, net::{UnixListener, UnixStream}}, path::PathBuf, sync::{atomic::{AtomicU64, Ordering}, mpsc}, thread, time::Duration};

use anyhow::Result;
use serde_json::{Value, json};

use super::{Ime, transport::{AckScope, Connection, Transport}};

fn reporter(client: UnixStream) -> Ime {
	Ime {
		enabled:    true,
		running:    true,
		focused:    false,
		focus_lost: false,
		suspended:  false,
		state:      None,
		transport:  None,
		connection: Some(Connection::from_stream(client)),
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
	ime.resume(false)?;
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
		enabled:    true,
		running:    true,
		focused:    false,
		focus_lost: false,
		suspended:  false,
		state:      None,
		transport:  Some(Transport::Direct(fixture.root.join("missing"))),
		connection: None,
	};
	ime.focus_in(false)?;
	assert_disabled(&mut ime)?;
	// A subsequently available socket cannot revive the reporter in this process.
	let service = UnixListener::bind(fixture.root.join("missing"))?;
	service.set_nonblocking(true)?;
	ime.resume(false)?;
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
			("activate", Some("text"), "applied"),
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
	ime.resume(true)?;
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
	root:     PathBuf,
	path:     PathBuf,
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

	fn values(&self) -> Vec<(&'static str, OsString)> {
		vec![
			("HC_TERMINAL_ID", OsString::from("t7")),
			("HC_TERMINAL_SOCKET", self.path.clone().into_os_string()),
		]
	}

	fn select(&self) -> Result<Transport> {
		let values = self.values();
		Ok(
			Transport::select(
				|key| values.iter().find(|(name, _)| *name == key).map(|(_, value)| value.clone()),
				|| panic!("hc must be selected before ordinary SSH/GUI eligibility"),
			)?
			.unwrap(),
		)
	}

	fn reporter(&self) -> Result<Ime> {
		Ok(Ime {
			enabled:    true,
			running:    true,
			focused:    false,
			focus_lost: false,
			suspended:  false,
			state:      None,
			transport:  Some(self.select()?),
			connection: None,
		})
	}
}

impl Drop for Fixture {
	fn drop(&mut self) { fs::remove_dir_all(&self.root).unwrap(); }
}

/// Reads one terminal.label request and returns (id, label value).
fn label(peer: &mut BufReader<UnixStream>) -> (Value, String) {
	let mut frame = String::new();
	peer.read_line(&mut frame).unwrap();
	let request: Value = serde_json::from_str(&frame).unwrap();
	assert_eq!(request["jsonrpc"], "2.0");
	assert_eq!(request["method"], "terminal.label");
	assert_eq!(request["params"]["terminal"], "t7");
	let labels = request["params"]["labels"].as_object().unwrap();
	assert_eq!(labels.len(), 1);
	(request["id"].clone(), labels["imekit.intent"].as_str().unwrap().to_owned())
}

/// Replies the way herdr-core does: the terminal's full label set.
fn stored(peer: &mut BufReader<UnixStream>, id: Value, value: &str) {
	let labels = if value.is_empty() {
		json!({ "other": "x" })
	} else {
		json!({ "imekit.intent": value, "other": "x" })
	};
	writeln!(
		peer.get_mut(),
		"{}",
		json!({ "jsonrpc": "2.0", "id": id, "result": { "terminal": "t7", "labels": labels } })
	)
	.unwrap();
}

fn expect(peer: &mut BufReader<UnixStream>, value: &str) {
	let (id, written) = label(peer);
	assert_eq!(written, value);
	stored(peer, id, value);
}

fn intent(state: &str) -> String { format!("1:{}:{state}", unsafe { libc::getpgrp() }) }

#[test]
fn hc_labels_follow_lifecycle_and_preserve_parent_handoff() -> Result<()> {
	let fixture = Fixture::new()?;
	let mut ime = fixture.reporter()?;
	let listener = fixture.listener.try_clone()?;
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(listener.accept().unwrap().0);
		let mut ids = Vec::new();
		for value in [
			intent("command"), // activate
			intent("text"),    // state while held
			intent("idle"),    // suspend
			intent("text"),    // activate on return
			intent("command"), // state
			intent("idle"),    // blur
			intent("text"),    // activate
			String::new(),     // close deletes
		] {
			let (id, written) = label(&mut peer);
			assert_eq!(written, value);
			ids.push(id.clone());
			stored(&mut peer, id, &value);
		}
		let unique: std::collections::HashSet<_> = ids.iter().map(Value::to_string).collect();
		assert_eq!(unique.len(), ids.len(), "every exchange uses a fresh id");
		let mut unexpected = String::new();
		assert_eq!(peer.read_line(&mut unexpected).unwrap(), 0);
	});
	ime.focus_in(false)?;
	assert!(ime.command_ready());
	ime.sync(true)?;
	ime.stop()?;
	ime.sync(false)?;
	assert!(!ime.command_ready());
	ime.resume(true)?;
	ime.sync(true)?;
	assert!(!ime.command_ready());
	ime.input(false)?;
	assert!(ime.command_ready());
	ime.focus_out()?;
	ime.sync(false)?;
	assert!(!ime.command_ready());
	ime.input(true)?;
	assert!(!ime.focused, "input must not override explicit focus loss");
	ime.focus_in(true)?;
	ime.quit()?;
	service.join().unwrap();
	Ok(())
}

#[test]
fn hc_state_while_not_held_records_mode_without_writing() -> Result<()> {
	let fixture = Fixture::new()?;
	let transport = fixture.select()?;
	let listener = fixture.listener.try_clone()?;
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(listener.accept().unwrap().0);
		expect(&mut peer, &intent("idle"));
		expect(&mut peer, &intent("command"));
		expect(&mut peer, "");
		let mut unexpected = String::new();
		assert_eq!(peer.read_line(&mut unexpected).unwrap(), 0);
	});
	let mut connection = Connection::connect(&transport)?;
	assert_eq!(connection.request(b"{\"op\":\"state\",\"state\":\"text\"}\n")?, AckScope::Recorded);
	assert_eq!(connection.request(b"{\"op\":\"blur\"}\n")?, AckScope::Recorded);
	assert_eq!(connection.request(b"{\"op\":\"state\",\"state\":\"text\"}\n")?, AckScope::Recorded);
	assert_eq!(
		connection.request(b"{\"op\":\"resume\",\"state\":\"command\"}\n")?,
		AckScope::Recorded
	);
	assert_eq!(connection.request(b"{\"op\":\"close\"}\n")?, AckScope::Recorded);
	drop(connection);
	service.join().unwrap();
	Ok(())
}

#[test]
fn terminal_return_reports_own_current_mode_before_any_input() -> Result<()> {
	for editing in [false, true] {
		let fixture = Fixture::new()?;
		let mut ime = fixture.reporter()?;
		let listener = fixture.listener.try_clone()?;
		let service = thread::spawn(move || {
			let mut peer = BufReader::new(listener.accept().unwrap().0);
			expect(&mut peer, &intent("command"));
			expect(&mut peer, &intent("idle"));
			expect(&mut peer, &intent(if editing { "text" } else { "command" }));
			expect(&mut peer, "");
		});
		ime.focus_in(false)?;
		ime.stop()?;
		ime.sync(editing)?;
		assert!(!ime.command_ready());
		ime.resume(editing)?;
		assert_eq!(ime.command_ready(), !editing);
		ime.sync(editing)?;
		ime.quit()?;
		service.join().unwrap();
	}
	Ok(())
}

#[test]
fn inactive_terminal_return_cannot_authorize_or_replay_a_mode() -> Result<()> {
	let (client, server) = UnixStream::pair()?;
	let mut ime = reporter(client);
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(server);
		for (op, state, scope) in [
			("activate", Some("command"), "applied"),
			("suspend", None, "applied"),
			("activate", Some("command"), "inactive"),
			("close", None, "applied"),
		] {
			receive(&mut peer, op, state);
			acknowledge(&mut peer, 1, scope);
		}
	});
	ime.focus_in(false)?;
	ime.stop()?;
	ime.resume(false)?;
	assert!(ime.enabled);
	assert!(!ime.command_ready());
	ime.sync(true)?;
	ime.sync(false)?;
	assert!(!ime.command_ready());
	ime.quit()?;
	service.join().unwrap();
	Ok(())
}

#[test]
fn hc_invalid_label_reply_disables_reporter() -> Result<()> {
	let value = intent("command");
	for reply in [
		json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32602, "message": "no terminal" } }),
		json!({ "jsonrpc": "2.0", "id": 2, "result": { "labels": { "imekit.intent": value } } }),
		json!({ "jsonrpc": "2.0", "id": 1, "result": { "labels": { "imekit.intent": "1:1:text" } } }),
		json!({ "jsonrpc": "2.0", "id": 1, "result": { "labels": {} } }),
		json!({ "jsonrpc": "2.0", "id": 1, "result": { "labels": [] } }),
		json!({ "jsonrpc": "2.0", "id": 1, "result": null }),
		json!({ "jsonrpc": "2.0", "id": 1, "result": { "labels": { "imekit.intent": value } }, "error": null }),
	] {
		let fixture = Fixture::new()?;
		let mut ime = fixture.reporter()?;
		let listener = fixture.listener.try_clone()?;
		let value = value.clone();
		let service = thread::spawn(move || {
			let mut peer = BufReader::new(listener.accept().unwrap().0);
			let (id, written) = label(&mut peer);
			assert_eq!(id, json!(1));
			assert_eq!(written, value);
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
fn hc_oversized_reply_disables_reporter() -> Result<()> {
	let fixture = Fixture::new()?;
	let mut ime = fixture.reporter()?;
	let listener = fixture.listener.try_clone()?;
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(listener.accept().unwrap().0);
		label(&mut peer);
		peer.get_mut().write_all(&[b' '; 5000]).ok();
	});
	ime.focus_in(false)?;
	assert_disabled(&mut ime)?;
	service.join().unwrap();
	Ok(())
}

#[test]
fn partial_hc_env_never_falls_back_to_the_host_daemon() -> Result<()> {
	for values in [
		vec![("HC_TERMINAL_ID", "t7")],
		vec![("HC_TERMINAL_SOCKET", "/tmp/hc.sock")],
		vec![("HC_TERMINAL_ID", "t7"), ("HC_TERMINAL_SOCKET", "")],
		vec![("HC_TERMINAL_ID", ""), ("HC_TERMINAL_SOCKET", "/tmp/hc.sock")],
		vec![("HC_TERMINAL_ID", "t7"), ("HC_TERMINAL_SOCKET", "relative.sock")],
		vec![("HC_TERMINAL_ID", "t7"), ("HC_TERMINAL_SOCKET", "/tmp/../tmp/hc.sock")],
	] {
		assert!(
			Transport::select(
				|key| values.iter().find(|(name, _)| *name == key).map(|(_, value)| OsString::from(value)),
				|| panic!("invalid hc env must never reach ordinary local eligibility")
			)
			.is_err()
		);
	}
	assert!(Transport::select(|_| None, || false)?.is_none());
	Ok(())
}

#[test]
fn hc_beats_ssh_and_gui_and_accepts_herdr_core_socket_mode() -> Result<()> {
	let fixture = Fixture::new()?;
	// herdr-core leaves its socket 0755; trust comes from the 0700 parent.
	fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o755))?;
	assert!(matches!(fixture.select()?, Transport::Hc { ref terminal, .. } if terminal == "t7"));
	Ok(())
}

#[test]
fn hc_refuses_unsafe_parent_or_symlink_without_contacting_direct_daemon() -> Result<()> {
	let fixture = Fixture::new()?;
	fixture.listener.set_nonblocking(true)?;
	fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o755))?;
	assert!(fixture.select().is_err());
	fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700))?;
	let alias = fixture.root.join("alias");
	symlink(&fixture.path, &alias)?;
	let values =
		[("HC_TERMINAL_ID", OsString::from("t7")), ("HC_TERMINAL_SOCKET", alias.into_os_string())];
	assert!(
		Transport::select(
			|key| values.iter().find(|(name, _)| *name == key).map(|(_, value)| value.clone()),
			|| panic!("hc cannot fall back after unsafe socket refusal")
		)
		.is_err()
	);
	assert!(fixture.listener.accept().is_err(), "refused socket must not be contacted");
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

#[test]
fn explicit_blur_blocks_input_and_child_return_until_focus_in() -> Result<()> {
	for blur_first in [true, false] {
		let (client, server) = UnixStream::pair()?;
		let mut ime = reporter(client);
		let service = thread::spawn(move || {
			let mut peer = BufReader::new(server);
			let lifecycle = if blur_first { ["blur", "suspend"] } else { ["suspend", "blur"] };
			receive(&mut peer, "activate", Some("command"));
			acknowledge(&mut peer, 1, "applied");
			for op in lifecycle {
				receive(&mut peer, op, None);
				acknowledge(&mut peer, 2, "applied");
			}
			receive(&mut peer, "activate", Some("command"));
			acknowledge(&mut peer, 3, "applied");
			receive(&mut peer, "close", None);
			acknowledge(&mut peer, 4, "applied");
		});
		ime.focus_in(false)?;
		if blur_first {
			ime.focus_out()?;
			ime.input(false)?;
			ime.input(true)?;
			ime.stop()?;
		} else {
			ime.stop()?;
			ime.focus_out()?;
		}
		ime.resume(false)?;
		ime.input(false)?;
		ime.sync(true)?;
		assert!(!ime.focused);
		assert!(!ime.command_ready());
		ime.focus_in(false)?;
		assert!(ime.command_ready());
		ime.quit()?;
		service.join().unwrap();
	}
	Ok(())
}
