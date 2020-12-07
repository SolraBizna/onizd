/*
 *
 * This file is part of onizd, copyright ©2020 Solra Bizna.
 *
 * onizd is free software: you can redistribute it and/or modify it under the
 * terms of the GNU General Public License as published by the Free Software
 * Foundation, either version 3 of the License, or (at your option) any later
 * version.
 *
 * onizd is distributed in the hope that it will be useful, but WITHOUT ANY
 * WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
 * FOR A PARTICULAR PURPOSE. See the GNU General Public License for more
 * details.
 *
 * You should have received a copy of the GNU General Public License along with
 * onizd. If not, see <https://www.gnu.org/licenses/>.
 *
 */

use std::{
    rc::{Rc,Weak},
    cell::RefCell,
    {thread, thread::JoinHandle},
    time::Duration,
};
use tokio::sync::mpsc;

use gtk::{
    prelude::*,
    Align,
    Application,
    ApplicationWindow,
    BoxBuilder,
    Button, ButtonBuilder,
    CheckButton,
    Entry, EntryBuilder,
    InputPurpose,
    LabelBuilder,
    Orientation,
    PolicyType,
    ScrolledWindowBuilder,
    SeparatorBuilder,
    TextView, TextViewBuilder, TextBuffer,
};
use gio::prelude::*;
use glib;
use crate::{Invocation, Outputter};

/// The maximum number of bytes that the log is allowed to grow to.
const MAX_LOG_SIZE: i32 = 1_000_000; // this is a lot, okay
/// The number of lines to kill every time we truncate the log.
const LOG_TRUNC_LINES: i32 = 500;

/// Contains all the actual logic for the GUI.
struct Controller {
    _window: ApplicationWindow,
    listen_checkbox: CheckButton,
    listen_field: Entry,
    ping_checkbox: CheckButton,
    ping_field: Entry,
    verbose_checkbox: CheckButton,
    save_checkbox: CheckButton,
    save_field: Entry,
    start_button: Button,
    stop_button: Button,
    output_view: TextView,
    server_thread: Option<JoinHandle<()>>,
    terminator: Option<mpsc::Sender<()>>,
    server_canary: Option<mpsc::Receiver<()>>,
    self_ref: Option<Weak<RefCell<Controller>>>,
    log_tx: mpsc::UnboundedSender<String>,
    log_rx: mpsc::UnboundedReceiver<String>,
}

