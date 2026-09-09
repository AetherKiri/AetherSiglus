//! Host continuation state shared by the embedded and standalone runners.
//! Saved at the script savepoint, not at the later save-menu file write.

use super::globals::{SyscomPendingProc, SyscomPendingProcKind};
use super::wait::VmWait;
use crate::original_save::{OriginalStreamReader, OriginalStreamWriter};
use anyhow::{bail, ensure, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ProcType {
    Script = 1,
    Disp = 2,
    GameEndWipe = 4,
    GameTimerStart = 5,
    ReturnToMenu = 8,
    MsgBack = 17,
    TimeWait = 20,
    EndGame = 44,
    StartWarning = 45,
    // Rust host-only state; deliberately outside the native proc domain.
    SyscomWarning = 0x100,
    Native = 0x101,
}

impl ProcType {
    fn read(value: i32) -> Result<Self> {
        Ok(match value {
            1 => Self::Script,
            2 => Self::Disp,
            4 => Self::GameEndWipe,
            5 => Self::GameTimerStart,
            8 => Self::ReturnToMenu,
            17 => Self::MsgBack,
            20 => Self::TimeWait,
            44 => Self::EndGame,
            45 => Self::StartWarning,
            0x100 => Self::SyscomWarning,
            0x101 => Self::Native,
            _ => bail!("unknown saved host proc {value}"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ProcFrame {
    pub ty: ProcType,
    pub option: i32,
    pub deadline_frame: Option<u32>,
    /// Original C_tnm_proc bytes, including its typed argument list. Keeping
    /// the record here preserves nested native continuations across a later
    /// Rust save without translating away fields the host does not own.
    pub native_record: Vec<u8>,
    pub native_started: bool,
}

#[derive(Debug, Default, Clone)]
pub struct ProcFlow {
    pub stack: Vec<ProcFrame>,
    pub booted_menu: bool,
    pub pending_syscom_proc: Option<SyscomPendingProc>,
}

impl ProcFlow {
    pub fn push(&mut self, ty: ProcType, option: i32) {
        self.stack.push(ProcFrame {
            ty,
            option,
            deadline_frame: None,
            native_record: Vec::new(),
            native_started: false,
        });
    }
    pub fn pop(&mut self) {
        self.stack.pop();
    }
    pub fn top(&self) -> Option<&ProcFrame> {
        self.stack.last()
    }
    pub fn top_mut(&mut self) -> Option<&mut ProcFrame> {
        self.stack.last_mut()
    }
}

#[derive(Debug, Default, Clone)]
pub struct HostFlowSnapshot {
    pub flow: ProcFlow,
    pub suspended_waits: Vec<(usize, VmWait, String)>,
    pub host_frame: u32,
}

impl HostFlowSnapshot {
    pub fn rebase_host_frame(&mut self, frame: u32) {
        for entry in &mut self.flow.stack {
            entry.deadline_frame = entry
                .deadline_frame
                .map(|deadline| frame.saturating_add(deadline.saturating_sub(self.host_frame)));
        }
        self.host_frame = frame;
    }

    pub(crate) fn write_extension(&self, w: &mut OriginalStreamWriter, frame: u64) {
        w.push_i32(i32::from_le_bytes(*b"SGPF"));
        // Version 3 adds the full seven-WORD backlog S_tid. Older saves still
        // decode with a zero/absent target and therefore cannot resume Proc 49.
        w.push_i32(3);
        w.push_bool(self.flow.booted_menu);
        w.push_i32(self.flow.stack.len() as i32);
        for entry in &self.flow.stack {
            w.push_i32(entry.ty as i32);
            w.push_i32(entry.option);
            w.push_bool(entry.deadline_frame.is_some());
            if let Some(deadline) = entry.deadline_frame {
                w.push_i32(deadline.saturating_sub(self.host_frame) as i32);
            }
            w.push_i32(entry.native_record.len() as i32);
            w.push_raw(&entry.native_record);
            w.push_bool(entry.native_started);
        }
        w.push_bool(self.flow.pending_syscom_proc.is_some());
        if let Some(p) = &self.flow.pending_syscom_proc {
            use SyscomPendingProcKind::*;
            w.push_i32(match p.kind {
                EndGame => 0,
                ReturnToSel => 1,
                ReturnToMenu => 2,
                Save => 3,
                Load => 4,
                QuickSave => 5,
                QuickLoad => 6,
                BacklogLoad => 7,
                MsgBack => 8,
                OpenSyscomMenu => 9,
                OpenSave => 10,
                OpenLoad => 11,
                OpenConfig => 12,
            });
            for flag in [p.warning, p.se_play, p.fade_out, p.leave_msgbk] {
                w.push_bool(flag);
            }
            w.push_i32(p.save_id as i32);
            w.push_i32((p.save_id >> 32) as i32);
            w.push_bool(p.save_tid.is_some());
            if let Some(tid) = p.save_tid {
                for word in tid {
                    w.push_i32(word as i32);
                }
            }
        }
        w.push_i32(self.suspended_waits.len() as i32);
        for (depth, wait, key) in &self.suspended_waits {
            w.push_i32(*depth as i32);
            w.push_str(key);
            wait.write_save_extension(w, frame);
        }
    }

    pub(crate) fn read_extension(
        rd: &mut OriginalStreamReader<'_>,
        frame: u64,
    ) -> Result<Option<Self>> {
        let mut probe = rd.clone();
        if probe.i32().ok() != Some(i32::from_le_bytes(*b"SGPF")) {
            return Ok(None);
        }
        let version = probe.i32()?;
        ensure!(
            matches!(version, 1 | 2 | 3),
            "unsupported host flow save version"
        );
        let mut state = Self::default();
        state.host_frame = frame as u32;
        state.flow.booted_menu = probe.bool()?;
        for _ in 0..read_count(&mut probe)? {
            let ty = ProcType::read(probe.i32()?)?;
            let option = probe.i32()?;
            let deadline_frame = if probe.bool()? {
                Some((frame as u32).saturating_add(probe.u32()?))
            } else {
                None
            };
            let (native_record, mut native_started) = if version >= 2 {
                let size = probe.i32()?;
                ensure!((0..=1_048_576).contains(&size), "invalid native proc size");
                (probe.take_raw(size as usize)?.to_vec(), probe.bool()?)
            } else {
                (Vec::new(), false)
            };
            ensure!(
                native_record.is_empty() || native_record.len() >= 147,
                "truncated native proc record"
            );
            ensure!(
                ty != ProcType::Native || !native_record.is_empty(),
                "missing native proc record"
            );
            if ty == ProcType::Native
                && matches!(
                    i32::from_le_bytes(native_record[..4].try_into().unwrap()),
                    22..=24
                )
            {
                // UI instances are reconstructed, not serialized. Rehydrate
                // these from the saved elapsed animation work on first poll.
                native_started = false;
            }
            state.flow.stack.push(ProcFrame {
                ty,
                option,
                deadline_frame,
                native_record,
                native_started,
            });
        }
        if probe.bool()? {
            use SyscomPendingProcKind::*;
            let kind = match probe.i32()? {
                0 => EndGame,
                1 => ReturnToSel,
                2 => ReturnToMenu,
                3 => Save,
                4 => Load,
                5 => QuickSave,
                6 => QuickLoad,
                7 => BacklogLoad,
                8 => MsgBack,
                9 => OpenSyscomMenu,
                10 => OpenSave,
                11 => OpenLoad,
                12 => OpenConfig,
                value => bail!("unknown pending host proc {value}"),
            };
            let warning = probe.bool()?;
            let se_play = probe.bool()?;
            let fade_out = probe.bool()?;
            let leave_msgbk = probe.bool()?;
            let low = probe.u32()? as u64;
            let high = probe.u32()? as u64;
            let save_tid = if version >= 3 && probe.bool()? {
                let mut tid = [0u16; 7];
                for word in &mut tid {
                    *word = probe.i32()?.clamp(0, u16::MAX as i32) as u16;
                }
                Some(tid)
            } else {
                None
            };
            state.flow.pending_syscom_proc = Some(SyscomPendingProc {
                kind,
                warning,
                se_play,
                fade_out,
                leave_msgbk,
                save_id: (low | (high << 32)) as i64,
                save_tid,
            });
        }
        for _ in 0..read_count(&mut probe)? {
            let depth = probe.i32()?;
            ensure!(
                depth > 0 && depth as usize <= state.flow.stack.len(),
                "invalid suspended proc depth"
            );
            let key = probe.string()?;
            let mut wait = VmWait::default();
            ensure!(
                wait.read_save_extension(&mut probe, frame)?,
                "missing suspended wait record"
            );
            state.suspended_waits.push((depth as usize, wait, key));
        }
        *rd = probe;
        Ok(Some(state))
    }
}

fn read_count(rd: &mut OriginalStreamReader<'_>) -> Result<usize> {
    let count = rd.i32()?;
    ensure!((0..=4096).contains(&count), "invalid saved proc count");
    Ok(count as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_flow_restores_options_deadlines_and_suspended_return_values() {
        let mut saved = HostFlowSnapshot::default();
        saved.host_frame = 100;
        saved.flow.booted_menu = true;
        saved.flow.push(ProcType::Script, 7);
        saved.flow.push(ProcType::TimeWait, 99);
        saved.flow.top_mut().unwrap().deadline_frame = Some(130);
        let mut wait = VmWait::default();
        wait.wait_key();
        wait.pending_value = Some(super::super::Value::Int(42));
        saved.suspended_waits.push((1, wait, "SAVE_SCENE".into()));
        saved.flow.pending_syscom_proc = Some(SyscomPendingProc {
            kind: SyscomPendingProcKind::ReturnToSel,
            warning: true,
            se_play: false,
            fade_out: true,
            leave_msgbk: true,
            save_id: 0x1234567812345678,
            save_tid: Some([2026, 9, 9, 12, 34, 56, 789]),
        });
        let mut w = OriginalStreamWriter::new();
        saved.write_extension(&mut w, 100);
        let bytes = w.into_inner();
        let mut loaded =
            HostFlowSnapshot::read_extension(&mut OriginalStreamReader::new(&bytes), 900)
                .unwrap()
                .unwrap();
        assert_eq!(loaded.flow.stack.len(), 2);
        assert_eq!(loaded.flow.stack[0].option, 7);
        assert_eq!(loaded.flow.top().unwrap().deadline_frame, Some(930));
        loaded.rebase_host_frame(2000);
        assert_eq!(loaded.flow.top().unwrap().deadline_frame, Some(2030));
        let pending = loaded.flow.pending_syscom_proc.as_ref().unwrap();
        assert_eq!(pending.save_id,
            0x1234567812345678);
        assert_eq!(pending.save_tid, Some([2026, 9, 9, 12, 34, 56, 789]));
        assert!(loaded.suspended_waits[0].1.waiting_for_key);
        assert_eq!(
            loaded.suspended_waits[0]
                .1
                .pending_value
                .as_ref()
                .unwrap()
                .as_i64(),
            Some(42)
        );
        assert_eq!(loaded.suspended_waits[0].2, "SAVE_SCENE");
    }
    #[test]
    fn absent_extension_leaves_native_reader_untouched() {
        let bytes = 123i32.to_le_bytes();
        let mut rd = OriginalStreamReader::new(&bytes);
        assert!(HostFlowSnapshot::read_extension(&mut rd, 0)
            .unwrap()
            .is_none());
        assert_eq!(rd.i32().unwrap(), 123);
    }
}
