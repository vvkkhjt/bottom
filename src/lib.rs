#![warn(rust_2018_idioms)]
#[allow(unused_imports)]
#[macro_use]
extern crate log;

use std::{
    boxed::Box,
    fs,
    io::{stdout, Write},
    panic::PanicInfo,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use crossterm::{
    event::{poll, read, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent},
    execute,
    style::Print,
    terminal::{disable_raw_mode, LeaveAlternateScreen},
};

use anyhow::Context;

use app::{
    data_harvester::{self, processes::ProcessSorting},
    layout_manager::{UsedWidgets, WidgetDirection},
    App,
};
use constants::*;
use data_conversion::*;
use options::*;
use utils::error;

pub mod app;

pub mod utils {
    pub mod error;
    pub mod gen_util;
    pub mod logging;
}

pub mod canvas;
pub mod constants;
pub mod data_conversion;
pub mod options;

pub mod clap;

#[cfg(target_family = "windows")]
pub type Pid = usize;

#[cfg(target_family = "unix")]
pub type Pid = libc::pid_t;

pub enum BottomEvent<I, J> {
    KeyInput(I),
    MouseInput(J),
    Update(Box<data_harvester::Data>),
    Clean,
}

pub enum ResetEvent {
    Reset,
}

pub fn handle_mouse_event(event: MouseEvent, app: &mut App) {
    match event {
        MouseEvent::ScrollUp(_x, _y, _modifiers) => app.handle_scroll_up(),
        MouseEvent::ScrollDown(_x, _y, _modifiers) => app.handle_scroll_down(),
        MouseEvent::Down(button, x, y, _modifiers) => {
            // debug!("Button down: {:?}, x: {}, y: {}", button, x, y);

            if !app.app_config_fields.disable_click {
                match button {
                    crossterm::event::MouseButton::Left => {
                        // Trigger left click widget activity
                        app.left_mouse_click_movement(x, y);
                    }
                    crossterm::event::MouseButton::Right => {}
                    _ => {}
                }
            }
        }
        _ => {}
    };
}

pub fn handle_key_event_or_break(
    event: KeyEvent, app: &mut App, reset_sender: &std::sync::mpsc::Sender<ResetEvent>,
) -> bool {
    // debug!("KeyEvent: {:?}", event);

    // TODO: [PASTE] Note that this does NOT support some emojis like flags.  This is due to us
    // catching PER CHARACTER right now WITH A forced throttle!  This means multi-char will not work.
    // We can solve this (when we do paste probably) while keeping the throttle (mainly meant for movement)
    // by throttling after *bulk+singular* actions, not just singular ones.

    if event.modifiers.is_empty() {
        // Required catch for searching - otherwise you couldn't search with q.
        if event.code == KeyCode::Char('q') && !app.is_in_search_widget() {
            return true;
        }
        match event.code {
            KeyCode::End => app.skip_to_last(),
            KeyCode::Home => app.skip_to_first(),
            KeyCode::Up => app.on_up_key(),
            KeyCode::Down => app.on_down_key(),
            KeyCode::Left => app.on_left_key(),
            KeyCode::Right => app.on_right_key(),
            KeyCode::Char(caught_char) => app.on_char_key(caught_char),
            KeyCode::Esc => app.on_esc(),
            KeyCode::Enter => app.on_enter(),
            KeyCode::Tab => app.on_tab(),
            KeyCode::Backspace => app.on_backspace(),
            KeyCode::Delete => app.on_delete(),
            KeyCode::F(1) => app.toggle_ignore_case(),
            KeyCode::F(2) => app.toggle_search_whole_word(),
            KeyCode::F(3) => app.toggle_search_regex(),
            KeyCode::F(5) => app.toggle_tree_mode(),
            KeyCode::F(6) => app.toggle_sort(),
            _ => {}
        }
    } else {
        // Otherwise, track the modifier as well...
        if let KeyModifiers::ALT = event.modifiers {
            match event.code {
                KeyCode::Char('c') | KeyCode::Char('C') => app.toggle_ignore_case(),
                KeyCode::Char('w') | KeyCode::Char('W') => app.toggle_search_whole_word(),
                KeyCode::Char('r') | KeyCode::Char('R') => app.toggle_search_regex(),
                KeyCode::Char('h') => app.on_left_key(),
                KeyCode::Char('l') => app.on_right_key(),
                _ => {}
            }
        } else if let KeyModifiers::CONTROL = event.modifiers {
            if event.code == KeyCode::Char('c') {
                return true;
            }

            match event.code {
                KeyCode::Char('f') => app.on_slash(),
                KeyCode::Left => app.move_widget_selection(&WidgetDirection::Left),
                KeyCode::Right => app.move_widget_selection(&WidgetDirection::Right),
                KeyCode::Up => app.move_widget_selection(&WidgetDirection::Up),
                KeyCode::Down => app.move_widget_selection(&WidgetDirection::Down),
                KeyCode::Char('r') => {
                    if reset_sender.send(ResetEvent::Reset).is_ok() {
                        app.reset();
                    }
                }
                KeyCode::Char('a') => app.skip_cursor_beginning(),
                KeyCode::Char('e') => app.skip_cursor_end(),
                KeyCode::Char('u') => app.clear_search(),
                // KeyCode::Char('j') => {}, // Move down
                // KeyCode::Char('k') => {}, // Move up
                // KeyCode::Char('h') => {}, // Move right
                // KeyCode::Char('l') => {}, // Move left
                // Can't do now, CTRL+BACKSPACE doesn't work and graphemes
                // are hard to iter while truncating last (eloquently).
                // KeyCode::Backspace => app.skip_word_backspace(),
                _ => {}
            }
        } else if let KeyModifiers::SHIFT = event.modifiers {
            match event.code {
                KeyCode::Left => app.move_widget_selection(&WidgetDirection::Left),
                KeyCode::Right => app.move_widget_selection(&WidgetDirection::Right),
                KeyCode::Up => app.move_widget_selection(&WidgetDirection::Up),
                KeyCode::Down => app.move_widget_selection(&WidgetDirection::Down),
                KeyCode::Char(caught_char) => app.on_char_key(caught_char),
                _ => {}
            }
        }
    }

    false
}

pub fn read_config(config_location: Option<&str>) -> error::Result<Option<PathBuf>> {
    let config_path = if let Some(conf_loc) = config_location {
        Some(PathBuf::from(conf_loc))
    } else if cfg!(target_os = "windows") {
        if let Some(home_path) = dirs::config_dir() {
            let mut path = home_path;
            path.push(DEFAULT_CONFIG_FILE_PATH);
            Some(path)
        } else {
            None
        }
    } else if let Some(home_path) = dirs::home_dir() {
        let mut path = home_path;
        path.push(".config/");
        path.push(DEFAULT_CONFIG_FILE_PATH);
        if path.exists() {
            // If it already exists, use the old one.
            Some(path)
        } else {
            // If it does not, use the new one!
            if let Some(config_path) = dirs::config_dir() {
                let mut path = config_path;
                path.push(DEFAULT_CONFIG_FILE_PATH);
                Some(path)
            } else {
                None
            }
        }
    } else {
        None
    };

    Ok(config_path)
}

pub fn create_or_get_config(config_path: &Option<PathBuf>) -> error::Result<Config> {
    if let Some(path) = config_path {
        if let Ok(config_string) = fs::read_to_string(path) {
            Ok(toml::from_str(config_string.as_str())?)
        } else {
            if let Some(parent_path) = path.parent() {
                fs::create_dir_all(parent_path)?;
            }
            fs::File::create(path)?.write_all(DEFAULT_CONFIG_CONTENT.as_bytes())?;
            Ok(toml::from_str(DEFAULT_CONFIG_CONTENT)?)
        }
    } else {
        // Don't write otherwise...
        Ok(toml::from_str(DEFAULT_CONFIG_CONTENT)?)
    }
}

pub fn try_drawing(
    terminal: &mut tui::terminal::Terminal<tui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut App, painter: &mut canvas::Painter,
) -> error::Result<()> {
    if let Err(err) = painter.draw_data(terminal, app) {
        cleanup_terminal(terminal)?;
        return Err(err);
    }

    Ok(())
}

pub fn generate_config_colours(
    config: &Config, painter: &mut canvas::Painter,
) -> anyhow::Result<()> {
    if let Some(colours) = &config.colors {
        if let Some(border_color) = &colours.border_color {
            painter
                .colours
                .set_border_colour(border_color)
                .context("Update 'border_color' in your config file..")?;
        }

        if let Some(highlighted_border_color) = &colours.highlighted_border_color {
            painter
                .colours
                .set_highlighted_border_colour(highlighted_border_color)
                .context("Update 'highlighted_border_color' in your config file..")?;
        }

        if let Some(text_color) = &colours.text_color {
            painter
                .colours
                .set_text_colour(text_color)
                .context("Update 'text_color' in your config file..")?;
        }

        if let Some(avg_cpu_color) = &colours.avg_cpu_color {
            painter
                .colours
                .set_avg_cpu_colour(avg_cpu_color)
                .context("Update 'avg_cpu_color' in your config file..")?;
        }

        if let Some(all_cpu_color) = &colours.all_cpu_color {
            painter
                .colours
                .set_all_cpu_colour(all_cpu_color)
                .context("Update 'all_cpu_color' in your config file..")?;
        }

        if let Some(cpu_core_colors) = &colours.cpu_core_colors {
            painter
                .colours
                .set_cpu_colours(cpu_core_colors)
                .context("Update 'cpu_core_colors' in your config file..")?;
        }

        if let Some(ram_color) = &colours.ram_color {
            painter
                .colours
                .set_ram_colour(ram_color)
                .context("Update 'ram_color' in your config file..")?;
        }

        if let Some(swap_color) = &colours.swap_color {
            painter
                .colours
                .set_swap_colour(swap_color)
                .context("Update 'swap_color' in your config file..")?;
        }

        if let Some(rx_color) = &colours.rx_color {
            painter
                .colours
                .set_rx_colour(rx_color)
                .context("Update 'rx_color' in your config file..")?;
        }

        if let Some(tx_color) = &colours.tx_color {
            painter
                .colours
                .set_tx_colour(tx_color)
                .context("Update 'tx_color' in your config file..")?;
        }

        // if let Some(rx_total_color) = &colours.rx_total_color {
        //     painter.colours.set_rx_total_colour(rx_total_color)?;
        // }

        // if let Some(tx_total_color) = &colours.tx_total_color {
        //     painter.colours.set_tx_total_colour(tx_total_color)?;
        // }

        if let Some(table_header_color) = &colours.table_header_color {
            painter
                .colours
                .set_table_header_colour(table_header_color)
                .context("Update 'table_header_color' in your config file..")?;
        }

        if let Some(scroll_entry_text_color) = &colours.selected_text_color {
            painter
                .colours
                .set_scroll_entry_text_color(scroll_entry_text_color)
                .context("Update 'selected_text_color' in your config file..")?;
        }

        if let Some(scroll_entry_bg_color) = &colours.selected_bg_color {
            painter
                .colours
                .set_scroll_entry_bg_color(scroll_entry_bg_color)
                .context("Update 'selected_bg_color' in your config file..")?;
        }

        if let Some(widget_title_color) = &colours.widget_title_color {
            painter
                .colours
                .set_widget_title_colour(widget_title_color)
                .context("Update 'widget_title_color' in your config file..")?;
        }

        if let Some(graph_color) = &colours.graph_color {
            painter
                .colours
                .set_graph_colour(graph_color)
                .context("Update 'graph_color' in your config file..")?;
        }

        if let Some(battery_colors) = &colours.battery_colors {
            painter
                .colours
                .set_battery_colors(battery_colors)
                .context("Update 'battery_colors' in your config file.")?;
        }
    }

    Ok(())
}

pub fn cleanup_terminal(
    terminal: &mut tui::terminal::Terminal<tui::backend::CrosstermBackend<std::io::Stdout>>,
) -> error::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    Ok(())
}

