use std::{collections::HashMap, rc::Rc, sync::Arc};

use floem::reactive::{RwSignal, Scope};
use lapce_core::mode::Mode;
use lapce_rpc::{
    dap_types::{DapId, RunDebugConfig, StackFrame, Stopped, ThreadId},
    terminal::TermId,
};

use super::{data::TerminalData, tab::TerminalTabData};
use crate::{
    debug::{DapData, RunDebugData, RunDebugMode, RunDebugProcess},
    id::TerminalTabId,
    keypress::{EventRef, KeyPressData, KeyPressFocus},
    panel::kind::PanelKind,
    window_tab::{CommonData, Focus},
    workspace::LapceWorkspace,
};

pub struct TerminalTabInfo {
    pub active: usize,
    pub tabs: im::Vector<(RwSignal<usize>, TerminalTabData)>,
}

#[derive(Clone)]
pub struct TerminalPanelData {
    pub cx: Scope,
    pub workspace: Arc<LapceWorkspace>,
    pub tab_info: RwSignal<TerminalTabInfo>,
    pub debug: RunDebugData,
    pub common: Rc<CommonData>,
}

impl TerminalPanelData {
    pub fn new(
        workspace: Arc<LapceWorkspace>,
        run_debug: Option<RunDebugProcess>,
        common: Rc<CommonData>,
    ) -> Self {
        let terminal_tab =
            TerminalTabData::new(workspace.clone(), run_debug, common.clone());

        let cx = common.scope;

        let tabs =
            im::vector![(terminal_tab.scope.create_rw_signal(0), terminal_tab)];
        let tab_info = TerminalTabInfo { active: 0, tabs };
        let tab_info = cx.create_rw_signal(tab_info);

        let debug = RunDebugData::new(cx);

        Self {
            cx,
            workspace,
            tab_info,
            debug,
            common,
        }
    }

    pub fn active_tab(&self, tracked: bool) -> Option<TerminalTabData> {
        if tracked {
            self.tab_info.with(|info| {
                info.tabs
                    .get(info.active)
                    .or_else(|| info.tabs.last())
                    .cloned()
                    .map(|(_, tab)| tab)
            })
        } else {
            self.tab_info.with_untracked(|info| {
                info.tabs
                    .get(info.active)
                    .or_else(|| info.tabs.last())
                    .cloned()
                    .map(|(_, tab)| tab)
            })
        }
    }

