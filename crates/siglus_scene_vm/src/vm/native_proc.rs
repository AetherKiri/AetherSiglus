//! Original C_tnm_proc continuation import. The native stack is bottom-first,
//! with cur_proc stored separately, so it must not be flattened into one wait.
use super::*;
use crate::original_save::OriginalStreamReader;
use runtime::flow::{HostFlowSnapshot, ProcType};
use runtime::wait::AudioWait;

struct Record {
    kind: i32,
    element: Vec<i32>,
    al_id: i32,
    args: Vec<UserPropCell>,
    key: bool,
    skip_disabled: bool,
    returns: bool,
    option: i32,
}

fn restorable_native_kind(bytes: &[u8]) -> Result<i32> {
    // The empty typed-argument record still contains the fixed S_element.
    // Check this before taking the option at the end of the record.
    anyhow::ensure!(bytes.len() >= 147, "truncated native proc record");
    let kind = i32::from_le_bytes(bytes[..4].try_into().unwrap());
    anyhow::ensure!(
        (0..=51).contains(&kind) && !(10..=13).contains(&kind),
        "invalid native proc type {kind}"
    );
    Ok(kind)
}

impl SceneVm<'_> {
    /// Inspect continuations before finish_local tears down the live scene.
    /// Proc 49 is accepted here like the native engine; its exact seven-WORD
    /// target is checked only when the continuation is actually polled.
    pub(super) fn preflight_original_local_procs(&self, bytes: &[u8]) -> Result<()> {
        let mut rd = OriginalStreamReader::new(bytes);
        let _scene = rd.string()?;
        let _line = rd.i32()?;
        let _pc = rd.i32()?;
        restorable_native_kind(&self.read_cpp_proc_record(&mut rd)?)?;
        let count = rd.i32()?;
        anyhow::ensure!((0..=4096).contains(&count), "invalid native proc stack size");
        for _ in 0..count {
            restorable_native_kind(&self.read_cpp_proc_record(&mut rd)?)?;
        }
        Ok(())
    }

    pub(crate) fn report_unavailable_load(&mut self, reason: &str) {
        log::warn!("[SG_SAVELOAD] load cancelled before replacing the current scene: {reason}");
        self.ctx.request_system_messagebox_no_return(
            17, false,
            format!("This save cannot be resumed safely.\n{reason}\nThe current game state has been kept."),
            vec![runtime::globals::SystemMessageBoxButton { label: "OK".into(), value: 0 }],
        );
    }

    fn decode_native_proc(&self, bytes: &[u8]) -> Result<Record> {
        let mut rd = OriginalStreamReader::new(bytes);
        let kind = rd.i32()?;
        let element = rd.element()?;
        let al_id = rd.i32()?;
        let args = rd.extend_items(|rd| self.read_cpp_prop(rd).map(|(_, value)| value))?;
        let result = Record {
            kind,
            element,
            al_id,
            args,
            key: rd.bool()?,
            skip_disabled: rd.bool()?,
            returns: rd.bool()?,
            option: rd.i32()?,
        };
        anyhow::ensure!(rd.remaining().is_empty(), "trailing native proc data");
        Ok(result)
    }

    pub(super) fn native_proc_flow(records: Vec<Vec<u8>>) -> Result<Option<HostFlowSnapshot>> {
        let mut snapshot = HostFlowSnapshot::default();
        snapshot.flow.booted_menu = true;
        for record in records {
            let kind = restorable_native_kind(&record)?;
            if kind == 0 {
                continue;
            } // early Rust used NONE as a placeholder
            let ty = match kind {
                1 => ProcType::Script,
                2 | 46 => ProcType::Disp,
                4 => ProcType::GameEndWipe,
                5 => ProcType::GameTimerStart,
                8 => ProcType::ReturnToMenu,
                17 => ProcType::MsgBack,
                44 => ProcType::EndGame,
                45 => ProcType::StartWarning,
                _ => ProcType::Native,
            };
            let option = i32::from_le_bytes(record[record.len() - 4..].try_into().unwrap());
            snapshot.flow.push(ty, option);
            snapshot.flow.top_mut().unwrap().native_record = record;
        }
        Ok((!snapshot.flow.stack.is_empty()).then_some(snapshot))
    }

    fn native_wait_dispatch(
        &mut self,
        mut element: Vec<i32>,
        suffix: &[i32],
        args: &[Value],
    ) -> Result<()> {
        element.extend_from_slice(suffix);
        let Some(&form) = element.first() else {
            bail!("native wait has no target element");
        };
        let saved = self.ctx.vm_call.replace(runtime::VmCallMeta {
            element,
            al_id: 0,
            ret_form: self.cfg.fm_void as i64,
        });
        let stack_size = self.ctx.stack.len();
        let form = self.canonical_runtime_form_id(form as u32);
        let dispatched = runtime::forms::dispatch_form(&mut self.ctx, form, args);
        self.ctx.vm_call = saved;
        // A wait's return is produced on completion, never by starting the
        // synthetic WAIT command. Preserve unrelated existing stack values.
        self.ctx.stack.truncate(stack_size);
        anyhow::ensure!(dispatched?, "native wait target could not be resolved");
        Ok(())
    }

    fn native_proc_arg(&self, cell: &UserPropCell) -> Value {
        match cell.form {
            form if form == self.cfg.fm_int => Value::Int(cell.int_value as i64),
            form if form == self.cfg.fm_str => Value::Str(cell.str_value.clone()),
            form if form == self.cfg.fm_intlist => Value::List(
                cell.int_list
                    .iter()
                    .map(|v| Value::Int(*v as i64))
                    .collect(),
            ),
            form if form == self.cfg.fm_strlist => Value::List(
                cell.str_list
                    .iter()
                    .map(|v| Value::Str(v.clone()))
                    .collect(),
            ),
            codes::FM_LIST => Value::List(
                cell.list_items
                    .iter()
                    .map(|v| self.native_proc_arg(v))
                    .collect(),
            ),
            _ => Value::Element(cell.element.clone()),
        }
    }

    fn native_stage_child(&self, element: &[i32], child: i32) -> Option<(u32, i64, usize)> {
        let head = *element.first()?;
        let form = runtime::forms::stage::stage_storage_form_id(&self.ctx, head);
        let stage = match head {
            codes::ELM_GLOBAL_BACK => 0,
            codes::ELM_GLOBAL_FRONT => 1,
            codes::ELM_GLOBAL_NEXT => 2,
            _ if runtime::forms::stage::is_stage_form_id(&self.ctx, head)
                && element.get(1) == Some(&codes::ELM_ARRAY) =>
            {
                *element.get(2)? as i64
            }
            _ => return None,
        };
        let tail = element
            .windows(3)
            .find(|w| w[0] == child && w[1] == codes::ELM_ARRAY)?;
        Some((form, stage, usize::try_from(tail[2]).ok()?))
    }

    fn poll_native_mwnd(&mut self, proc: &Record, started: bool, skipping: bool) -> bool {
        let opening = proc.kind == 22;
        let targets: Vec<_> = if proc.kind == 24 {
            let form = runtime::forms::stage::current_stage_form_id(&self.ctx);
            self.ctx
                .globals
                .stage_forms
                .get(&form)
                .into_iter()
                .flat_map(|st| {
                    st.mwnd_lists.iter().flat_map(move |(stage, list)| {
                        (0..list.len()).map(move |index| (form, *stage, index))
                    })
                })
                .collect()
        } else {
            self.native_stage_child(&proc.element, codes::ELM_STAGE_MWND)
                .into_iter()
                .collect()
        };
        if !started {
            self.ctx.sync_mwnd_window_ui();
        }
        let mut active = false;
        for key @ (form, stage, index) in targets {
            let Some(mwnd) = self
                .ctx
                .globals
                .stage_forms
                .get_mut(&form)
                .and_then(|st| st.mwnd_lists.get_mut(&stage))
                .and_then(|list| list.get_mut(index))
            else {
                continue;
            };
            let work = if opening {
                &mut mwnd.saved_open_anime
            } else {
                &mut mwnd.saved_close_anime
            };
            if !started {
                let Some((ty, duration, start)) = *work else {
                    continue;
                };
                if ty < 0 {
                    continue;
                }
                let elapsed = (mwnd.time as i32).wrapping_sub(start as i32).max(0) as i64;
                self.ctx
                    .ui
                    .restore_native_mwnd_animation(key, opening, ty, duration, elapsed);
            }
            if self
                .ctx
                .ui
                .poll_native_mwnd_animation(key, opening, skipping)
            {
                active = true;
            } else {
                *work = Some((-1, 0, 0));
            }
        }
        !active
    }

    fn restore_native_selection(&mut self) -> Result<()> {
        let saved = self
            .ctx
            .globals
            .syscom
            .sel_save_ids
            .last()
            .and_then(|id| {
                self.ctx
                    .local_save_snapshot
                    .as_ref()?
                    .sel_saves
                    .iter()
                    .find(|save| &save.save_id == id)
            })
            .cloned();
        let Some(saved) = saved else {
            anyhow::ensure!(
                self.restore_last_sel_point(),
                "native RETURN_TO_SEL has no matching selection snapshot"
            );
            self.ctx.host_flow_snapshot = None;
            self.ctx.mark_runtime_load_completed();
            return Ok(());
        };
        if let Err(error) = self.preflight_original_local_procs(&saved.local_stream) {
            self.report_unavailable_load(&error.to_string());
            return Ok(());
        }
        self.scene_stack.clear();
        self.scene_user_props.clear();
        self.sel_point_stack.clear();
        self.save_point = None;
        self.ctx.local_save_snapshot = None;
        self.ctx.begin_runtime_load_apply();
        self.ctx.globals.append_dir = saved.append_dir.clone();
        self.ctx.globals.append_name = saved.append_name.clone();
        self.ctx
            .images
            .set_current_append_dir(saved.append_dir.clone());
        self.ctx
            .movie
            .set_current_append_dir(saved.append_dir.clone());
        self.ctx
            .bgm
            .set_current_append_dir(saved.append_dir.clone());
        let snapshot = self.parse_original_local_stream(&saved.local_stream)?;
        self.parse_original_local_ex_stream(&saved.local_ex_stream)?;
        self.ctx.local_save_snapshot = Some(runtime::LocalSaveSnapshot {
            save_id: saved.save_id,
            append_dir: saved.append_dir,
            append_name: saved.append_name,
            save_scene_title: saved.title,
            save_msg: String::new(),
            save_full_msg: self.ctx.globals.syscom.current_save_full_message.clone(),
            local_stream: saved.local_stream,
            local_ex_stream: saved.local_ex_stream,
            sel_saves: saved.sel_saves,
        });
        self.activate_original_runtime_snapshot(snapshot)
    }

    /// Returns true only when the original continuation has completed. Return
    /// values go straight to the original lexer stack, not a newly executed
    /// Rust command's delayed-return slot.
    pub fn poll_native_proc(&mut self, bytes: &[u8], started: bool) -> Result<bool> {
        let proc = self.decode_native_proc(bytes)?;
        let skipping = !proc.skip_disabled && self.ctx.runtime_is_skipping();
        match proc.kind {
            9 => { self.restore_native_selection()?; return Ok(true); }
            14..=16 => {
                let stage_form = runtime::forms::stage::current_stage_form_id(&self.ctx);
                let mwnd = self.ctx.globals.current_sel_mwnd_no
                    .map(|index| {
                    (stage_form, self.ctx.globals.current_sel_mwnd_stage_idx, index,
                    )
                });
                let group = self.native_stage_child(&proc.element, codes::ELM_STAGE_OBJBTNGROUP);
                let active = match proc.kind {
                    14 => mwnd.and_then(|(form, stage, index)| {
                            self.ctx.globals.stage_forms.get(&form)
                        .and_then(|st| st.mwnd_lists.get(&stage)).and_then(|list| list.get(index))
                        })
                        .is_some_and(|m| m.selection.is_some()),
                    15 => self.ctx.globals.selbtn.processing_flag_0,
                    16 => group.and_then(|(form, stage, index)| {
                            self.ctx.globals.stage_forms.get(&form)
                        .and_then(|st| st.group_lists.get(&stage)).and_then(|list| list.get(index))
                        })
                        .is_some_and(|g| g.is_doing() && g.wait_flag),
                    _ => false,
                };
                if !started {
                    self.ctx.wait.clear();
                    self.ctx.wait.configure_native_return(true, false);
                    if active { self.ctx.wait.wait_system_modal(); }
                    if proc.kind == 14 { self.ctx.globals.focused_stage_mwnd = mwnd; }
                    if proc.kind == 16 { self.ctx.globals.focused_stage_group = group; }
                }
                if active || self.ctx.wait.system_modal { return Ok(false); }
                if let Some(value) = self.ctx.wait.take_native_result().and_then(|v| v.as_i64()) {
                    self.int_stack.push(value as i32);
                }
                return Ok(true);
            }
            6 | 7 | 42 | 47 if !started => {
                let kind = match proc.kind { 6 => RuntimeSaveKind::Normal, 7 => RuntimeSaveKind::Quick,
                    42 => RuntimeSaveKind::End, _ => RuntimeSaveKind::Inner,
                };
                let index = usize::try_from(proc.option).map_err(|_| anyhow!("invalid native load index"))?;
                self.perform_runtime_load_request(RuntimeLoadRequest { kind, index })?;
                return Ok(true);
            }
            43 if !started => {
                // The preceding CAPTURE_ONLY already captured the thumbnail.
                self.perform_runtime_save_request(RuntimeSaveRequest { kind: RuntimeSaveKind::End, index: 0,
                })?;
                return Ok(true);
            }
            41 if !started => {
                // SAVE_DIALOG is phase two: do not capture again or replace
                // the thumbnail made by the preceding CAPTURE_ONLY frame.
                self.ctx.globals.syscom.pending_proc = Some(runtime::globals::SyscomPendingProc {
                    kind: runtime::globals::SyscomPendingProcKind::OpenSave,
                    warning: false, se_play: false, fade_out: false, leave_msgbk: false, save_id: 0,
                    save_tid: None,
                });
                return Ok(true);
            }
            50 if !started => {
                self.dispatch_syscom_button_op(codes::syscom_op::OPEN_TWEET_DIALOG, &[])?;
                return Ok(true);
            }
            49 => {
                if started {
                    return Ok(true);
                }
                let Some(tid) = self
                    .ctx
                    .globals
                    .syscom
                    .pending_proc
                    .as_ref()
                    .and_then(|p| p.save_tid)
                    .or_else(|| {
                        let tid = self.ctx.globals.syscom.msg_back_load_tid;
                        (tid != [0; 7]).then_some(tid)
                    })
                else {
                    bail!("native BACKLOG_LOAD (Proc 49) has no in-memory S_tid target; refusing to substitute a selection point")
                };
                anyhow::ensure!(
                    self.restore_backlog_snapshot(tid)?,
                    "native BACKLOG_LOAD target {:?} is not available in the current backlog map",
                    tid
                );
                return Ok(true);
            }
            48 => {
                if !started { return Ok(false); } // original EASY_LOAD yields one frame
                self.call_syscom_configured_scene("LOAD_AFTER_CALL")?;
                return Ok(true);
            }
            3 => {
                if !started {
                    self.ctx.finish_wipe_runtime();
                    let params = self.ctx.tables.gameexe.as_ref().and_then(|c| c.get_value("LOAD.WIPE"))
                        .map(|v| {
                            v.split(|c: char| !(c == '-' || c.is_ascii_digit()))
                            .filter_map(|v| v.parse::<i32>().ok()).collect::<Vec<_>>()
                        }).unwrap_or_default();
                    let form = runtime::forms::stage::current_stage_form_id(&self.ctx);
                    // A loaded FRONT is the new scene; NEXT is the black old
                    // scene. Unlike a script WIPE, don't copy FRONT to NEXT.
                    runtime::forms::stage::reinit_wipe_next_stage(&mut self.ctx, form);
                    self.ctx.globals.start_wipe(runtime::globals::WipeState::new(form, None, None,
                        params.first().copied().unwrap_or(0), params.get(1).copied().unwrap_or(1000),
                        0, 0, vec![], i32::MIN, i32::MAX, i32::MIN, i32::MAX, true, 0, 0,
                        ));
                }
                return Ok(self.ctx.globals.wipe_done());
            }
            22..=24 => return Ok(self.poll_native_mwnd(&proc, started, skipping)),
            36 => {
                let skip = skipping || (proc.key && self.ctx.input.take_native_decide_cancel(false).is_some());
                let form = if self.ctx.ids.form_global_screen != 0 { self.ctx.ids.form_global_screen }
                    else { runtime::constants::global_form::SCREEN };
                let Some(screen) = self.ctx.globals.screen_forms.get_mut(&form) else { return Ok(true); };
                if skip { screen.shake.end(); }
                return Ok(!screen.shake.is_active());
            }
            18 => {
                if !started {
                    let mut args = proc.args.iter().map(|v| self.native_proc_arg(v)).collect();
                    // eng_frame.cpp always dispatches COMMAND with FM_VOID,
                    // irrespective of the stale return flag in the record.
                    self.exec_command(proc.element, proc.al_id, self.cfg.fm_void, &mut args)?;
                    self.drain_runtime_save_load_requests()?;
                }
                return Ok(!self.ctx.wait_poll());
            }
            19 | 20 | 21 => {
                let timed_out = match proc.kind {
                    19 => false,
                    20 => (self.ctx.globals.local_game_time as i32).wrapping_sub(proc.option) >= 0,
                    21 => proc.element.first().and_then(|head| self.ctx.globals.counter_lists.get(&(*head as u32)))
                        .and_then(|list| {
                            proc.element.get(2).and_then(|index| usize::try_from(*index).ok()).and_then(|index| list.get(index))
                        })
                        .map(|counter| (counter.get_count() as i32).wrapping_sub(proc.option) >= 0).unwrap_or(true),
                    _ => false,
                };
                let answer = if timed_out || skipping { Some(0) }
                    else if proc.kind == 19 || proc.key {
                        self.ctx.input.take_native_decide_cancel(proc.kind == 20)
                    } else { None };
                if let Some(answer) = answer {
                    if proc.kind != 19 && proc.key { self.int_stack.push(answer as i32); }
                    return Ok(true);
                }
                return Ok(false);
            }
            _ => {}
        }

        if !started {
            self.ctx.wait.clear();
            let key_op = |normal, key| if proc.key { key } else { normal };
            match proc.kind {
                25 => {
                    self.ctx.ui.begin_message_reveal_wait();
                    self.ctx.wait.wait_message_reveal();
                }
                26 => {
                    self.ctx.ui.begin_wait_message();
                    self.ctx.wait.wait_key();
                }
                27 => self
                    .ctx
                    .wait
                    .wait_audio_with_return(AudioWait::Bgm, false, proc.returns),
                28 => self
                    .ctx
                    .wait
                    .wait_audio_with_return(AudioWait::BgmFade, false, proc.returns),
                29 => self
                    .ctx
                    .wait
                    .wait_audio_with_return(AudioWait::KoeAny, false, proc.returns),
                30 => self
                    .ctx
                    .wait
                    .wait_audio_with_return(AudioWait::PcmAny, false, proc.returns),
                31 | 32 => {
                    let index = proc.element.get(2).copied().unwrap_or(-1);
                    if !(0..=255).contains(&index) {
                        if proc.returns {
                            self.int_stack.push(0);
                        }
                        return Ok(true);
                    }
                    let wait = if proc.kind == 31 {
                        AudioWait::PcmSlot(index as u8)
                    } else {
                        AudioWait::PcmSlotFade(index as u8)
                    };
                    self.ctx
                        .wait
                        .wait_audio_with_return(wait, false, proc.returns);
                }
                33 => {
                    let Some(&form) = proc.element.first() else {
                        bail!("missing PCM event target");
                    };
                    let index = proc.element.get(2).copied().unwrap_or(-1);
                    if index < 0 {
                        if proc.returns {
                            self.int_stack.push(0);
                        }
                        return Ok(true);
                    }
                    self.ctx.wait.wait_pcm_event(
                        form as u32,
                        index as usize,
                        proc.key,
                        proc.returns,
                    );
                }
                34 => self.ctx.wait.wait_global_movie(proc.key, proc.key),
                35 => self.ctx.wait.wait_wipe(proc.key),
                37 => self.native_wait_dispatch(
                    proc.element.clone(),
                    &[key_op(codes::ELM_QUAKE_WAIT, codes::ELM_QUAKE_WAIT_KEY)],
                    &[],
                )?,
                38 => self.native_wait_dispatch(
                    proc.element.clone(),
                    &[key_op(
                        codes::ELM_INTEVENT_WAIT,
                        codes::ELM_INTEVENT_WAIT_KEY,
                    )],
                    &[],
                )?,
                39 => self.native_wait_dispatch(
                    proc.element.clone(),
                    &[codes::ELM_OBJECT_ALL_EVE, codes::ELM_ALLEVENT_WAIT],
                    &[Value::Int(proc.key as i64)],
                )?,
                40 => self.native_wait_dispatch(
                    proc.element.clone(),
                    &[key_op(
                        codes::ELM_OBJECT_WAIT_MOVIE,
                        codes::ELM_OBJECT_WAIT_MOVIE_KEY,
                    )],
                    &[],
                )?,
                51 => self.native_wait_dispatch(
                    proc.element.clone(),
                    &[key_op(
                        codes::ELM_OBJECT_EMOTE_WAIT_PLAYING,
                        codes::ELM_OBJECT_EMOTE_WAIT_PLAYING_KEY,
                    )],
                    &[],
                )?,
                kind => bail!("native Proc {kind} cannot yet be resumed safely; record preserved"),
            }
            if proc.kind != 26 {
                self.ctx.wait.configure_native_return(
                    proc.returns || (proc.kind == 34 && proc.key),
                    proc.key,
                );
            }
        }

        if skipping && matches!(proc.kind, 27..=33 | 35 | 37) {
            if proc.kind == 35 {
                self.ctx.finish_wipe_runtime();
            }
            if proc.kind == 37 {
                self.ctx.wait.finish_native_quake(&mut self.ctx.globals);
            }
            self.ctx.wait.clear();
            if proc.returns {
                self.int_stack.push(0);
            }
            return Ok(true);
        }
        if proc.key && matches!(proc.kind, 27..=32) {
            if self.ctx.input.take_native_decide_cancel(false).is_some() {
                self.ctx.wait.clear();
                if proc.returns {
                    self.int_stack.push(1);
                }
                return Ok(true);
            }
        }
        if self.ctx.wait_poll() {
            return Ok(false);
        }
        if let Some(value) = self.ctx.wait.take_native_result() {
            if (proc.returns || (proc.kind == 34 && proc.key)) && value.as_i64().is_some() {
                self.int_stack.push(value.as_i64().unwrap() as i32);
            }
        } else if proc.returns {
            self.int_stack.push(0);
        }
        Ok(true)
    }
}
