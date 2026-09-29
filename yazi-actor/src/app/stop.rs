use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::task;
use yazi_core::notify::{MessageLevel, MessageOpt};
use yazi_emulator::EMULATOR;
use yazi_macro::{act, succ};
use yazi_parser::app::StopForm;
use yazi_scheduler::AppProxy;
use yazi_shared::data::Data;

use crate::{Actor, Ctx};

pub struct Stop;

impl Actor for Stop {
	type Form = StopForm;

	const NAME: &str = "stop";

	fn act(cx: &mut Ctx, Self::Form { replier }: Self::Form) -> Result<Data> {
		if let Some(id) = EMULATOR.probe.pending() {
			task::spawn_local(async move {
				EMULATOR.probe.wait(id).await;
				AppProxy::stop_with(replier);
			});
			succ!();
		}
		if let Err(error) = cx.ime.stop() {
			let message = format!("Cannot release IME control before terminal handoff: {error}");
			act!(notify:push, cx, MessageOpt {
				title: "Input method".to_owned(),
				content: message.clone(),
				level: MessageLevel::Warn,
				timeout: Duration::from_secs(8),
			}).ok();
			if let Some(replier) = replier {
				replier.send(Err(anyhow!(message))).ok();
			}
			return Err(error);
		}

		cx.active_mut().preview.reset_image();

		*cx.term = None;

		if let Some(replier) = replier {
			replier.send(Ok(Data::Nil)).ok();
		}

		succ!();
	}
}
