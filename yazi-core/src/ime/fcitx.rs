use std::{env, path::Path};

#[cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt};

use anyhow::{Result, bail, ensure};

use super::{Runner, Source};

#[derive(Clone, Copy)]
enum Cli {
	Fcitx5,
	Fcitx4,
}

impl Cli {
	fn binary(self) -> &'static str {
		match self {
			Self::Fcitx5 => "fcitx5-remote",
			Self::Fcitx4 => "fcitx-remote",
		}
	}
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
	fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(_path: &Path) -> bool {
	false
}

fn discover() -> Option<Cli> {
	let path = env::var_os("PATH")?;
	[Cli::Fcitx5, Cli::Fcitx4]
		.into_iter()
		.find(|cli| env::split_paths(&path).any(|dir| executable(&dir.join(cli.binary()))))
}

fn available_session() -> bool {
	fn present(key: &str) -> bool {
		env::var_os(key).is_some_and(|value| !value.is_empty())
	}

	env::var_os("SSH_CONNECTION").is_none()
		&& env::var_os("SSH_TTY").is_none()
		&& (present("DISPLAY") || present("WAYLAND_DISPLAY"))
}

pub(super) fn query(runner: &mut dyn Runner) -> Result<Source> {
	ensure!(available_session(), "Fcitx requires a local graphical session (not SSH)");
	let cli = discover().ok_or_else(|| anyhow::anyhow!("neither fcitx5-remote nor fcitx-remote is executable in PATH"))?;
	query_with(runner, cli)
}

fn query_with(runner: &mut dyn Runner, cli: Cli) -> Result<Source> {
	let binary = cli.binary();
	let status = runner.run(binary, &[])?;
	let active = match cli {
		Cli::Fcitx5 => {
			ensure!(status.code == 0, "{binary} status failed ({})", status.code);
			match status.text.trim() {
				"1" => 1,
				"2" => 2,
				"0" => bail!("{binary} is unavailable"),
				_ => bail!("{binary} returned an invalid status"),
			}
		}
		Cli::Fcitx4 => match (status.code, status.text.trim()) {
			(0 | 1, "1") | (1, "") => 1,
			(0 | 2, "2") | (2, "") => 2,
			(0, "0" | "") => bail!("{binary} is unavailable"),
			_ => bail!("{binary} returned an invalid status"),
		},
	};

	match cli {
		Cli::Fcitx5 => {
			let method = runner.run(binary, &["-n"])?;
			ensure!(method.code == 0, "{binary} -n failed ({})", method.code);
			let method = method.text.trim();
			ensure!(!method.is_empty(), "{binary} returned an empty input method");
			Ok(Source::Fcitx5 { method: method.into(), active })
		}
		Cli::Fcitx4 => Ok(Source::Fcitx4(active)),
	}
}

fn checked(runner: &mut dyn Runner, binary: &str, args: &[&str]) -> Result<()> {
	let result = runner.run(binary, args)?;
	ensure!(result.code == 0, "{binary} {} failed ({})", args[0], result.code);
	Ok(())
}

pub(super) fn english(runner: &mut dyn Runner, current: &Source) -> Result<()> {
	let binary = match current {
		Source::Fcitx5 { method, active: 1 | 2 } if !method.is_empty() => Cli::Fcitx5.binary(),
		Source::Fcitx4(1 | 2) => Cli::Fcitx4.binary(),
		_ => bail!("invalid Fcitx source for English switch"),
	};
	checked(runner, binary, &["-c"])
}

pub(super) fn restore(runner: &mut dyn Runner, saved: &Source) -> Result<()> {
	match saved {
		Source::Fcitx5 { method, active: 1 | 2 } if !method.is_empty() => {
			let binary = Cli::Fcitx5.binary();
			checked(runner, binary, &["-s", method])?;
			checked(runner, binary, &[if is_english(saved) { "-c" } else { "-o" }])
		}
		Source::Fcitx4(1 | 2) => {
			checked(runner, Cli::Fcitx4.binary(), &[if is_english(saved) { "-c" } else { "-o" }])
		}
		_ => bail!("invalid Fcitx source for restoration"),
	}
}