pub fn termination_hook() {
    let mut stdout = stdout();
    disable_raw_mode().unwrap();
    execute!(stdout, DisableMouseCapture, LeaveAlternateScreen).unwrap();
}

/// Based on https://github.com/Rigellute/spotify-tui/blob/master/src/main.rs
pub fn panic_hook(panic_info: &PanicInfo<'_>) {
    let mut stdout = stdout();

    let msg = match panic_info.payload().downcast_ref::<&'static str>() {
        Some(s) => *s,
        None => match panic_info.payload().downcast_ref::<String>() {
            Some(s) => &s[..],
            None => "Box<Any>",
        },
    };

    let stacktrace: String = format!("{:?}", backtrace::Backtrace::new());

    disable_raw_mode().unwrap();
    execute!(stdout, DisableMouseCapture, LeaveAlternateScreen).unwrap();

    // Print stack trace.  Must be done after!
    execute!(
        stdout,
        Print(format!(
            "thread '<unnamed>' panicked at '{}', {}\n\r{}",
            msg,
            panic_info.location().unwrap(),
            stacktrace
        )),
    )
    .unwrap();
}

pub fn handle_force_redraws(app: &mut App) {
    // Currently we use an Option... because we might want to future-proof this
    // if we eventually get widget-specific redrawing!
    if app.proc_state.force_update_all {
        update_all_process_lists(app);
        app.proc_state.force_update_all = false;
    } else if let Some(widget_id) = app.proc_state.force_update {
        update_final_process_list(app, widget_id);
        app.proc_state.force_update = None;
    }

    if app.cpu_state.force_update.is_some() {
        app.canvas_data.cpu_data = convert_cpu_data_points(&app.data_collection, app.is_frozen);
        app.cpu_state.force_update = None;
    }

    if app.mem_state.force_update.is_some() {
        app.canvas_data.mem_data = convert_mem_data_points(&app.data_collection, app.is_frozen);
        app.canvas_data.swap_data = convert_swap_data_points(&app.data_collection, app.is_frozen);
        app.mem_state.force_update = None;
    }

    if app.net_state.force_update.is_some() {
        let (rx, tx) = get_rx_tx_data_points(&app.data_collection, app.is_frozen);
        app.canvas_data.network_data_rx = rx;
        app.canvas_data.network_data_tx = tx;
        app.net_state.force_update = None;
    }
}

