# PS Vita Port Roadmap

Status: planned. The Vita backend has not been implemented, and the full engine does not yet support Vita.

Related request: [Issue #23](https://github.com/xmoezzz/siglus_rs/issues/23).

The goal is to reuse the existing script execution, resource parsing, and game semantics while adding a Vita platform backend. The final output will be a VPK that can be installed and run on a Vita configured for homebrew. The first playable version will cover basic 2D scenes: backgrounds, character sprites, text, choices, audio, and saving and loading. Compatibility will expand as advanced effects, 3D, Emote, and video are validated individually.

## Existing foundations and blockers

| Area | Current code | Porting work |
|---|---|---|
| Scripts and runtime | [vm.rs](../../crates/siglus_scene_vm/src/vm.rs), [runtime](../../crates/siglus_scene_vm/src/runtime/mod.rs) | Reuse instruction semantics; isolate platform services and audit transitive dependencies |
| Frame stepping and input | `step()`, key, and touch interfaces in [host.rs](../../crates/siglus_scene_vm/src/host.rs) | Preserve the driving model; remove the coupling between `SiglusHost` and the concrete `Renderer` |
| Drawing data | `RenderFrame` in [layer.rs](../../crates/siglus_scene_vm/src/layer.rs) | Use shared data across backends; define ordering, blending, clipping, and capture semantics |
| Rendering | [render/mod.rs](../../crates/siglus_scene_vm/src/render/mod.rs) | Currently uses wgpu and winit directly; add a Vita implementation |
| Audio | [audio/kira_hub.rs](../../crates/siglus_scene_vm/src/audio/kira_hub.rs) | Currently uses `AudioManager<DefaultBackend>`; evaluate a custom Kira backend or a separate output adapter |
| Resources and video | [siglus_assets](../../crates/siglus_assets/Cargo.toml), [na_mpeg2_decoder](../../crates/na_mpeg2_decoder/Cargo.toml), [movie](../../crates/siglus_scene_vm/src/movie/mod.rs) | The resource dependency graph includes the MPEG decoder, whose manifest unconditionally includes wgpu and rodio; the movie module also uses Kira types directly |

Avoiding winit in the main program or passing `--no-default-features` is insufficient. The port must address the Cargo dependency graph, module compilation conditions, and runtime service boundaries together.

Rust provides the Tier 3 target `armv7-sony-vita-newlibeabihf`. It requires nightly to build the standard library from source and uses VitaSDK. Dynamic linking is unsupported, so native bindings and final artifacts must use static linking. [Rust target documentation](https://doc.rust-lang.org/rustc/platform-support/armv7-sony-vita-newlibeabihf.html)

## Technical direction

- **Platform entry point:** Add a Vita main loop that calls VitaSDK through Rust FFI for startup, timing, buttons, touch, file paths, and shutdown.
- **Rendering:** Prototype with VitaGL first to validate the required texture, blending, offscreen rendering, and shader capabilities. Evaluate direct GXM use if essential capabilities or performance fall short. VitaGL translates a subset of OpenGL calls into hardware-accelerated GXM calls; this does not establish that the existing wgpu renderer can be reused directly. [VitaGL project documentation](https://github.com/Rinnegatamante/vitaGL)
- **Audio:** Reuse portable decoding and playback semantics, with PCM output through VitaSDK. Validate a custom Kira backend before deciding whether to replace the mixing layer.
- **Build:** Use VitaSDK, Rust nightly, and `cargo-vita` to generate VPKs. After validating the toolchain, pin versions or an image digest and upgrade them through separate changes. [Vita Rust build guide](https://vita-rust.github.io/book/build/index.html)

The names and file locations below are proposed designs. Final boundaries depend on milestone results. All milestones remain unimplemented.

## M0: Establish the toolchain and hardware validation process

Work:

- Add a minimal startup program and build instructions under `platform/vita/`, initially independent of the full engine.
- Validate standard library compilation, static linking, VPK metadata, and packaging.
- Implement screen or file logging, reading a test file, and exiting through a button press. Record the hardware model and runtime environment.
- Identify a hardware tester who can participate throughout the port. Emulators can assist debugging; final runtime acceptance requires physical hardware.

Acceptance: CI generates the startup VPK from a clean environment. A physical Vita can install it, launch it, display logs, read the file, and exit. Without hardware results, this milestone can only be marked as build-validated.

## M1: Isolate platform dependencies in the engine

Work:

- Audit the target dependency graph. Place winit, wgpu, egui, audio device backends, and desktop networking/UI features behind appropriate features or target conditions. Update module compilation and binary entry points accordingly.
- Separate decoding logic from player dependencies in the MPEG decoder crate so resource parsing can compile independently.
- Define a minimal rendering interface around `RenderFrame`, covering drawing, dimensions, texture lifetime, and frame capture. Preserve the semantics required by `FrameCaptureBackend` so transitions and save thumbnails remain functional.
- Adapt interfaces that expose the concrete renderer, including `SiglusHost::new_with_renderer()` and `renderer_mut()`, so desktop and Vita can drive the same VM.
- Introduce replaceable interfaces for audio and other platform services. Implementations without device output are for testing only and must define playback completion and wait behavior.

Acceptance: The VM and resource loading path compile for Vita without a window, and their dependency graph excludes platform backends that have not been adapted. Desktop builds retain their existing rendering and audio implementations, and relevant existing tests and builds pass.

## M2: Implement and select the Vita rendering backend

Work:

- Use the M0 startup program to validate VitaGL screen clearing, texture uploads, alpha blending, clipping, offscreen drawing, and presentation before integrating the engine interface.
- Validate coordinate orientation, color channels, alpha conventions, texture size limits, and resource cleanup.
- Scale the logical game viewport to the device display with letterboxing, and map touch coordinates back into game coordinates.
- Validate required effects with simple shaders. Existing WGSL shaders must be ported or rewritten; they cannot be passed directly to VitaGL.
- Document initialization dependencies, required system modules, and installation prerequisites. If using VitaGL, verify the `libshacccg.suprx` environment required by its documentation.
- Choose VitaGL or GXM based on correctness, frame time, memory use, and implementation cost. Record the decision.

Acceptance: A physical Vita correctly displays a background, translucent character sprites, clipped regions, and offscreen composition. Compare the output with desktop reference images and define acceptable pixel differences. This milestone validates rendering foundations only.

## M3: Run a basic 2D scenario

Work:

- Add a Vita host and engine entry point, connecting `step()`, scene loading, text rendering, and image management.
- Map confirm, cancel, menu, skip, and choice navigation. Support touch controls and deliver matching press and release events.
- Generate glyphs with the existing font rasterization logic. Validate Japanese text, line breaks, punctuation, and font caching.
- Create a minimal test scenario that can be distributed with the repository: a background, character sprites, dialogue, two branches, a basic transition, and a return to the menu.
- Define explicit behavior and diagnostic logs for unsupported instructions or effects. They must not silently leave execution stuck in a wait state.

Acceptance: A physical Vita can launch the scenario, advance dialogue, follow either branch, and return to the menu. Scenario state matches the desktop run for the same input sequence.

## M4: Audio, saving and loading, and application lifecycle

Work:

- Connect BGM, voice, and sound effects, including concurrent mixing, looping, volume, fades, and playback completion notifications.
- Decouple decoding from device output. Use bounded buffers and handle sample rates, thread shutdown, and audio underruns.
- Define paths for game data, settings, saves, and logs. Users supply their own game data.
- Reuse existing save logic and validate write failures, insufficient storage, and recovery after interrupted writes.
- Handle suspend/resume, focus loss, and shutdown. Correct timing and clear stale input state.

Acceptance: The basic scenario plays BGM, voice, and sound effects together. Saving, exiting, and relaunching restores scenario progress. Suspend/resume causes no noticeable timing jumps, stuck inputs, or persistent audio failures.

## M5: Performance and compatibility expansion

Work:

- Measure VM execution, decoding, texture uploads, and drawing on hardware, along with peak heap, texture, and audio buffer usage. Use these measurements to set memory budgets and frame time thresholds.
- Bound image and glyph caches and reduce repeated uploads. Prefer streaming for long audio. Decide whether resource scaling or preprocessing tools are needed based on measurements.
- Adapt blend modes, masks, transitions that use captured frames, 3D, and Emote individually. Add representative scenes and desktop comparison results for each capability.
- Evaluate video decoding, audio/video synchronization, and memory costs separately. Report unsupported video explicitly and validate skip and completion semantics.
- Maintain a game compatibility table with versions, test coverage, known issues, and hardware details. Passing one scene does not establish compatibility with an entire game.

Acceptance: The basic scenario runs continuously for at least 30 minutes without sustained memory growth, crashes, or deadlocks in wait handling. Record frame time distributions and audio underruns. Mark games playable only after validating the capabilities they require.

## M6: Preview builds and release CI

Work:

- Build the actual Vita engine entry point and package its VPK in CI. Upload artifacts, toolchain versions, and necessary logs.
- Include installation steps, data directories, controls, validated features, and known limitations in release notes.
- Complete hardware regression testing before release and record the tested commit. Describe preview readiness through its validated compatibility scope.

Acceptance: Testers can download a VPK from CI, install it using the documentation, and complete the basic scenario acceptance checks. The release package matches the commit tested on hardware.

## CI responsibilities by stage

| Stage | Automated checks | What they establish |
|---|---|---|
| From M0 | Cross-compile portable libraries; build and package the standalone startup program | The toolchain and selected libraries compile, and the startup program can be packaged |
| After M1 | Compile the VM and resource path for Vita; run semantics tests on the build host and desktop regression checks | The core no longer depends on platform backends that have not been adapted |
| After M3 | Build the complete Vita entry point and package the minimal test scenario | Engine artifacts can be generated; runtime correctness still requires device validation |
| M6 | Upload preview VPKs, record versions, and associate hardware acceptance results | A reproducible release process with explicit runtime validation records |

CI can use the [SDK image recommended by Vita Rust](https://vita-rust.github.io/book/build/ci.html). The image is based on Alpine. Run checkout and artifact uploads on an Ubuntu runner, and run cross-compilation inside the container, so Actions that require glibc do not run directly in Alpine.

Initially, list the libraries under validation explicitly. Report successful compilation of a library subset as such, rather than as a successful Vita engine build. Do not use `continue-on-error` to present an engine build that still fails as a passing gate. Report host tests, cross-compilation, and hardware tests separately.

The current [testcase_menu_flow.rs](../../crates/siglus_scene_vm/tests/testcase_menu_flow.rs) uses absolute paths from a developer's machine, and the repository does not include the referenced `testcase` directory. Before using it for automated acceptance, make the paths configurable and provide test assets that can be publicly distributed and reproduced.

## Implementation order and completion criteria

Start with the M0 toolchain and startup program, then complete M1 dependency isolation in small changes. The standalone M2 graphics prototype can begin after M0 passes. Once M1 and M2 are complete, proceed to M3, then M4, followed by compatibility expansion and preview releases. Split PRs along these milestones and record the capability added and its acceptance results in each change.

The first playable version requires hardware acceptance for M0–M4 and the basic stability criteria from M5. Validated preview downloads require M6. Estimate the schedule after assessing hardware testing availability, the rendering prototype, and the dependency audit. This roadmap does not commit to a release date.