impl Controller {
    pub fn new(_window: ApplicationWindow,
               listen_checkbox: CheckButton,
               listen_field: Entry,
               ping_checkbox: CheckButton,
               ping_field: Entry,
               verbose_checkbox: CheckButton,
               save_checkbox: CheckButton,
               save_field: Entry,
               start_button: Button,
               stop_button: Button,
               output_view: TextView) -> Rc<RefCell<Controller>> {
        let (log_tx, log_rx) = mpsc::unbounded_channel();
        let ret = Rc::new(RefCell::new(Controller {
            _window, listen_checkbox, listen_field, ping_checkbox, ping_field,
            output_view, verbose_checkbox, save_checkbox, save_field,
            start_button, stop_button,
            server_thread: None, terminator: None, server_canary: None,
            self_ref: None, log_tx, log_rx,
        }));
        let rc = ret.clone();
        let mut me = rc.borrow_mut();
        me.self_ref = Some(Rc::downgrade(&ret)); // for later...
        let rc = ret.clone();
        me.listen_checkbox.connect_clicked(move |_| rc.borrow_mut().update_sensitive());
        let rc = ret.clone();
        me.ping_checkbox.connect_clicked(move |_| rc.borrow_mut().update_sensitive());
        let rc = ret.clone();
        me.start_button.connect_clicked(move |_| rc.borrow_mut().start_server());
        let rc = ret.clone();
        me.stop_button.connect_clicked(move |_| rc.borrow_mut().stop_server());
        ret
    }
    fn update_sensitive(&mut self) {
        match self.server_thread {
            None => {
                self.listen_checkbox.set_sensitive(true);
                self.ping_checkbox.set_sensitive(true);
                self.verbose_checkbox.set_sensitive(true);
                self.save_checkbox.set_sensitive(true);
                self.listen_field.set_sensitive(self.listen_checkbox.get_active());
                self.ping_field.set_sensitive(self.ping_checkbox.get_active());
                self.save_field.set_sensitive(self.save_checkbox.get_active());
                self.start_button.set_sensitive(true);
                self.stop_button.set_sensitive(false);
            },
            _ => {
                self.listen_checkbox.set_sensitive(false);
                self.ping_checkbox.set_sensitive(false);
                self.verbose_checkbox.set_sensitive(false);
                self.save_checkbox.set_sensitive(false);
                self.listen_field.set_sensitive(false);
                self.ping_field.set_sensitive(false);
                self.save_field.set_sensitive(false);
                self.start_button.set_sensitive(false);
                self.stop_button.set_sensitive(true);
            },
        }
    }
    fn start_server(&mut self) {
        if !self.server_thread.is_none() { return; }
        let invocation = match self.get_invocation() {
            Ok(x) => x,
            Err(x) => {
                self.append_text(&x);
                self.append_text("\n");
                return;
            }
        };
        let (termination_tx, termination_rx) = mpsc::channel(1);
        let termination_tx_clone = termination_tx.clone();
        let (canary_tx, canary_rx) = mpsc::channel(1);
        let log_tx = self.log_tx.clone();
        let neu = thread::Builder::new().name("onizd server thread".to_owned())
            .spawn(move || {
                let canary_tx = canary_tx;
                crate::true_main(invocation, termination_tx_clone,
                                 termination_rx, Outputter::Channel(log_tx));
                std::mem::drop(canary_tx); // explicit but unnecessary
            });
        match neu {
            Err(x) => {
                self.append_text(&format!("Unable to start the server: {}",x));
                return;
            },
            Ok(neu) => {
                self.server_thread = Some(neu);
                self.terminator = Some(termination_tx);
                self.server_canary = Some(canary_rx);
                self.update_sensitive();
                // neither of these unwraps should fail
                let rc = self.self_ref.as_ref().unwrap().upgrade().unwrap();
                glib::idle_add_local(move ||
                    Continue(rc.borrow_mut().check_server_status()));
            },
        }
    }
    fn stop_server(&mut self) {
        match &mut self.terminator {
            None => (),
            Some(terminator) => {
                let _ = terminator.try_send(());
            }
        }
    }
    /// Check if the server thread is [still] running. If it isn't, clean up
    /// the server thread stuff, call `update_sensitive`, and return false.
    ///
    /// Also reads the `log_tx` channel and appends any outputted log data to
    /// the output view.
    fn check_server_status(&mut self) -> bool {
        let ret =
        if self.server_thread.is_none() || self.server_canary.is_none() {
            false
        }
        else {
            match self.server_canary.as_mut().unwrap().try_recv() {
                // thread still running
                Err(mpsc::error::TryRecvError::Empty) => true,
                // thread has died
                _ => false,
            }
        };
        while let Ok(str) = self.log_rx.try_recv() {
            self.append_text(&str);
        }
        if ret == false {
            self.server_thread = None;
            self.terminator = None;
            self.server_canary = None;
            self.update_sensitive();
            self.append_text("Server is no longer running.");
        }
        ret
    }
    fn append_text(&mut self, text: &str) {
        let buffer = self.output_view.get_buffer().unwrap();
        if buffer.get_char_count() + text.len() as i32 > MAX_LOG_SIZE {
            self.truncate_buffer(&buffer);
        }
        buffer.insert(&mut buffer.get_end_iter(), text);
        self.output_view.scroll_to_iter(&mut buffer.get_end_iter(),
                                        0.0, true, 0.0, 1.0);
    }
    fn truncate_buffer(&mut self, buffer: &TextBuffer) {
        let (mut hajime, _) = buffer.get_bounds();
        let mut koko = hajime.clone();
        koko.forward_lines(LOG_TRUNC_LINES);
        buffer.delete(&mut hajime, &mut koko);
    }
    fn get_invocation(&mut self) -> Result<Invocation, String> {
        let save_file = if self.save_checkbox.get_active() {
            let gtext = self.save_field.get_text();
            let text = gtext.as_str();
            if text == "" { Some(self.save_field.get_placeholder_text().unwrap().as_str().to_owned()) }
            else if !text.ends_with(".json") { Some(text.to_owned() + ".json")}
            else { Some(text.to_owned()) }
        } else { None };
        let listen_addr = if self.listen_checkbox.get_active() {
            let gtext = self.listen_field.get_text();
            let text = gtext.as_str();
            if text == "" { None }
            else {
                match text.parse::<std::net::SocketAddr>() {
                    Ok(_) => Some(text.to_owned()),
                    Err(_) => return Err("Invalid listen address.".to_owned()),
                }
            }
        } else { None };
        let ping_interval = if self.ping_checkbox.get_active() {
            let gtext = self.ping_field.get_text();
            let text = gtext.as_str();
            if text == "" { None }
            else {
                match text.parse::<u64>() {
                    Ok(x) if x >= 1 && x <= 999  => Some(Duration::new(x, 0)),
                    _ => return Err("Invalid ping interval.".to_owned()),
                }
            }
        } else { None };
        let verbosity = if self.verbose_checkbox.get_active() { 1 } else { 0 };
        Ok(Invocation { listen_addr, ping_interval, verbosity, save_file,
                        offset_mode: false, auth_file: None })
    }
}