#[allow(clippy::needless_collect)]
pub fn update_all_process_lists(app: &mut App) {
    // According to clippy, I can avoid a collect... but if I follow it,
    // I end up conflicting with the borrow checker since app is used within the closure... hm.
    if !app.is_frozen {
        let widget_ids = app
            .proc_state
            .widget_states
            .keys()
            .cloned()
            .collect::<Vec<_>>();

        widget_ids.into_iter().for_each(|widget_id| {
            update_final_process_list(app, widget_id);
        });
    }
}

fn update_final_process_list(app: &mut App, widget_id: u64) {
    let process_states = match app.proc_state.widget_states.get(&widget_id) {
        Some(process_state) => Some((
            process_state
                .process_search_state
                .search_state
                .is_invalid_or_blank_search(),
            process_state.is_using_command,
            process_state.is_grouped,
            process_state.is_tree_mode,
        )),
        None => None,
    };

    if let Some((is_invalid_or_blank, is_using_command, is_grouped, is_tree)) = process_states {
        if !app.is_frozen {
            app.canvas_data.single_process_data = convert_process_data(&app.data_collection);
        }

        let process_filter = app.get_process_filter(widget_id);
        let filtered_process_data: Vec<ConvertedProcessData> = if is_tree {
            app.canvas_data
                .single_process_data
                .iter()
                .map(|process| {
                    let mut process_clone = process.clone();
                    if !is_invalid_or_blank {
                        if let Some(process_filter) = process_filter {
                            process_clone.is_disabled_entry =
                                !process_filter.check(&process_clone, is_using_command);
                        }
                    }
                    process_clone
                })
                .collect::<Vec<_>>()
        } else {
            app.canvas_data
                .single_process_data
                .iter()
                .filter(|process| {
                    if !is_invalid_or_blank {
                        if let Some(process_filter) = process_filter {
                            process_filter.check(&process, is_using_command)
                        } else {
                            true
                        }
                    } else {
                        true
                    }
                })
                .cloned()
                .collect::<Vec<_>>()
        };

        if let Some(proc_widget_state) = app.proc_state.get_mut_widget_state(widget_id) {
            let mut finalized_process_data = if is_tree {
                tree_process_data(
                    &filtered_process_data,
                    is_using_command,
                    &proc_widget_state.process_sorting_type,
                    proc_widget_state.is_process_sort_descending,
                )
            } else if is_grouped {
                group_process_data(&filtered_process_data, is_using_command)
            } else {
                filtered_process_data
            };

            // Note tree mode is sorted well before this, as it's special.
            if !is_tree {
                sort_process_data(&mut finalized_process_data, proc_widget_state);
            }

            if proc_widget_state.scroll_state.current_scroll_position
                >= finalized_process_data.len()
            {
                proc_widget_state.scroll_state.current_scroll_position =
                    finalized_process_data.len().saturating_sub(1);
                proc_widget_state.scroll_state.previous_scroll_position = 0;
                proc_widget_state.scroll_state.scroll_direction = app::ScrollDirection::Down;
            }

            app.canvas_data
                .finalized_process_data_map
                .insert(widget_id, finalized_process_data);
        }
    }
}