    pub fn key_down<'a>(
        &self,
        event: impl Into<EventRef<'a>> + Copy,
        keypress: &KeyPressData,
    ) -> bool {
        if self.tab_info.with_untracked(|info| info.tabs.is_empty()) {
            self.new_tab(None);
        }

        let tab = self.active_tab(false);
        let terminal = tab.and_then(|tab| tab.active_terminal(false));
        if let Some(terminal) = terminal {
            let executed = keypress.key_down(event, &terminal);
            let mode = terminal.get_mode();

            if !executed && mode == Mode::Terminal {
                if let EventRef::Keyboard(key_event) = event.into() {
                    if terminal.send_keypress(key_event) {
                        return true;
                    }
                }
            }
            executed
        } else {
            false
        }
    }

    pub fn new_tab(&self, run_debug: Option<RunDebugProcess>) -> TerminalTabData {
        let terminal_tab = TerminalTabData::new(
            self.workspace.clone(),
            run_debug,
            self.common.clone(),
        );

        self.tab_info.update(|info| {
            info.tabs.insert(
                if info.tabs.is_empty() {
                    0
                } else {
                    (info.active + 1).min(info.tabs.len())
                },
                (terminal_tab.scope.create_rw_signal(0), terminal_tab.clone()),
            );
            let new_active = (info.active + 1).min(info.tabs.len() - 1);
            info.active = new_active;
        });

        terminal_tab
    }

    pub fn next_tab(&self) {
        self.tab_info.update(|info| {
            if info.active >= info.tabs.len().saturating_sub(1) {
                info.active = 0;
            } else {
                info.active += 1;
            }
        });
        self.update_debug_active_term();
    }

    pub fn previous_tab(&self) {
        self.tab_info.update(|info| {
            if info.active == 0 {
                info.active = info.tabs.len().saturating_sub(1);
            } else {
                info.active -= 1;
            }
        });
        self.update_debug_active_term();
    }

    pub fn close_tab(&self, terminal_tab_id: Option<TerminalTabId>) {
        self.tab_info.update(|info| {
            if let Some(terminal_tab_id) = terminal_tab_id {
                info.tabs
                    .retain(|(_, t)| t.terminal_tab_id != terminal_tab_id);
            } else {
                let active = info.active.min(info.tabs.len().saturating_sub(1));
                if !info.tabs.is_empty() {
                    info.tabs.remove(active);
                }
            }
            let new_active = info.active.min(info.tabs.len().saturating_sub(1));
            info.active = new_active;
        });
        self.update_debug_active_term();
    }

    pub fn set_title(&self, term_id: &TermId, title: &str) {
        if let Some(t) = self.get_terminal(term_id) {
            t.title.set(title.to_string());
        }
    }

    pub fn get_terminal(&self, term_id: &TermId) -> Option<TerminalData> {
        self.tab_info.with_untracked(|info| {
            for (_, tab) in &info.tabs {
                let terminal = tab.terminals.with_untracked(|terminals| {
                    terminals
                        .iter()
                        .find(|(_, t)| &t.term_id == term_id)
                        .cloned()
                });
                if let Some(terminal) = terminal {
                    return Some(terminal.1);
                }
            }
            None
        })
    }

    fn get_terminal_in_tab(
        &self,
        term_id: &TermId,
    ) -> Option<(usize, TerminalTabData, usize, TerminalData)> {
        self.tab_info.with_untracked(|info| {
            for (tab_index, (_, tab)) in info.tabs.iter().enumerate() {
                let result = tab.terminals.with_untracked(|terminals| {
                    terminals
                        .iter()
                        .enumerate()
                        .find(|(_, (_, t))| &t.term_id == term_id)
                        .map(|(i, (_, terminal))| (i, terminal.clone()))
                });
                if let Some((index, terminal)) = result {
                    return Some((tab_index, tab.clone(), index, terminal));
                }
            }
            None
        })
    }

    pub fn split(&self, term_id: TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(&term_id) {
            let terminal_data = TerminalData::new(
                tab.scope,
                self.workspace.clone(),
                None,
                self.common.clone(),
            );
            let i = terminal_data.scope.create_rw_signal(0);
            tab.terminals.update(|terminals| {
                terminals.insert(index + 1, (i, terminal_data));
            });
        }
    }

    pub fn split_next(&self, term_id: TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(&term_id) {
            let max = tab.terminals.with_untracked(|t| t.len() - 1);
            let new_index = (index + 1).min(max);
            if new_index != index {
                tab.active.set(new_index);
                self.update_debug_active_term();
            }
        }
    }

    pub fn split_previous(&self, term_id: TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(&term_id) {
            let new_index = index.saturating_sub(1);
            if new_index != index {
                tab.active.set(new_index);
                self.update_debug_active_term();
            }
        }
    }

    pub fn split_exchange(&self, term_id: TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(&term_id) {
            let max = tab.terminals.with_untracked(|t| t.len() - 1);
            if index < max {
                tab.terminals.update(|terminals| {
                    terminals.swap(index, index + 1);
                });
                self.update_debug_active_term();
            }
        }
    }

    pub fn close_terminal(&self, term_id: &TermId) {
        if let Some((_, tab, index, _)) = self.get_terminal_in_tab(term_id) {
            let active = tab.active.get_untracked();
            let len = tab
                .terminals
                .try_update(|terminals| {
                    terminals.remove(index);
                    terminals.len()
                })
                .unwrap();
            if len == 0 {
                self.close_tab(Some(tab.terminal_tab_id));
            } else {
                let new_active = active.min(len.saturating_sub(1));
                if new_active != active {
                    tab.active.set(new_active);
                    self.update_debug_active_term();
                }
            }
        }
    }

    pub fn terminal_stopped(&self, term_id: &TermId) {
        if let Some(terminal) = self.get_terminal(term_id) {
            if terminal.run_debug.with_untracked(|r| r.is_some()) {
                terminal.run_debug.update(|run_debug| {
                    if let Some(run_debug) = run_debug.as_mut() {
                        run_debug.stopped = true
                    }
                });
            } else {
                self.close_terminal(term_id);
            }
        }
    }

    pub fn get_stopped_run_debug_terminal(
        &self,
        mode: &RunDebugMode,
        config: &RunDebugConfig,
    ) -> Option<TerminalData> {
        self.tab_info.with_untracked(|info| {
            for (_, tab) in &info.tabs {
                let terminal = tab.terminals.with_untracked(|terminals| {
                    for (_, terminal) in terminals {
                        if let Some(run_debug) =
                            terminal.run_debug.get_untracked().as_ref()
                        {
                            if run_debug.stopped && &run_debug.mode == mode {
                                match run_debug.mode {
                                    RunDebugMode::Run => {
                                        if run_debug.config.name == config.name {
                                            return Some(terminal.clone());
                                        }
                                    }
                                    RunDebugMode::Debug => {
                                        if run_debug.config.dap_id == config.dap_id {
                                            return Some(terminal.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    None
                });
                if let Some(terminal) = terminal {
                    return Some(terminal);
                }
            }
            None
        })
    }

    pub fn restart_run_debug(&self, term_id: TermId) -> Option<()> {
        let (_, terminal_tab, index, terminal) =
            self.get_terminal_in_tab(&term_id)?;
        let run_debug = terminal.run_debug.get_untracked()?;
        let new_term_id = match run_debug.mode {
            RunDebugMode::Run => {
                self.common.proxy.terminal_close(term_id);

                let mut run_debug = run_debug;
                run_debug.stopped = false;
                let new_terminal = TerminalData::new(
                    terminal_tab.scope,
                    self.workspace.clone(),
                    Some(run_debug),
                    self.common.clone(),
                );
                let new_term_id = new_terminal.term_id;
                terminal_tab.terminals.update(|terminals| {
                    terminals[index] =
                        (new_terminal.scope.create_rw_signal(0), new_terminal);
                });
                self.debug.active_term.set(Some(new_term_id));
                new_term_id
            }
            RunDebugMode::Debug => {
                let dap_id =
                    terminal.run_debug.get_untracked().as_ref()?.config.dap_id;
                let daps = self.debug.daps.get_untracked();
                let dap = daps.get(&dap_id)?;
                self.common
                    .proxy
                    .dap_restart(dap.dap_id, self.debug.source_breakpoints());
                term_id
            }
        };

        self.focus_terminal(new_term_id);

        Some(())
    }

    pub fn focus_terminal(&self, term_id: TermId) {
        if let Some((tab_index, terminal_tab, index, _terminal)) =
            self.get_terminal_in_tab(&term_id)
        {
            self.tab_info.update(|info| {
                info.active = tab_index;
            });
            terminal_tab.active.set(index);
            self.common.focus.set(Focus::Panel(PanelKind::Terminal));

            self.update_debug_active_term();
        }
    }

    fn update_debug_active_term(&self) {
        let tab = self.active_tab(false);
        let terminal = tab.and_then(|tab| tab.active_terminal(false));
        if let Some(terminal) = terminal {
            let term_id = terminal.term_id;
            let is_run_debug =
                terminal.run_debug.with_untracked(|run| run.is_some());
            if is_run_debug {
                let current_active = self.debug.active_term.get_untracked();
                if current_active != Some(term_id) {
                    self.debug.active_term.set(Some(term_id));
                }
            }
        }
    }

    pub fn stop_run_debug(&self, term_id: TermId) -> Option<()> {
        let terminal = self.get_terminal(&term_id)?;
        let run_debug = terminal.run_debug.get_untracked()?;

        match run_debug.mode {
            RunDebugMode::Run => {
                self.common.proxy.terminal_close(term_id);
            }
            RunDebugMode::Debug => {
                let dap_id = run_debug.config.dap_id;
                let daps = self.debug.daps.get_untracked();
                let dap = daps.get(&dap_id)?;
                self.common.proxy.dap_stop(dap.dap_id);
            }
        }

        self.focus_terminal(term_id);
        Some(())
    }

    pub fn run_debug_process(
        &self,
        tracked: bool,
    ) -> Vec<(TermId, RunDebugProcess)> {
        let mut processes = Vec::new();
        if tracked {
            self.tab_info.with(|info| {
                for (_, tab) in &info.tabs {
                    tab.terminals.with(|terminals| {
                        for (_, terminal) in terminals {
                            if let Some(run_debug) = terminal.run_debug.get() {
                                processes.push((terminal.term_id, run_debug));
                            }
                        }
                    })
                }
            });
        } else {
            self.tab_info.with_untracked(|info| {
                for (_, tab) in &info.tabs {
                    tab.terminals.with_untracked(|terminals| {
                        for (_, terminal) in terminals {
                            if let Some(run_debug) = terminal.run_debug.get() {
                                processes.push((terminal.term_id, run_debug));
                            }
                        }
                    })
                }
            });
        }
        processes.sort_by_key(|(_, process)| process.created);
        processes
    }

    pub fn set_process_id(&self, term_id: &TermId, process_id: Option<u32>) {
        if let Some(terminal) = self.get_terminal(term_id) {
            terminal.run_debug.with_untracked(|run_debug| {
                if let Some(run_debug) = run_debug.as_ref() {
                    if run_debug.config.debug_command.is_some() {
                        let dap_id = run_debug.config.dap_id;
                        self.common
                            .proxy
                            .dap_process_id(dap_id, process_id, *term_id);
                    }
                }
            });
        }
    }

    pub fn dap_continued(&self, dap_id: &DapId) {
        let dap = self
            .debug
            .daps
            .with_untracked(|daps| daps.get(dap_id).cloned());
        if let Some(dap) = dap {
            dap.stopped.set(false);
        }
    }

    pub fn dap_stopped(
        &self,
        dap_id: &DapId,
        stopped: &Stopped,
        stack_frames: &HashMap<ThreadId, Vec<StackFrame>>,
    ) {
        let dap = self
            .debug
            .daps
            .with_untracked(|daps| daps.get(dap_id).cloned());
        if let Some(dap) = dap {
            dap.stopped(self.cx, stopped, stack_frames);
        }
    }

    pub fn dap_continue(&self, term_id: TermId) -> Option<()> {
        let terminal = self.get_terminal(&term_id)?;
        let dap_id = terminal
            .run_debug
            .with_untracked(|r| r.as_ref().map(|r| r.config.dap_id))?;
        let thread_id = self.debug.daps.with_untracked(|daps| {
            daps.get(&dap_id)
                .and_then(|dap| dap.thread_id.get_untracked())
        });
        let thread_id = thread_id.unwrap_or_default();
        self.common.proxy.dap_continue(dap_id, thread_id);
        Some(())
    }

    pub fn dap_pause(&self, term_id: TermId) -> Option<()> {
        let terminal = self.get_terminal(&term_id)?;
        let dap_id = terminal
            .run_debug
            .with_untracked(|r| r.as_ref().map(|r| r.config.dap_id))?;
        let thread_id = self.debug.daps.with_untracked(|daps| {
            daps.get(&dap_id)
                .and_then(|dap| dap.thread_id.get_untracked())
        });
        let thread_id = thread_id.unwrap_or_default();
        self.common.proxy.dap_pause(dap_id, thread_id);
        Some(())
    }

    pub fn get_active_dap(&self, tracked: bool) -> Option<DapData> {
        let active_term = if tracked {
            self.debug.active_term.get()?
        } else {
            self.debug.active_term.get_untracked()?
        };
        self.get_dap(active_term, tracked)
    }

    pub fn get_dap(&self, term_id: TermId, tracked: bool) -> Option<DapData> {
        let terminal = self.get_terminal(&term_id)?;
        let dap_id = if tracked {
            terminal
                .run_debug
                .with(|r| r.as_ref().map(|r| r.config.dap_id))?
        } else {
            terminal
                .run_debug
                .with_untracked(|r| r.as_ref().map(|r| r.config.dap_id))?
        };

        if tracked {
            self.debug.daps.with(|daps| daps.get(&dap_id).cloned())
        } else {
            self.debug
                .daps
                .with_untracked(|daps| daps.get(&dap_id).cloned())
        }
    }
}
