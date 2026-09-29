use std::{cell::RefCell, rc::Rc};

use anyhow::{Result, bail};

use super::*;

#[derive(Default)]
struct State {
	source: String,
	sets: usize,
	fail_set: bool,
	fail_query: bool,
}

struct Fake(Rc<RefCell<State>>);

impl Runner for Fake {
	fn run(&mut self, _: &str, args: &[&str]) -> Result<Output> {
		let mut state = self.0.borrow_mut();
		if args.is_empty() {
			if state.fail_query { bail!("query unavailable"); }
			return Ok(Output { code: 0, text: state.source.clone() });
		}
		state.sets += 1;
		if state.fail_set { bail!("set unavailable"); }
		state.source = args[0].to_owned();
		Ok(Output { code: 0, text: String::new() })
	}
}

fn fixture(source: &str) -> (Ime, Rc<RefCell<State>>) {
	let state = Rc::new(RefCell::new(State { source: source.to_owned(), ..Default::default() }));
	let ime = Ime {
		runner: Box::new(Fake(state.clone())), platform: Platform::Mac, snapshot: None,
		forced: false, running: true, focused: true, handoff_until: None, last_error: None,
	};
	(ime, state)
}

#[test]
fn command_and_edit_modes_restore_original_source_without_redundant_sets() -> Result<()> {
	let (mut ime, state) = fixture("im.rime.inputmethod.Squirrel.Hans");
	ime.sync(false)?;
	assert_eq!(state.borrow().source, "com.apple.keylayout.ABC");
	ime.sync(false)?;
	ime.poll(false)?;
	assert_eq!(state.borrow().sets, 1);
	ime.sync(true)?;
	assert_eq!(state.borrow().source, "im.rime.inputmethod.Squirrel.Hans");
	ime.sync(false)?;
	ime.quit()?;
	assert_eq!(state.borrow().source, "im.rime.inputmethod.Squirrel.Hans");
	Ok(())
}

#[test]
fn manual_selection_while_editing_is_sampled_on_next_command() -> Result<()> {
	let (mut ime, state) = fixture("com.apple.keylayout.ABC");
	ime.sync(false)?;
	ime.sync(true)?;
	state.borrow_mut().source = "im.rime.inputmethod.Squirrel.Hans".into();
	ime.sync(false)?;
	ime.sync(true)?;
	assert_eq!(state.borrow().source, "im.rime.inputmethod.Squirrel.Hans");
	Ok(())
}

#[test]
fn guard_recaptures_manual_change_and_respects_new_choice_on_release() -> Result<()> {
	let (mut ime, state) = fixture("im.rime.inputmethod.Squirrel.Hans");
	ime.sync(false)?;
	state.borrow_mut().source = "com.apple.inputmethod.SCIM.ITABC".into();
	ime.poll(false)?;
	assert_eq!(state.borrow().source, "com.apple.keylayout.ABC");
	ime.sync(true)?;
	assert_eq!(state.borrow().source, "com.apple.inputmethod.SCIM.ITABC");
	ime.sync(false)?;
	state.borrow_mut().source = "com.apple.inputmethod.SCIM.ITABC".into();
	ime.sync(true)?;
	assert_eq!(state.borrow().source, "com.apple.inputmethod.SCIM.ITABC");
	Ok(())
}

#[test]
fn failure_keeps_snapshot_and_retries_restoration() -> Result<()> {
	let (mut ime, state) = fixture("im.rime.inputmethod.Squirrel.Hans");
	ime.sync(false)?;
	state.borrow_mut().fail_set = true;
	assert!(ime.sync(true).is_err());
	assert_eq!(state.borrow().source, "com.apple.keylayout.ABC");
	state.borrow_mut().fail_set = false;
	ime.sync(true)?;
	assert_eq!(state.borrow().source, "im.rime.inputmethod.Squirrel.Hans");
	Ok(())
}

#[test]
fn failed_stop_retains_snapshot_for_retry_while_paused() -> Result<()> {
	let (mut ime, state) = fixture("im.rime.inputmethod.Squirrel.Hans");
	ime.sync(false)?;
	state.borrow_mut().fail_set = true;
	assert!(ime.stop().is_err());
	state.borrow_mut().fail_set = false;
	ime.sync(false)?;
	assert_eq!(state.borrow().source, "im.rime.inputmethod.Squirrel.Hans");
	Ok(())
}

#[test]
fn failed_acquire_uses_the_new_manual_source_on_retry() -> Result<()> {
	let (mut ime, state) = fixture("im.rime.inputmethod.Squirrel.Hans");
	state.borrow_mut().fail_set = true;
	assert!(ime.sync(false).is_err());
	state.borrow_mut().source = "com.apple.inputmethod.SCIM.ITABC".into();
	state.borrow_mut().fail_set = false;
	ime.poll(false)?;
	ime.sync(true)?;
	assert_eq!(state.borrow().source, "com.apple.inputmethod.SCIM.ITABC");
	Ok(())
}

#[test]
fn failed_query_never_acquires_and_focus_stop_resume_release() -> Result<()> {
	let (mut ime, state) = fixture("im.rime.inputmethod.Squirrel.Hans");
	state.borrow_mut().fail_query = true;
	assert!(ime.sync(false).is_err());
	assert_eq!(state.borrow().sets, 0);
	state.borrow_mut().fail_query = false;
	ime.sync(false)?;
	ime.handoff_until = None;
	ime.focus_out()?;
	assert_eq!(state.borrow().source, "im.rime.inputmethod.Squirrel.Hans");
	ime.focus_in(false)?;
	ime.stop()?;
	assert_eq!(state.borrow().source, "im.rime.inputmethod.Squirrel.Hans");
	ime.resume(false)?;
	ime.quit()?;
	assert_eq!(state.borrow().source, "im.rime.inputmethod.Squirrel.Hans");
	Ok(())
}

#[test]
fn macism_temporary_focus_leaves_lease_untouched_until_grace_expires() -> Result<()> {
	let (mut ime, state) = fixture("im.rime.inputmethod.Squirrel.Hans");
	ime.sync(false)?;
	ime.focus_out()?;
	assert_eq!(state.borrow().source, "com.apple.keylayout.ABC");
	ime.focus_in(false)?;
	ime.focus_out()?;
	ime.handoff_until = Some(Instant::now() - Duration::from_millis(1));
	ime.poll(false)?;
	assert_eq!(state.borrow().source, "im.rime.inputmethod.Squirrel.Hans");
	Ok(())
}

#[cfg(unix)]
#[test]
fn command_timeout_is_bounded() {
	let start = Instant::now();
	assert!(ProcessRunner::run_with_timeout("/bin/sleep", &["2"], Duration::from_millis(30)).is_err());
	assert!(start.elapsed() < Duration::from_secs(1));
}