fn sort_process_data(
    to_sort_vec: &mut Vec<ConvertedProcessData>, proc_widget_state: &app::ProcWidgetState,
) {
    to_sort_vec.sort_by(|a, b| {
        utils::gen_util::get_ordering(&a.name.to_lowercase(), &b.name.to_lowercase(), false)
    });

    match &proc_widget_state.process_sorting_type {
        ProcessSorting::CpuPercent => {
            to_sort_vec.sort_by(|a, b| {
                utils::gen_util::get_ordering(
                    a.cpu_percent_usage,
                    b.cpu_percent_usage,
                    proc_widget_state.is_process_sort_descending,
                )
            });
        }
        ProcessSorting::Mem => {
            to_sort_vec.sort_by(|a, b| {
                utils::gen_util::get_ordering(
                    a.mem_usage_bytes,
                    b.mem_usage_bytes,
                    proc_widget_state.is_process_sort_descending,
                )
            });
        }
        ProcessSorting::MemPercent => {
            to_sort_vec.sort_by(|a, b| {
                utils::gen_util::get_ordering(
                    a.mem_percent_usage,
                    b.mem_percent_usage,
                    proc_widget_state.is_process_sort_descending,
                )
            });
        }
        ProcessSorting::ProcessName => {
            // Don't repeat if false... it sorts by name by default anyways.
            if proc_widget_state.is_process_sort_descending {
                to_sort_vec.sort_by(|a, b| {
                    utils::gen_util::get_ordering(
                        &a.name.to_lowercase(),
                        &b.name.to_lowercase(),
                        proc_widget_state.is_process_sort_descending,
                    )
                })
            }
        }
        ProcessSorting::Command => to_sort_vec.sort_by(|a, b| {
            utils::gen_util::get_ordering(
                &a.command.to_lowercase(),
                &b.command.to_lowercase(),
                proc_widget_state.is_process_sort_descending,
            )
        }),
        ProcessSorting::Pid => {
            if !proc_widget_state.is_grouped {
                to_sort_vec.sort_by(|a, b| {
                    utils::gen_util::get_ordering(
                        a.pid,
                        b.pid,
                        proc_widget_state.is_process_sort_descending,
                    )
                });
            }
        }
        ProcessSorting::ReadPerSecond => {
            to_sort_vec.sort_by(|a, b| {
                utils::gen_util::get_ordering(
                    a.rps_f64,
                    b.rps_f64,
                    proc_widget_state.is_process_sort_descending,
                )
            });
        }
        ProcessSorting::WritePerSecond => {
            to_sort_vec.sort_by(|a, b| {
                utils::gen_util::get_ordering(
                    a.wps_f64,
                    b.wps_f64,
                    proc_widget_state.is_process_sort_descending,
                )
            });
        }
        ProcessSorting::TotalRead => {
            to_sort_vec.sort_by(|a, b| {
                utils::gen_util::get_ordering(
                    a.tr_f64,
                    b.tr_f64,
                    proc_widget_state.is_process_sort_descending,
                )
            });
        }
        ProcessSorting::TotalWrite => {
            to_sort_vec.sort_by(|a, b| {
                utils::gen_util::get_ordering(
                    a.tw_f64,
                    b.tw_f64,
                    proc_widget_state.is_process_sort_descending,
                )
            });
        }
        ProcessSorting::State => to_sort_vec.sort_by(|a, b| {
            utils::gen_util::get_ordering(
                &a.process_state.to_lowercase(),
                &b.process_state.to_lowercase(),
                proc_widget_state.is_process_sort_descending,
            )
        }),
        ProcessSorting::Count => {
            if proc_widget_state.is_grouped {
                to_sort_vec.sort_by(|a, b| {
                    utils::gen_util::get_ordering(
                        a.group_pids.len(),
                        b.group_pids.len(),
                        proc_widget_state.is_process_sort_descending,
                    )
                });
            }
        }
    }
}

