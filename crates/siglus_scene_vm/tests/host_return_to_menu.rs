//! Return to the menu after the script has ended, using generated public scenes only.
#![cfg(not(target_arch = "wasm32"))]

#[path = "support/scene_fixture.rs"]
mod scene_fixture;

use scene_fixture::{Code, gameexe, scene_chunk, scene_pack};
use siglus_scene_vm::{
    host::{SiglusHost, SiglusHostConfig},
    render::Renderer,
    runtime::{
        forms::codes::{ELM_GLOBAL_OWARI, ELM_GLOBAL_SYSCOM, syscom_op},
        globals::{SyscomPendingProc, SyscomPendingProcKind},
    },
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Project {
    base: PathBuf,
}

impl Project {
    fn new(start: Vec<u8>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "siglus-public-host-return-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let project = Self { base };
        fs::create_dir_all(project.game()).unwrap();
        let menu = Code::default().line(7).eof();
        fs::write(
            project.game().join("Scene.pck"),
            scene_pack(&[
                ("start", scene_chunk(&start, &[])),
                ("menu", scene_chunk(&menu, &[])),
            ]),
        )
        .unwrap();
        fs::write(project.game().join("Gameexe.dat"), gameexe(1280, "menu")).unwrap();
        project
    }

    fn game(&self) -> PathBuf {
        self.base.join("game")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Steps until the menu scene has run to its end and the proc stack is empty.
fn step_until_menu_ended(host: &mut SiglusHost) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        assert!(!host.step(0).unwrap(), "menu must not exit the session");
        if host.vm_mut().current_scene_name() == Some("menu") && host.vm_mut().is_halted() {
            break;
        }
        assert!(Instant::now() < deadline, "menu scene must end");
        thread::sleep(Duration::from_millis(5));
    }
    let vm = host.vm_mut();
    assert_eq!(vm.current_scene_name(), Some("menu"));
    assert_eq!(vm.current_line_no(), 7);
    assert!(vm.is_halted());
}

#[test]
#[ignore = "requires a GPU adapter"]
fn return_to_menu_queued_after_the_script_ended_restarts_the_menu() {
    let start = Code::default()
        .line(1)
        .command(&[ELM_GLOBAL_SYSCOM, syscom_op::RETURN_TO_MENU], &[0, 0, 1])
        .line(2)
        .command(&[ELM_GLOBAL_OWARI], &[])
        .eof();
    let project = Project::new(start);
    let renderer = pollster::block_on(Renderer::new_offscreen(1280, 720, 1.0)).unwrap();
    let mut config = SiglusHostConfig::new(project.game());
    config.width = Some(1280);
    config.height = Some(720);
    let mut host = pollster::block_on(SiglusHost::new_with_renderer(config, renderer)).unwrap();

    // The return command finishes the wipe without input, preempts OWARI,
    // and runs the menu scene to completion.
    step_until_menu_ended(&mut host);
    for _ in 0..8 {
        assert!(
            !host.step(0).unwrap(),
            "idle menu must keep the session alive"
        );
    }

    // A return to menu requested while idle, as a native menu button does.
    host.vm_mut().ctx.globals.syscom.pending_proc = Some(SyscomPendingProc {
        kind: SyscomPendingProcKind::ReturnToMenu,
        warning: false,
        se_play: false,
        fade_out: false,
        leave_msgbk: false,
        save_id: 0,
        save_tid: None,
        script_result: false,
    });
    // The host must restart the menu scene and then run it to its end again.
    let mut restarted = false;
    let finished = (0..240).any(|_| {
        assert!(
            !host.step(0).unwrap(),
            "return to menu is not an exit request"
        );
        restarted |= !host.vm_mut().is_halted();
        restarted && host.vm_mut().is_halted()
    });
    assert!(restarted, "idle host ignored the queued return to menu");
    assert!(finished, "restarted menu scene must end");
    assert_eq!(host.vm_mut().current_scene_name(), Some("menu"));
    assert_eq!(host.vm_mut().current_line_no(), 7);
}