pub(super) fn is_english(source: &Source) -> bool {
	matches!(source, Source::Fcitx5 { active: 1, .. } | Source::Fcitx4(1))
}

#[cfg(test)]
mod tests {
	use std::collections::VecDeque;

	use super::*;
	use crate::ime::Output;

	struct Fake {
		steps: VecDeque<(&'static str, Vec<&'static str>, Result<Output>)>,
	}

	impl Fake {
		fn new(steps: Vec<(&'static str, Vec<&'static str>, i32, &'static str)>) -> Self {
			Self {
				steps: steps.into_iter().map(|(binary, args, code, text)| (binary, args, Ok(Output { code, text: text.into() }))).collect(),
			}
		}

		fn done(&self) {
			assert!(self.steps.is_empty(), "unexecuted commands");
		}
	}

	impl Runner for Fake {
		fn run(&mut self, binary: &str, args: &[&str]) -> Result<Output> {
			let (expected_binary, expected_args, result) = self.steps.pop_front().expect("unexpected command");
			assert_eq!(binary, expected_binary);
			assert_eq!(args, expected_args);
			result
		}
	}

	#[test]
	fn fcitx5_restores_method_and_activation() {
		for (state, restore_flag) in [("1", "-c"), ("2", "-o")] {
			let mut fake = Fake::new(vec![
				("fcitx5-remote", vec![], 0, state),
				("fcitx5-remote", vec!["-n"], 0, "pinyin\n"),
				("fcitx5-remote", vec!["-c"], 0, ""),
				("fcitx5-remote", vec!["-s", "pinyin"], 0, ""),
				("fcitx5-remote", vec![restore_flag], 0, ""),
			]);
			let saved = query_with(&mut fake, Cli::Fcitx5).unwrap();
			assert_eq!(is_english(&saved), state == "1");
			english(&mut fake, &saved).unwrap();
			restore(&mut fake, &saved).unwrap();
			fake.done();
		}
	}

	#[test]
	fn fcitx4_restores_activation_without_changing_method() {
		for (code, text, flag) in [
			(0, "1\n", "-c"),
			(0, "2\n", "-o"),
			(1, "1\n", "-c"),
			(2, "2\n", "-o"),
			(2, "", "-o"),
			(1, "", "-c"),
		] {
			let mut fake = Fake::new(vec![
				("fcitx-remote", vec![], code, text),
				("fcitx-remote", vec!["-c"], 0, ""),
				("fcitx-remote", vec![flag], 0, ""),
			]);
			let saved = query_with(&mut fake, Cli::Fcitx4).unwrap();
			english(&mut fake, &saved).unwrap();
			restore(&mut fake, &saved).unwrap();
			fake.done();
		}
	}

	#[test]
	fn unavailable_or_missing_method_never_switches() {
		for cli in [Cli::Fcitx5, Cli::Fcitx4] {
			let mut fake = Fake::new(vec![(cli.binary(), vec![], 0, "0\n")]);
			assert!(query_with(&mut fake, cli).is_err());
			fake.done();
		}
		let mut fake = Fake::new(vec![("fcitx5-remote", vec![], 1, "2\n")]);
		assert!(query_with(&mut fake, Cli::Fcitx5).is_err());
		fake.done();
		for (code, method) in [(0, "\n"), (1, "pinyin\n")] {
			let mut fake = Fake::new(vec![
				("fcitx5-remote", vec![], 0, "2\n"),
				("fcitx5-remote", vec!["-n"], code, method),
			]);
			assert!(query_with(&mut fake, Cli::Fcitx5).is_err());
			fake.done();
		}
		let mut fake = Fake::new(vec![]);
		assert!(english(&mut fake, &Source::Fcitx4(0)).is_err());
		assert!(restore(&mut fake, &Source::Fcitx5 { method: String::new(), active: 2 }).is_err());
		fake.done();
	}

	#[test]
	fn failed_method_restore_does_not_change_activation() {
		let saved = Source::Fcitx5 { method: "pinyin".into(), active: 2 };
		let mut fake = Fake::new(vec![("fcitx5-remote", vec!["-s", "pinyin"], 1, "")]);
		assert!(restore(&mut fake, &saved).is_err());
		fake.done();
	}
}