pub fn go() {
    const SPACING: i32 = 8;
    // TODO: right-to-left support...
    let application = Application::new(
        Some("name.bizna.onizd"),
        Default::default(),
    ).expect("failed to initialize GTK application");
    application.connect_activate(|app| {
        let window = ApplicationWindow::new(app);
        window.set_title("ONI ZTransport Server");
        window.set_default_size(500, 400);
        let big_box = BoxBuilder::new().orientation(Orientation::Vertical)
            .margin(SPACING).spacing(SPACING).build();
        // Row #0: listen address
        let little_box = BoxBuilder::new().spacing(SPACING).build();
        let listen_checkbox = CheckButton::new();
        let listen_label = LabelBuilder::new().label("Listen address:")
            .hexpand(false).build();
        let listen_field = EntryBuilder::new().hexpand(true).hexpand_set(true)
            .placeholder_text(crate::DEFAULT_ADDR_AND_PORT)
            .sensitive(false).build();
        little_box.add(&listen_checkbox);
        little_box.add(&listen_label);
        little_box.add(&listen_field);
        big_box.add(&little_box);
        // Row #1: ping interval, verbosity
        let little_box = BoxBuilder::new().spacing(SPACING).build();
        let ping_checkbox = CheckButton::new();
        let ping_label = LabelBuilder::new().label("Ping interval:")
            .halign(Align::Start).build();
        let ping_field = EntryBuilder::new().sensitive(false)
            .placeholder_text("60").width_request(80)
            .input_purpose(InputPurpose::Number).max_length(3).build();
        little_box.add(&ping_checkbox);
        little_box.add(&ping_label);
        little_box.add(&ping_field);
        let verbose_checkbox = CheckButton::new();
        let verbose_label = LabelBuilder::new()
            .label("Output more information").halign(Align::Start).build();
        little_box.add(&verbose_checkbox);
        little_box.add(&verbose_label);
        big_box.add(&little_box);
        // Row #2: saving-related things
        let little_box = BoxBuilder::new().spacing(SPACING).build();
        let save_checkbox = CheckButton::new();
        let save_label = LabelBuilder::new().label("Save in file:")
            .halign(Align::Start).build();
        let save_field = EntryBuilder::new().hexpand(true).hexpand_set(true)
            .placeholder_text("Saved Map.json")
            .sensitive(false).build();
        little_box.add(&save_checkbox);
        little_box.add(&save_label);
        little_box.add(&save_field);
        big_box.add(&little_box);
        // Row #3: buttons!
        let button_box = BoxBuilder::new().halign(Align::End).spacing(SPACING)
            .build();
        let stop_button = ButtonBuilder::new().label("Stop Server")
            .sensitive(false).build();
        button_box.add(&stop_button);
        let start_button = ButtonBuilder::new().label("Start Server").build();
        button_box.add(&start_button);
        big_box.add(&button_box);
        // Row #4: intermission...
        let separator = SeparatorBuilder::new().hexpand(true)
            .build();
        big_box.add(&separator);
        // Row #5: GIANT TEXT BOX
        let scroller = ScrolledWindowBuilder::new()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Always)
            .build();
        let output_view = TextViewBuilder::new()
            .editable(false).hexpand(true).expand(true).build();
        scroller.add(&output_view);
        output_view.connect_size_allocate(move |view, _| {
            view.scroll_to_iter(&mut view.get_buffer().unwrap().get_end_iter(),
                                0.0, true, 0.0, 1.0);
        });
        big_box.add(&scroller);
        window.add(&big_box);
        window.show_all();
        // Controller will keep track of itself
        Controller::new(window, listen_checkbox, listen_field, ping_checkbox,
                        ping_field, verbose_checkbox, save_checkbox,
                        save_field, start_button, stop_button, output_view);
    });
    application.run(&[]);
}
