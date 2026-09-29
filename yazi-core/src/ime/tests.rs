use std::{io::{BufRead, BufReader, Write}, os::unix::net::UnixStream, sync::mpsc, thread, time::Duration};

use anyhow::Result;

use super::Ime;

fn reporter(client: UnixStream) -> Ime {
	let mut ime = Ime::new();
	ime.enabled = true;
	ime.running = true;
	ime.focused = true;
	ime.connection = Some(BufReader::new(client));
	ime
}

#[test]
fn reports_mode_focus_and_lifetime_without_a_source_snapshot() -> Result<()> {
	let (client, server) = UnixStream::pair()?;
	let mut ime = reporter(client);
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(server);
		for request in [
			"{\"op\":\"activate\",\"state\":\"command\",\"policy\":\"mode\"}\n",
			"{\"op\":\"state\",\"state\":\"text\"}\n",
			"{\"op\":\"blur\"}\n",
			"{\"op\":\"activate\",\"state\":\"text\",\"policy\":\"mode\"}\n",
			"{\"op\":\"suspend\"}\n",
			"{\"op\":\"resume\",\"state\":\"command\"}\n",
			"{\"op\":\"close\"}\n",
		] {
			let mut actual = String::new();
			peer.read_line(&mut actual).unwrap();
			assert_eq!(actual, request);
			peer.get_mut().write_all(b"{\"ok\":true,\"generation\":1,\"session\":\"test-lease\"}\n").unwrap();
		}
	});

	ime.sync(false)?;
	assert!(ime.command_ready());
	ime.sync(false)?; // A repeated mode must not make a second request.
	ime.sync(true)?;
	ime.focus_out()?;
	assert!(!ime.command_ready());
	ime.focus_in(true)?;
	ime.stop()?;
	ime.resume(false)?;
	assert!(ime.command_ready());
	ime.quit()?;
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
		done.send(ime.sync(false).is_ok() && ime.command_ready()).unwrap();
	});
	let mut peer = BufReader::new(server);
	let mut request = String::new();
	peer.read_line(&mut request)?;
	assert_eq!(request, "{\"op\":\"activate\",\"state\":\"command\",\"policy\":\"mode\"}\n");
	assert!(verdict.recv_timeout(Duration::from_millis(50)).is_err());
	peer.get_mut().write_all(b"{\"ok\":true,\"generation\":1,\"session\":\"test-lease\"}\n")?;
	assert!(verdict.recv_timeout(Duration::from_secs(1))?);
	worker.join().unwrap();
	Ok(())
}

#[test]
fn rejected_or_malformed_ack_never_marks_command_mode_protected() -> Result<()> {
	for reply in [
		b"{\"ok\":false,\"generation\":2,\"error\":\"BACKEND_UNAVAILABLE\"}\n".as_slice(),
		b"{\"ok\":true,\"generation\":3}\n".as_slice(),
	] {
		let (client, mut server) = UnixStream::pair()?;
		let mut ime = reporter(client);
		let service = thread::spawn(move || {
			let mut request = String::new();
			BufReader::new(&server).read_line(&mut request).unwrap();
			server.write_all(reply).unwrap();
		});
		assert!(ime.sync(false).is_err());
		assert!(!ime.command_ready());
		assert_eq!(ime.state, None);
		assert!(ime.connection.is_none());
		service.join().unwrap();
	}
	Ok(())
}

#[test]
fn failed_suspend_cancels_handoff_and_allows_command_reacquisition() -> Result<()> {
	let (client, server) = UnixStream::pair()?;
	let mut ime = reporter(client);
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(server);
		for (expected, ack) in [
			(
				"{\"op\":\"activate\",\"state\":\"command\",\"policy\":\"mode\"}\n",
				"{\"ok\":true,\"generation\":1,\"session\":\"old\"}\n",
			),
			(
				"{\"op\":\"suspend\"}\n",
				"{\"ok\":false,\"generation\":2,\"error\":\"RESTORE_FAILED\"}\n",
			),
		] {
			let mut request = String::new();
			peer.read_line(&mut request).unwrap();
			assert_eq!(request, expected);
			peer.get_mut().write_all(ack.as_bytes()).unwrap();
		}
	});
	ime.sync(false)?;
	assert!(ime.stop().is_err());
	assert!(ime.running, "a failed handoff must not mark the app suspended");
	assert!(!ime.command_ready(), "the rejected ACK must revoke command authorization");
	service.join().unwrap();

	let (client, server) = UnixStream::pair()?;
	ime.connection = Some(BufReader::new(client));
	let service = thread::spawn(move || {
		let mut peer = BufReader::new(server);
		let mut request = String::new();
		peer.read_line(&mut request).unwrap();
		assert_eq!(request, "{\"op\":\"activate\",\"state\":\"command\",\"policy\":\"mode\"}\n");
		peer.get_mut().write_all(b"{\"ok\":true,\"generation\":3,\"session\":\"new\"}\n").unwrap();
	});
	ime.sync(false)?;
	assert!(ime.command_ready());
	service.join().unwrap();
	Ok(())
}
