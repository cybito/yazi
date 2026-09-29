use anyhow::{Result, bail};

use super::{Output, Runner, Source};

const MACISM: &str = "/opt/homebrew/Cellar/macism/3.1.1/bin/macism";
const ENGLISH: &str = "com.apple.keylayout.ABC";
const SQUIRREL_PREFIX: &str = "im.rime.inputmethod.Squirrel.";

pub(super) fn query(runner: &mut dyn Runner) -> Result<Source> {
	let output = runner.run(MACISM, &[])?;
	check_status(&output)?;

	let source = output.text.trim();
	if source.is_empty() {
		bail!("macism returned an empty input source");
	}
	Ok(Source::Mac(source.to_owned()))
}

pub(super) fn english(runner: &mut dyn Runner, current: &Source) -> Result<()> {
	if !matches!(current, Source::Mac(_)) {
		bail!("cannot set macOS input source from a non-macOS state");
	}
	check_status(&runner.run(MACISM, &[ENGLISH])?)
}

pub(super) fn restore(runner: &mut dyn Runner, saved: &Source) -> Result<()> {
	let Source::Mac(source) = saved else {
		bail!("cannot restore macOS input source from a non-macOS state");
	};
	let args: &[&str] = if source.starts_with(SQUIRREL_PREFIX) { &[source, "0"] } else { &[source] };
	check_status(&runner.run(MACISM, args)?)
}

pub(super) fn is_english(source: &Source) -> bool {
	matches!(source, Source::Mac(id) if id == ENGLISH)
}

fn check_status(output: &Output) -> Result<()> {
	if output.code != 0 {
		bail!("macism failed with exit status {}: {}", output.code, output.text.trim());
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	struct Fake {
		steps: Vec<(Vec<String>, Result<Output>)>,
	}

	impl Fake {
		fn new(steps: Vec<(Vec<&str>, Result<Output>)>) -> Self {
			Self { steps: steps.into_iter().rev().map(|(args, result)| (args.into_iter().map(str::to_owned).collect(), result)).collect() }
		}
	}

	impl Runner for Fake {
		fn run(&mut self, binary: &str, args: &[&str]) -> Result<Output> {
			assert_eq!(binary, MACISM);
			let (expected, result) = self.steps.pop().expect("unexpected macism call");
			assert_eq!(args, expected);
			result
		}
	}

	fn ok(text: &str) -> Result<Output> {
		Ok(Output { code: 0, text: text.to_owned() })
	}

	#[test]
	fn captures_chinese_source_and_switches_to_abc() -> Result<()> {
		let mut runner = Fake::new(vec![
			(vec![], ok(" im.rime.inputmethod.Squirrel.Hans\n")),
			(vec![ENGLISH], ok("")),
		]);
		let saved = query(&mut runner)?;
		assert!(matches!(&saved, Source::Mac(id) if id == "im.rime.inputmethod.Squirrel.Hans"));
		assert!(!is_english(&saved));
		english(&mut runner, &saved)?;
		assert!(runner.steps.is_empty());
		Ok(())
	}

	#[test]
	fn abc_is_english_but_not_similar_ids() -> Result<()> {
		let mut runner = Fake::new(vec![(vec![], ok("com.apple.keylayout.ABC\n"))]);
		assert!(is_english(&query(&mut runner)?));
		assert!(!is_english(&Source::Mac(format!("{ENGLISH}.other"))));
		assert!(!is_english(&Source::Fcitx4(2)));
		Ok(())
	}

	#[test]
	fn failed_or_empty_query_never_yields_a_snapshot() {
		let mut runner = Fake::new(vec![
			(vec![], Ok(Output { code: 1, text: "unavailable".into() })),
			(vec![], ok("  \n")),
		]);
		assert!(query(&mut runner).is_err());
		assert!(query(&mut runner).is_err());
		assert!(runner.steps.is_empty());
	}

	#[test]
	fn restores_squirrel_without_stealing_focus_and_other_sources_normally() -> Result<()> {
		let mut runner = Fake::new(vec![
			(vec!["im.rime.inputmethod.Squirrel.Hans", "0"], ok("")),
			(vec!["com.apple.inputmethod.SCIM.ITABC"], ok("")),
		]);
		restore(&mut runner, &Source::Mac("im.rime.inputmethod.Squirrel.Hans".into()))?;
		restore(&mut runner, &Source::Mac("com.apple.inputmethod.SCIM.ITABC".into()))?;
		assert!(runner.steps.is_empty());
		Ok(())
	}

	#[test]
	fn rejects_foreign_state_and_failed_setters() {
		let mut runner = Fake::new(vec![
			(vec![ENGLISH], Ok(Output { code: 2, text: "failure".into() })),
			(vec!["com.apple.inputmethod.SCIM.ITABC"], Ok(Output { code: 3, text: "failure".into() })),
		]);
		assert!(english(&mut runner, &Source::Fcitx4(2)).is_err());
		assert!(restore(&mut runner, &Source::Fcitx4(2)).is_err());
		assert!(english(&mut runner, &Source::Mac("Chinese".into())).is_err());
		assert!(restore(&mut runner, &Source::Mac("com.apple.inputmethod.SCIM.ITABC".into())).is_err());
		assert!(runner.steps.is_empty());
	}
}
