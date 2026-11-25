//! Game-initiated exit through the embedded host, using generated public scenes only.
#![cfg(not(target_arch = "wasm32"))]

#[path = "support/scene_fixture.rs"]
mod scene_fixture;

use scene_fixture::{Code, gameexe, scene_chunk, scene_pack};
use siglus_scene_vm::{
    host::{SiglusHost, SiglusHostConfig},
    render::Renderer,
    runtime::{
        forms::codes::{ELM_GLOBAL_OWARI, ELM_GLOBAL_SYSCOM, syscom_op},
        native_ui::{NativeMessageBoxRequest, NativeUiBackend},
    },
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};


const END_GAME: [i32; 2] = [ELM_GLOBAL_SYSCOM, syscom_op::END_GAME];

struct Project {
    base: PathBuf,
}

impl Project {
    fn new(start: Vec<u8>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "siglus-public-host-exit-{}-{}",
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

    fn host(&self) -> SiglusHost {
        let renderer = pollster::block_on(Renderer::new_offscreen(1280, 720, 1.0)).unwrap();
        let mut config = SiglusHostConfig::new(self.game());
        config.width = Some(1280);
        config.height = Some(720);
        pollster::block_on(SiglusHost::new_with_renderer(config, renderer)).unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[derive(Default)]
struct Prompts(Mutex<Vec<NativeMessageBoxRequest>>);

impl NativeUiBackend for Prompts {
    fn show_system_messagebox(&self, request: NativeMessageBoxRequest) {
        self.0.lock().unwrap().push(request);
    }
}

/// Line 1 asks `END_GAME(warning)` and is declined, line 2 ends with `OWARI`.
/// Line 3 would run if the exit fell back into the suspended script proc.
///
/// One host per process: creating a second GPU instance in the same process
/// crashes some drivers, so the scenario covers both answers in one run.
#[test]
#[ignore = "requires a GPU adapter"]
fn game_initiated_exit_finishes_the_host_without_resuming_the_script() {
    let start = Code::default()
        .line(1)
        .command(&END_GAME, &[1, 0, 0])
        .line(2)
        .command(&[ELM_GLOBAL_OWARI], &[])
        .line(3)
        .eof();
    let project = Project::new(start);
    let mut host = project.host();
    let prompts = Arc::new(Prompts::default());
    host.vm_mut()
        .ctx
        .set_native_ui_backend(Some(prompts.clone() as Arc<dyn NativeUiBackend>));

    let mut request = None;
    for _ in 0..240 {
        assert!(!host.step(0).unwrap(), "the END_GAME warning must wait");
        request = prompts.0.lock().unwrap().pop();
        if request.is_some() {
            break;
        }
    }
    let request = request.expect("END_GAME(warning) asks the host");
    for _ in 0..8 {
        assert!(!host.step(0).unwrap(), "no answer means no exit");
    }
    assert_eq!(
        host.vm_mut().current_line_no(),
        1,
        "the warning stops the script"
    );

    // Declining resumes the script, which then ends the game with OWARI.
    const DECLINE: i64 = 1;
    assert!(request.buttons.iter().any(|b| b.value == DECLINE));
    host.submit_native_messagebox_result(request.request_id, DECLINE);
    assert!(
        (0..240).any(|_| host.step(0).unwrap()),
        "OWARI must finish the host"
    );
    assert_eq!(host.vm_mut().current_scene_name(), Some("start"));
    assert_eq!(host.vm_mut().current_line_no(), 2);
    assert!(!host.vm_mut().ctx.globals.system.active_flag);
    // Finished is sticky: later frames neither resume the script nor fail.
    for _ in 0..8 {
        assert!(host.step(0).unwrap());
    }
    assert_eq!(host.vm_mut().current_line_no(), 2);
}
