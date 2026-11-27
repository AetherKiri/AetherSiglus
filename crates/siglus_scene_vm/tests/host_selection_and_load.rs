#![cfg(not(target_arch = "wasm32"))]

#[path = "support/scene_fixture.rs"]
mod scene_fixture;

use scene_fixture::{Code, gameexe_with_options, scene_chunk, scene_chunk_with_labels, scene_pack};
use siglus_scene_vm::{
    host::{SiglusHost, SiglusHostConfig},
    render::Renderer,
    runtime::{
        forms::codes::{
            ELM_GLOBAL_KEY, ELM_GLOBAL_SEL, ELM_GLOBAL_SET_SEL_MWND, ELM_GLOBAL_SET_TITLE,
            ELM_GLOBAL_SYSCOM, ELM_GLOBAL_TIMEWAIT, ELM_KEYLIST_WAIT, FM_INT, FM_STR, syscom_op,
        },
        input::{VmKey, VmMouseButton},
    },
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

struct Project(PathBuf);

impl Project {
    fn new(scenes: &[(&str, Vec<u8>)], options: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let project = Self(std::env::temp_dir().join(format!(
            "siglus-selection-and-load-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        fs::create_dir_all(&project.0).unwrap();
        fs::write(project.0.join("Scene.pck"), scene_pack(scenes)).unwrap();
        fs::write(
            project.0.join("Gameexe.dat"),
            gameexe_with_options(640, "", options),
        )
        .unwrap();
        project
    }

    fn open(&self) -> SiglusHost {
        let renderer = pollster::block_on(Renderer::new_offscreen(640, 720, 1.0)).unwrap();
        pollster::block_on(SiglusHost::new_with_renderer(
            SiglusHostConfig::new(self.0.clone()),
            renderer,
        ))
        .unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn step_until(host: &mut SiglusHost, done: impl Fn(&mut SiglusHost) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        assert!(!host.step(16).unwrap());
        if done(host) {
            return;
        }
        assert!(Instant::now() < deadline, "{}", host.debug_status_summary());
        thread::sleep(Duration::from_millis(4));
    }
}

fn press_enter(host: &mut SiglusHost) {
    host.key_down(VmKey::Enter);
    host.key_up(VmKey::Enter);
}

fn answer_warning(host: &mut SiglusHost, expected: &str, answer: i64) {
    step_until(host, |host| {
        host.vm_mut().ctx.globals.system.messagebox_modal.is_some()
    });
    let modal = host
        .vm_mut()
        .ctx
        .globals
        .system
        .messagebox_modal
        .as_ref()
        .unwrap();
    assert_eq!(modal.text, expected);
    let request_id = modal.request_id;
    host.submit_native_messagebox_result(request_id, answer);
}

#[test]
#[ignore = "requires a GPU adapter"]
fn end_load_runs_callback_at_configured_label_and_restores_the_caller_wait() {
    let start = Code::default()
        .line(10)
        .command(&[ELM_GLOBAL_KEY, ELM_KEYLIST_WAIT], &[])
        .line(11)
        .command(&[ELM_GLOBAL_SYSCOM, syscom_op::END_LOAD], &[1, 1, 1])
        .line(12)
        .command(&[ELM_GLOBAL_KEY, ELM_KEYLIST_WAIT], &[])
        .line(13)
        .command(&[ELM_GLOBAL_SYSCOM, syscom_op::END_LOAD], &[1, 1, 1])
        .line(14)
        .eof();

    let mut callback = Code::default();
    callback
        .line(20)
        .command_args(&[ELM_GLOBAL_SET_TITLE], &[(FM_STR, 0)], 0)
        .return_void();
    let label = callback.0.len();
    callback
        .line(21)
        .command_args(&[ELM_GLOBAL_SET_TITLE], &[(FM_STR, 1)], 0)
        .command(&[ELM_GLOBAL_TIMEWAIT], &[45])
        .line(22)
        .return_void();
    let callback = callback.eof();

    let project = Project::new(
        &[
            ("start", scene_chunk(&start, &[])),
            (
                "after",
                scene_chunk_with_labels(&callback, &["wrong label", "after load"], &[0, label]),
            ),
        ],
        concat!(
            "#LOAD_AFTER_CALL=\"after\",1\r\n",
            "#LOAD.WIPE=0,70\r\n",
            "#LOADINFO.WARNING_STR=\"Confirm end load\"\r\n",
        ),
    );
    let mut host = project.open();
    step_until(&mut host, |host| host.vm_mut().ctx.wait.waiting_for_key());
    host.vm_mut().build_local_save_snapshot_for_test();
    host.vm_mut().save_before_exit().unwrap();
    press_enter(&mut host);
    answer_warning(&mut host, "Confirm end load", 1);
    step_until(&mut host, |host| host.vm_mut().current_line_no() == 12);
    assert!(host.vm_mut().ctx.wait.waiting_for_key());
    press_enter(&mut host);
    answer_warning(&mut host, "Confirm end load", 0);

    step_until(&mut host, |host| {
        host.vm_mut().current_scene_name() == Some("after")
    });
    let vm = host.vm_mut();
    assert_eq!(vm.current_line_no(), 21);
    assert!(!vm.ctx.excall_state.ex_call_flag);
    assert_eq!(vm.ctx.globals.syscom.current_save_scene_title, "after load");

    step_until(&mut host, |host| {
        let vm = host.vm_mut();
        vm.current_scene_name() == Some("start") && vm.ctx.wait.waiting_for_key()
    });
    let vm = host.vm_mut();
    assert_eq!(vm.current_line_no(), 10);
    assert_eq!(vm.ctx.globals.syscom.current_save_scene_title, "after load");
}

#[test]
#[ignore = "requires a GPU adapter"]
fn message_window_choices_render_and_only_confirm_a_selectable_hit() {
    let mut choice = Code::default();
    choice
        .line(10)
        .command(&[ELM_GLOBAL_SET_SEL_MWND], &[0])
        .command_args(
            &[ELM_GLOBAL_SEL],
            &[(FM_STR, 0), (FM_STR, 1), (FM_INT, 2), (FM_STR, 2)],
            FM_INT,
        )
        .read_flag(0)
        .goto_if_zero(1)
        .line(90)
        .return_void();
    let first_choice = choice.0.len();
    let start = choice
        .line(11)
        .command(&[ELM_GLOBAL_KEY, ELM_KEYLIST_WAIT], &[])
        .eof();
    let project = Project::new(
        &[(
            "start",
            scene_chunk_with_labels(&start, &["Alpha", "Read", "Gamma"], &[0, first_choice]),
        )],
        concat!(
            "#MWND.CNT=1\r\n",
            "#MWND.000.WINDOW_POS=50,100\r\n",
            "#MWND.000.WINDOW_SIZE=300,200\r\n",
            "#MWND.000.MESSAGE_POS=8,14\r\n",
            "#MWND.000.MOJI_CNT=15,5\r\n",
            "#MWND.000.MOJI_SIZE=20\r\n",
            "#MWND.000.MOJI_SPACE=0,4\r\n",
        ),
    );
    let mut host = project.open();
    step_until(&mut host, |host| {
        !host.vm_mut().ctx.ui.mwnd.msg.glyphs.is_empty()
    });
    let drawn: String = host
        .vm_mut()
        .ctx
        .ui
        .mwnd
        .msg
        .glyphs
        .iter()
        .map(|glyph| glyph.ch)
        .collect();
    assert_eq!(drawn, "AlphaReadGamma");
    // Clicking blank space and a read-only item must leave the selector active.
    for (x, y) in [(620.0, 390.0), (65.0, 146.0)] {
        host.mouse_move(x, y);
        host.mouse_down(VmMouseButton::Left);
        host.mouse_up(VmMouseButton::Left);
        assert!(!host.step(16).unwrap());
        assert_eq!(host.vm_mut().current_line_no(), 10);
    }

    host.mouse_move(65.0, 121.0);
    host.mouse_down(VmMouseButton::Left);
    assert!(host.vm_mut().ctx.wait.waiting_for_key());
    host.mouse_up(VmMouseButton::Left);
    assert_eq!(host.vm_mut().ctx.globals.last_selection_message, "Alpha");
    step_until(&mut host, |host| host.vm_mut().current_line_no() == 11);
}

#[test]
#[ignore = "requires a GPU adapter"]
fn configured_shortcut_runs_excall_without_consuming_the_story_wait() {
    let start = Code::default()
        .line(10)
        .command(&[ELM_GLOBAL_KEY, ELM_KEYLIST_WAIT], &[])
        .line(11)
        .eof();
    let shortcut = Code::default()
        .line(20)
        .command_args(&[ELM_GLOBAL_SET_TITLE], &[(FM_STR, 0)], 0)
        .return_void()
        .eof();
    let project = Project::new(
        &[
            ("start", scene_chunk(&start, &[])),
            ("shortcut", scene_chunk(&shortcut, &["shortcut ran"])),
        ],
        concat!(
            "#SHORTCUT.CNT=1\r\n",
            "#SHORTCUT.000.KEY=112\r\n",
            "#SHORTCUT.000.COND=0\r\n",
            "#SHORTCUT.000.SCENE=\"shortcut\",0\r\n",
        ),
    );
    let mut host = project.open();
    step_until(&mut host, |host| host.vm_mut().ctx.wait.waiting_for_key());
    host.key_down(VmKey::F(1));
    host.key_up(VmKey::F(1));
    step_until(&mut host, |host| {
        let vm = host.vm_mut();
        vm.current_scene_name() == Some("start")
            && vm.ctx.globals.syscom.current_save_scene_title == "shortcut ran"
    });
    let vm = host.vm_mut();
    assert_eq!(vm.current_line_no(), 10);
    assert!(vm.ctx.wait.waiting_for_key());
    assert!(!vm.ctx.excall_state.ex_call_flag);
}

#[test]
#[ignore = "requires a GPU adapter"]
fn configured_flick_runs_excall_without_choosing_the_message_under_it() {
    let start = Code::default()
        .line(10)
        .command(&[ELM_GLOBAL_KEY, ELM_KEYLIST_WAIT], &[])
        .line(11)
        .command(&[ELM_GLOBAL_KEY, ELM_KEYLIST_WAIT], &[])
        .eof();
    let gesture = Code::default()
        .line(20)
        .command_args(&[ELM_GLOBAL_SET_TITLE], &[(FM_STR, 0)], 0)
        .return_void()
        .eof();
    let project = Project::new(
        &[
            ("start", scene_chunk(&start, &[])),
            ("gesture", scene_chunk(&gesture, &["gesture ran"])),
        ],
        concat!(
            "#FLICK_SCENE.CNT=1\r\n",
            "#FLICK_SCENE.000.ANGLE=5\r\n",
            "#FLICK_SCENE.000.SCENE=\"gesture\",0\r\n",
        ),
    );
    let mut host = project.open();
    step_until(&mut host, |host| host.vm_mut().ctx.wait.waiting_for_key());
    host.mouse_move(200.0, 400.0);
    host.mouse_down(VmMouseButton::Left);
    assert!(!host.step(16).unwrap());
    assert_eq!(host.vm_mut().current_line_no(), 10);
    host.mouse_move(200.0, 310.0);
    host.mouse_up(VmMouseButton::Left);
    step_until(&mut host, |host| {
        let vm = host.vm_mut();
        vm.current_scene_name() == Some("start")
            && vm.ctx.globals.syscom.current_save_scene_title == "gesture ran"
    });
    assert_eq!(host.vm_mut().current_line_no(), 10);
    assert!(host.vm_mut().ctx.wait.waiting_for_key());

    host.mouse_down(VmMouseButton::Left);
    host.mouse_up(VmMouseButton::Left);
    step_until(&mut host, |host| host.vm_mut().current_line_no() == 11);
}

#[test]
#[ignore = "requires a GPU adapter"]
fn save_overwrite_returns_the_confirmation_result_to_the_script() {
    let save = [ELM_GLOBAL_SYSCOM, syscom_op::SAVE];
    let key_wait = [ELM_GLOBAL_KEY, ELM_KEYLIST_WAIT];
    let mut code = Code::default();
    code.line(10)
        .command(&key_wait, &[])
        .line(11)
        .command_args(&save, &[(FM_INT, 0), (FM_INT, 1), (FM_INT, 0)], FM_INT)
        .goto_if_zero(1)
        .line(12)
        .command(&key_wait, &[])
        .line(13)
        .command_args(&save, &[(FM_INT, 0), (FM_INT, 1), (FM_INT, 0)], FM_INT)
        .goto_if_zero(2)
        .line(14)
        .return_void();
    let unexpected_failure = code.0.len();
    code.line(90).command(&key_wait, &[]).return_void();
    let declined = code.0.len();
    code.line(20).command(&key_wait, &[]).return_void();
    let code = code.eof();
    let project = Project::new(
        &[(
            "start",
            scene_chunk_with_labels(&code, &[], &[0, unexpected_failure, declined]),
        )],
        "#SAVEINFO.WARNING_STR=\"Confirm overwrite\"\r\n",
    );
    let mut host = project.open();
    step_until(&mut host, |host| host.vm_mut().ctx.wait.waiting_for_key());
    host.vm_mut().build_local_save_snapshot_for_test();
    press_enter(&mut host);
    step_until(&mut host, |host| host.vm_mut().current_line_no() == 12);
    assert!(host.vm_mut().ctx.globals.system.messagebox_modal.is_none());

    press_enter(&mut host);
    answer_warning(&mut host, "Confirm overwrite", 1);
    step_until(&mut host, |host| host.vm_mut().current_line_no() == 20);
    assert!(host.vm_mut().ctx.wait.waiting_for_key());
}