pub fn create_input_thread(
    sender: std::sync::mpsc::Sender<
        BottomEvent<crossterm::event::KeyEvent, crossterm::event::MouseEvent>,
    >,
) {
    thread::spawn(move || {
        let mut mouse_timer = Instant::now();
        let mut keyboard_timer = Instant::now();

        loop {
            if poll(Duration::from_millis(20)).is_ok() {
                if let Ok(event) = read() {
                    if let Event::Key(key) = event {
                        if Instant::now().duration_since(keyboard_timer).as_millis() >= 20 {
                            if sender.send(BottomEvent::KeyInput(key)).is_err() {
                                break;
                            }
                            keyboard_timer = Instant::now();
                        }
                    } else if let Event::Mouse(mouse) = event {
                        if Instant::now().duration_since(mouse_timer).as_millis() >= 20 {
                            if sender.send(BottomEvent::MouseInput(mouse)).is_err() {
                                break;
                            }
                            mouse_timer = Instant::now();
                        }
                    }
                }
            }
        }
    });
}

pub fn create_event_thread(
    sender: std::sync::mpsc::Sender<
        BottomEvent<crossterm::event::KeyEvent, crossterm::event::MouseEvent>,
    >,
    reset_receiver: std::sync::mpsc::Receiver<ResetEvent>,
    app_config_fields: &app::AppConfigFields, used_widget_set: UsedWidgets,
) {
    let temp_type = app_config_fields.temperature_type.clone();
    let use_current_cpu_total = app_config_fields.use_current_cpu_total;
    let show_average_cpu = app_config_fields.show_average_cpu;
    let update_rate_in_milliseconds = app_config_fields.update_rate_in_milliseconds;

    thread::spawn(move || {
        let mut data_state = data_harvester::DataCollector::default();
        data_state.set_collected_data(used_widget_set);
        data_state.set_temperature_type(temp_type);
        data_state.set_use_current_cpu_total(use_current_cpu_total);
        data_state.set_show_average_cpu(show_average_cpu);

        data_state.init();
        loop {
            if let Ok(message) = reset_receiver.try_recv() {
                match message {
                    ResetEvent::Reset => {
                        data_state.data.first_run_cleanup();
                    }
                }
            }
            futures::executor::block_on(data_state.update_data());
            let event = BottomEvent::Update(Box::from(data_state.data));
            data_state.data = data_harvester::Data::default();
            if sender.send(event).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(update_rate_in_milliseconds));
        }
    });
}
