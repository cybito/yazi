use std::{sync::atomic::Ordering, time::{Duration, Instant}};

use anyhow::Result;
use tokio::time::sleep;
use yazi_actor::Ctx;
use yazi_core::{Core, notify::{MessageLevel, MessageOpt}};
use yazi_macro::{act, render, succ, warn};
use yazi_shared::{data::Data, event::{Event, EventRx, NEED_RENDER}};
use yazi_tui::Raterm;

use crate::Dispatcher;

pub(crate) struct App {
	pub(crate) core: Core,
	pub(crate) term: Option<Raterm>,

	pub(super) need_render: u8,
	pub(crate) last_render: Instant,
	next_render:            Option<Duration>,
}

impl App {
	fn make(term: Raterm) -> Result<Self> {
		Ok(Self {
			core: Core::make(),
			term: Some(term),

			need_render: 0,
			last_render: Instant::now(),
			next_render: None,
		})
	}

	pub(crate) async fn serve() -> Result<()> {
		let term = Raterm::start()?;

		let mut app = Self::make(term)?;
		app.bootstrap()?;
		let editing = app.core.editing();
		app.core.ime.focus_in(editing)?;

		let mut rx = Event::take();
		loop {
			if let Some(t) = app.next_render.take() {
				tokio::select! {
					_ = sleep(t) => { app.render()?; },
					r = app.drain(&mut rx) => if !r? { break; }
				}
			} else if !app.drain(&mut rx).await? {
				break;
			}
		}
		Ok(())
	}

	fn bootstrap(&mut self) -> Result<Data> {
		let cx = &mut Ctx::active(&mut self.core, &mut self.term);
		act!(app:bootstrap, cx)?;
		act!(app:reflow, cx, crate::Root::reflow as fn(_) -> _)?;
		succ!(render!())
	}

	async fn drain(&mut self, rx: &mut EventRx) -> Result<bool> {
		let Some(event) = rx.recv().await else {
			return Ok(false);
		};

		self.dispatch(event)?;
		while let Ok(e) = rx.try_recv() {
			self.dispatch(e)?;
		}

		Ok(true)
	}

	fn dispatch(&mut self, event: Event) -> Result<()> {
		Dispatcher::new(self).dispatch(event);
		self.sync_ime()?;

		self.schedule_render()
	}

	fn schedule_render(&mut self) -> Result<()> {
		self.need_render = NEED_RENDER.load(Ordering::Relaxed);
		if self.need_render == 0 {
			return Ok(());
		}

		self.next_render = Duration::from_millis(10).checked_sub(self.last_render.elapsed());
		if self.next_render.is_none() {
			self.render()?;
		}
		Ok(())
	}
	pub(super) fn sync_ime(&mut self) -> Result<()> {
		// A missing or rejected ACK must terminate the TUI, not trap the user
		// behind a command-key gate with no working quit key.
		self.core.ime.sync(self.core.editing())
	}

	/// Genuine terminal input can start a new episode; render/business sync cannot.
	pub(crate) fn input_ime_ready(&mut self) -> bool {
		let editing = self.core.editing();
		let result = self.core.ime.input(editing);
		let ready = result.is_ok() && (editing || self.core.ime.command_ready());
		self.report_ime(result);
		ready
	}

	pub(crate) fn report_ime(&mut self, result: Result<()>) {
		if let Some(message) = self.core.ime.report(result) {
			warn!("IME: {message}");
			let cx = &mut Ctx::active(&mut self.core, &mut self.term);
			act!(notify:push, cx, MessageOpt {
				title: "Input method".to_owned(),
				content: message,
				level: MessageLevel::Warn,
				timeout: Duration::from_secs(8),
			}).ok();
		}
	}
}
